package com.crayen.domovoi.core

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.Build
import android.provider.Settings
import android.util.Log
import com.crayen.domovoi.BuildConfig
import com.crayen.domovoi.app.Effect
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.app.HttpRequest
import com.crayen.domovoi.app.LocalOperation
import com.crayen.domovoi.app.LocalOutput
import com.crayen.domovoi.app.NotifyOutput
import com.crayen.domovoi.app.Request
import com.crayen.domovoi.app.SocketEvent
import com.crayen.domovoi.app.SocketOperation
import com.crayen.domovoi.app.StoreOperation
import com.crayen.domovoi.app.StoreOutput
import com.crayen.domovoi.app.TunnelOperation
import com.crayen.domovoi.app.TunnelOutput
import com.crayen.domovoi.app.ViewDelta
import com.crayen.domovoi.ffi.CoreFfi
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.time.ZoneId
import java.time.ZonedDateTime

/**
 * Bridges the Rust core: forwards events, runs effects (HTTP, WebSocket, the
 * iroh tunnel, secure storage, notifications) and publishes the view model.
 * The core makes every decision; this class only performs I/O and the
 * timers the core asks for.
 *
 * The core runs on a thread of its own ([CoreLoop]), and so does everything
 * here that follows from it: effects are carried out, their answers given
 * and this class's state kept on that thread. Nothing of the core runs on the
 * main thread. What the app calls from the main thread ([update], the
 * lifecycle calls) is handed over and taken in order; `StateFlow` carries
 * the view back.
 */
class Core(
    private val context: Context,
    private val store: KeyValueStore = SecureStore(context),
) {
    private val ffi = CoreFfi()
    private val loop = CoreLoop(
        object : CoreBridge {
            override fun update(event: ByteArray) = ffi.update(event)
            override fun resolve(id: UInt, output: ByteArray) = ffi.resolve(id, output)
            override fun view() = ffi.view()
            override fun takeReset() = ffi.takeReset()
        },
        handle = ::handle,
        show = ::show,
        reset = ::coreReplaced,
    )
    /** The core's thread: everything launched here runs there, in order. */
    private val scope = loop.scope
    private val http = Http()
    private val socket = Socket(scope, ::emitSocket)
    private val tunnel = Tunnel()
    private val notifier = Notifier(context)

    /** A notice the server pushed (UnifiedPush) while the app had no connection. */
    fun notifyFromPush(note: com.crayen.domovoi.app.NotifyOperation) = notifier.post(note, foreground = false, page = null)

    /** From the core's first view, which carries every part. */
    val view = LiveView(ViewDelta.bincodeDeserialize(ffi.view()))

    /** When the scheduled retry happens (Unix ms); the banner counts down to it. */
    private val _retryAt = MutableStateFlow<Long?>(null)
    val retryAt: StateFlow<Long?> = _retryAt.asStateFlow()

    /** The app is on screen (resumed). */
    @Volatile var foreground = false
        private set

    private var launched = false
    private var reconnectJob: Job? = null
    private var reconnectGen: ULong = 0u
    private var tunnelPoll: Job? = null
    private var tunnelRequest: UInt? = null
    private var graceJob: Job? = null
    private var backgroundAt: Long? = null
    private var suspended = false

    init {
        // Every few seconds: unanswered sends time out on it. The core only
        // renders when something changed (or every 20 s, for relative times).
        scope.launch {
            while (isActive) {
                tick()
                delay(5_000)
            }
        }
        NetworkWatcher(context, online = { update(Event.NetworkStatus(it)) }, changed = { update(Event.NetworkChanged) }).start()
        // Usage's "Today" and hours follow the device's zone, and move with it.
        val zoneChanges = IntentFilter().apply {
            addAction(Intent.ACTION_TIMEZONE_CHANGED)
            addAction(Intent.ACTION_TIME_CHANGED)
        }
        val zoneReceiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) = onCore { now(timeZone()) }
        }
        androidx.core.content.ContextCompat.registerReceiver(context, zoneReceiver, zoneChanges, androidx.core.content.ContextCompat.RECEIVER_NOT_EXPORTED)
    }

    /** The device's zone and its offset now (which daylight saving moves). */
    private fun timeZone(): Event {
        val zone = ZoneId.systemDefault()
        return Event.SetTimeZone(zone.id, ZonedDateTime.now(zone).offset.totalSeconds)
    }

    /** Restores saved environments and reconnects to the last one (once per process). */
    fun launch() = onCore {
        if (launched) return@onCore
        launched = true
        val name = Settings.Global.getString(context.contentResolver, Settings.Global.DEVICE_NAME)
        // Before Launch: the first Usage load already has the local day.
        now(timeZone())
        now(Event.Launch("mobile", name ?: Build.MODEL, "Android"))
    }

    /** Hands the event to the core, from any thread. It is taken in order;
     *  what it changes shows in [view] a moment later. */
    fun update(event: Event) = loop.send(event)

    /** Runs `block` on the core's thread, after what was handed over before. */
    private fun onCore(block: () -> Unit) {
        scope.launch { block() }
    }

    /** An event from the core's thread, taken now. */
    private fun now(event: Event) = loop.sendNow(event)

    private fun tick() = now(Event.Tick(System.currentTimeMillis()))

    private fun resolve(id: UInt, bytes: ByteArray) = loop.resolve(id, bytes)

    /** Carries out an effect the core asked for (on the core's thread). */
    private fun handle(request: Request) {
        when (val effect = request.effect) {
            // Read by the loop, once for a burst.
            is Effect.Render -> Unit
            is Effect.Http -> perform(request.id, effect.value)
            is Effect.Socket -> when (val op = effect.value) {
                is SocketOperation.Open -> socket.open(op.url, request.id)
                is SocketOperation.Send -> {
                    if (TRACE) Log.d(TAG, ">> ${op.text.take(300)}")
                    socket.send(op.text)
                }
                SocketOperation.Close -> socket.close()
            }
            is Effect.Notify -> {
                notifier.post(effect.value, foreground, view.thread.value)
                resolve(request.id, NotifyOutput.POSTED.bincodeSerialize())
            }
            is Effect.Tunnel -> runTunnel(effect.value, request.id)
            is Effect.Store -> {
                val value = when (val op = effect.value) {
                    is StoreOperation.Load -> store.load(op.key)
                    is StoreOperation.Save -> { store.save(op.key, op.value); null }
                    is StoreOperation.Delete -> { store.delete(op.key); null }
                }
                resolve(request.id, StoreOutput.Value(value).bincodeSerialize())
            }
            is Effect.Local -> resolve(request.id, localAnswer(effect.value).bincodeSerialize())
        }
    }

    /** The core was replaced after a panic (on the core's thread): the
     *  thread page built from the old one's deltas goes, and the new core
     *  starts as at launch, reconnecting to the saved server. */
    private fun coreReplaced() {
        view.reset()
        launched = false
        launch()
        update(Event.AppFocus(foreground))
    }

    /** What changed in the view (on the core's thread). */
    private fun show(delta: ViewDelta) {
        view.apply(delta)
        if (delta.thread != null) notifier.pageShown(view.thread.value)
        scheduleReconnectIfNeeded()
    }

    // ---- HTTP and socket ----

    private fun perform(id: UInt, request: HttpRequest) {
        scope.launch {
            val result = http.perform(request)
            resolve(id, result.bincodeSerialize())
        }
    }

    private fun emitSocket(event: SocketEvent, requestId: UInt) {
        if (TRACE && event is SocketEvent.Message) Log.d(TAG, "<< ${event.text.take(1000)}")
        resolve(requestId, event.bincodeSerialize())
    }

    // ---- reconnect ----

    /** Times the retry the core scheduled. Each retry has its own number: a
     *  new one replaces the timer, and the core ignores a timer for an older one. */
    private fun scheduleReconnectIfNeeded() {
        val connection = view.connection.value
        val delaySeconds = connection.reconnectInSeconds.toLong()
        if (connection.state != "reconnecting" || delaySeconds == 0L || connection.offline) {
            reconnectJob?.cancel()
            reconnectJob = null
            _retryAt.value = null
            return
        }
        if (reconnectJob != null && reconnectGen == connection.retryGen) return
        reconnectJob?.cancel()
        val gen = connection.retryGen
        reconnectGen = gen
        _retryAt.value = System.currentTimeMillis() + delaySeconds * 1000
        reconnectJob = scope.launch {
            delay(delaySeconds * 1000)
            reconnectJob = null
            now(Event.RetryTimer(gen))
        }
    }

    // ---- lifecycle ----

    fun setFocused(focused: Boolean) {
        foreground = focused
        update(Event.AppFocus(focused))
    }

    // The lifecycle calls come from the main thread; their state is the
    // core thread's, so each is handed over like an event.

    /** Leaving the app for a moment keeps the connection: the socket stays
     *  open for a grace period and is closed only when that runs out. */
    fun enterBackground() = onCore {
        if (backgroundAt != null) return@onCore
        backgroundAt = System.currentTimeMillis()
        if (!keepAlive) startGrace()
    }

    private fun startGrace() {
        graceJob?.cancel()
        graceJob = scope.launch {
            delay(GRACE_MS)
            suspendNow()
        }
    }

    /** A turn runs with the app out of sight (`LiveTurnService`): the
     *  connection stays; when it ends, the usual grace applies. */
    private var keepAlive = false

    fun setKeepAlive(on: Boolean) = onCore {
        keepAlive = on
        if (on) {
            graceJob?.cancel()
            graceJob = null
        } else if (backgroundAt != null && !suspended) {
            backgroundAt = System.currentTimeMillis()
            startGrace()
        }
    }

    fun enterForeground() = onCore {
        // The offset may have moved (daylight saving) while away.
        now(timeZone())
        val since = backgroundAt ?: return@onCore
        backgroundAt = null
        graceJob?.cancel()
        graceJob = null
        // A frozen process misses its timer: the grace ran out all the same.
        if (!suspended && !keepAlive && System.currentTimeMillis() - since >= GRACE_MS) suspendNow()
        if (!suspended) {
            // Back within the grace period: the connection never stopped.
            now(Event.NetworkChanged)
            return@onCore
        }
        suspended = false
        scope.launch {
            // Check the tunnel's port before the core reuses it.
            val request = tunnelRequest
            if (request != null) tunnel.status()?.let { if (tunnelRequest == request) emitTunnel(it, request) }
            now(Event.Resume)
        }
    }

    private fun suspendNow() {
        if (suspended) return
        suspended = true
        // Taken at once: the drafts are saved and the socket closed before
        // this returns, should the process be frozen next.
        now(Event.Suspend)
    }

    /** Allow or Decline from a notification; `done` runs once the answer had
     *  time to go out (the receiver keeps the process up until then). */
    fun respondFromNotification(environment: String, thread: String, request: String, decision: String, done: () -> Unit) =
        answerInBackground(Event.RespondFromNotification(environment, thread, request, decision), done)

    /** Allow or Decline from the turn notification, which knows the thread
     *  but not the request: the core answers the approval the thread waits on. */
    fun respondToThread(environment: String, thread: String, decision: String, done: () -> Unit) =
        answerInBackground(Event.RespondToThread(environment, thread, decision), done)

    /** A question's answer typed in its notification. */
    fun answerFromNotification(environment: String, thread: String, request: String, question: String, text: String, done: () -> Unit) =
        answerInBackground(Event.AnswerFromNotification(environment, thread, request, question, text), done)

    private fun answerInBackground(event: Event, done: () -> Unit) {
        update(event)
        scope.launch {
            delay(8_000)
            withContext(Dispatchers.Main) { done() }
        }
    }

    fun openThread(thread: String, environment: String) {
        // The core holds a tap that arrives before saved servers are loaded.
        update(Event.OpenNotification(environment, thread))
        com.crayen.domovoi.system.AppRequests.post(com.crayen.domovoi.system.AppRequest.OpenThread(thread))
    }

    // ---- tunnel ----

    private fun runTunnel(op: TunnelOperation, requestId: UInt) {
        when (op) {
            is TunnelOperation.Start -> {
                tunnelPoll?.cancel()
                tunnelPoll = null
                tunnelRequest = requestId
                val key = (store as? SecureStore)?.deviceKey() ?: deviceKeyFallback()
                if (key == null) {
                    emitTunnel(TunnelOutput.Failed("This device's key could not be read. Unlock the device and try again."), requestId)
                    return
                }
                scope.launch {
                    val result = tunnel.start(key, op.ticket)
                    // Stopped or replaced while starting: nothing to report.
                    if (tunnelRequest != requestId) return@launch
                    result.fold(
                        onSuccess = { port ->
                            emitTunnel(TunnelOutput.Started(port), requestId)
                            pollTunnel(requestId)
                        },
                        onFailure = { emitTunnel(TunnelOutput.Failed(it.message ?: "The tunnel failed to start."), requestId) },
                    )
                }
            }
            TunnelOperation.NetworkChanged -> scope.launch { tunnel.networkChanged() }
            TunnelOperation.Stop -> {
                tunnelPoll?.cancel()
                tunnelPoll = null
                tunnelRequest = null
                scope.launch { tunnel.stop() }
            }
        }
    }

    private fun deviceKeyFallback(): String? =
        store.load("deviceKey") ?: runCatching { com.crayen.domovoi.ffi.tunnelGenerateKey() }.getOrNull()?.also { store.save("deviceKey", it) }

    private fun emitTunnel(output: TunnelOutput, requestId: UInt) = resolve(requestId, output.bincodeSerialize())

    /** Streams the bridge's path and round-trip time while it runs. */
    private fun pollTunnel(requestId: UInt) {
        tunnelPoll?.cancel()
        tunnelPoll = scope.launch {
            while (isActive && tunnelRequest == requestId) {
                val status = tunnel.status()
                if (!isActive || tunnelRequest != requestId) return@launch
                status?.let { emitTunnel(it, requestId) }
                delay(3_000)
            }
        }
    }

    private companion object {
        const val TAG = "DomovoiCore"
        const val GRACE_MS = 25_000L
        val TRACE = BuildConfig.DEBUG && Log.isLoggable("DomovoiTrace", Log.DEBUG)
    }
}

/**
 * The answer to the core's search for a server on the same computer. A phone
 * has none: there is no descriptor file to watch and nowhere to write a
 * sign-in request, and the core does not ask on the mobile surface.
 */
internal fun localAnswer(operation: LocalOperation): LocalOutput = when (operation) {
    LocalOperation.WATCH -> LocalOutput.Descriptor(null)
    LocalOperation.REQUEST -> LocalOutput.Unavailable
}
