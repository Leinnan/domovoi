package com.crayen.domovoi.core

import com.crayen.domovoi.app.Effect
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.app.Request
import com.crayen.domovoi.app.Requests
import com.crayen.domovoi.app.ViewDelta
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import java.util.concurrent.Executors

/** The calls made into the Rust core (`CoreFfi`); tests stand in for it. */
interface CoreBridge {
    fun update(event: ByteArray): ByteArray
    fun resolve(id: UInt, output: ByteArray): ByteArray
    fun view(): ByteArray

    /** Whether the core was replaced (after a panic it caught) since the
     *  last call: `CoreFfi::take_reset`. */
    fun takeReset(): Boolean = false
}

/**
 * The Rust core on a thread of its own. Every call into it (an event, an
 * effect's answer, reading the view) runs there, one at a time and in the
 * order sent, so the main thread never runs or waits for the core: a long
 * thread's frame is megabytes of JSON to parse and a view to build.
 *
 * [handle] and [show] are called on that thread: [handle] with each effect
 * the core asks for, in order; [show] with what changed in the view, read
 * once for a burst of renders (a streaming reply, a batch of replies) and at
 * most every [frameMs].
 *
 * [reset] is called there too when the core was replaced after a panic: what
 * was built from its deltas goes (the next view is the new core's whole one),
 * and the core is to be started again as at launch.
 */
class CoreLoop(
    private val bridge: CoreBridge,
    private val handle: (Request) -> Unit,
    private val show: (ViewDelta) -> Unit,
    private val frameMs: Long = 8,
    private val reset: () -> Unit = {},
) {
    @Volatile private var thread: Thread? = null
    private val executor = Executors.newSingleThreadExecutor { work -> Thread(work, "domovoi-core").also { thread = it } }

    /** Work launched here runs on the core's thread, in the order launched. */
    val scope = CoroutineScope(SupervisorJob() + executor.asCoroutineDispatcher())
    private var viewScheduled = false

    /** An event from any thread: the core takes it after what was sent before. */
    fun send(event: Event) {
        val bytes = event.bincodeSerialize()
        scope.launch { process(checked(bridge.update(bytes))) }
    }

    /** An event from the core's thread, taken now. */
    fun sendNow(event: Event) {
        onThread()
        process(checked(bridge.update(event.bincodeSerialize())))
    }

    /** An effect's answer, on the core's thread. */
    fun resolve(id: UInt, output: ByteArray) {
        onThread()
        process(checked(bridge.resolve(id, output)))
    }

    private fun process(effects: ByteArray) {
        for (request in Requests.bincodeDeserialize(effects).value) {
            if (request.effect is Effect.Render) scheduleView() else handle(request)
        }
    }

    private fun scheduleView() {
        if (viewScheduled) return
        viewScheduled = true
        scope.launch {
            delay(frameMs)
            // Cleared before the view is read: a render asked for while it is
            // being built gets a read of its own.
            viewScheduled = false
            // A panic while building the view: what comes back is the new
            // core's whole view, read after what was built before is gone.
            val bytes = checked(bridge.view())
            show(ViewDelta.bincodeDeserialize(bytes))
        }
    }

    /** [value], after telling [reset] if the call that gave it replaced the core. */
    private fun <T> checked(value: T): T {
        if (bridge.takeReset()) {
            reset()
            scheduleView()
        }
        return value
    }

    private fun onThread() = check(Thread.currentThread() === thread) { "the core is called on its own thread" }

    fun close() = executor.shutdown()
}
