package com.crayen.domovoi.system

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import com.crayen.domovoi.MainActivity
import com.crayen.domovoi.R
import com.crayen.domovoi.DomovoiApp
import com.crayen.domovoi.app.ThreadRowView
import com.crayen.domovoi.core.ApprovalReceiver
import com.crayen.domovoi.core.Notifier
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch

/**
 * Keeps the connection while an agent works and the app is out of sight
 * (Android's Live Activity counterpart): an ongoing notification shows the
 * turns under way, how long they have run and which need the user; the core
 * keeps its socket, so approvals and finished turns still notify. One
 * thread waiting on an approval gets Allow and Decline here too (iOS's Live
 * Activity buttons). It ends itself when no turn runs.
 */
class LiveTurnService : Service() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private var startedAt = HashMap<String, Long>()

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        channel(this)
        val core = DomovoiApp.core(this)
        startInForeground(build(emptyList()))
        core.setKeepAlive(true)
        // The server in use (what `RespondToThread` checks the answer against).
        val server = core.view.connection.map { c -> c.environments.firstOrNull { it.active }?.serverId.orEmpty() }
        scope.launch {
            combine(core.view.sidebar.map { s -> live(s.surfaceThreads) }, server) { threads, id -> threads to id }
                .distinctUntilChanged()
                .collect { (threads, serverId) ->
                    if (threads.isEmpty()) {
                        stopSelf()
                        return@collect
                    }
                    val now = System.currentTimeMillis()
                    threads.forEach { startedAt.getOrPut(it.id) { now } }
                    startedAt.keys.retainAll(threads.map { it.id }.toSet())
                    getSystemService(NotificationManager::class.java).notify(ID, build(threads, serverId))
                }
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int = START_NOT_STICKY

    override fun onDestroy() {
        DomovoiApp.core(this).setKeepAlive(false)
        scope.cancel()
        super.onDestroy()
    }

    override fun onTimeout(startId: Int, fgsType: Int) {
        stopSelf()
    }

    private fun startInForeground(notification: Notification) {
        val type = if (Build.VERSION.SDK_INT >= 34) ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE else 0
        ServiceCompat.startForeground(this, ID, notification, type)
    }

    private fun build(threads: List<ThreadRowView>, serverId: String = ""): Notification {
        val waiting = threads.filter { it.status == "approval" }
        val first = waiting.firstOrNull() ?: threads.firstOrNull()
        val title = when {
            threads.isEmpty() -> "Domovoi is connected"
            threads.size == 1 -> first!!.title
            else -> "${threads.size} agents working"
        }
        val text = when {
            waiting.size == 1 && threads.size == 1 -> "Needs your approval"
            waiting.isNotEmpty() -> "${waiting.size} need${if (waiting.size == 1) "s" else ""} you · ${waiting.joinToString { it.title }}"
            threads.size > 1 -> threads.joinToString { it.title }
            else -> "Working"
        }
        val open = PendingIntent.getActivity(
            this, 0,
            first?.let { MainActivity.threadIntent(this, it.id, "") } ?: Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val since = threads.mapNotNull { startedAt[it.id] }.minOrNull() ?: System.currentTimeMillis()
        // One approval waiting (a question needs the app): answered from here.
        val approving = waiting.singleOrNull()?.takeIf { !it.question }
        return NotificationCompat.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.ic_stat_domovoi)
            .setContentTitle(title)
            .setContentText(text)
            .setContentIntent(open)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setSilent(true)
            .setWhen(since)
            .setUsesChronometer(threads.isNotEmpty())
            .setShowWhen(threads.isNotEmpty())
            .setCategory(NotificationCompat.CATEGORY_PROGRESS)
            .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
            // Android 16: a Live Update chip in the status bar.
            .setRequestPromotedOngoing(true)
            .setShortCriticalText(if (waiting.isNotEmpty()) "Approve" else "Working")
            .setProgress(0, 0, waiting.isEmpty() && threads.isNotEmpty())
            .apply {
                if (approving != null) {
                    addAction(0, "Decline", respond(approving.id, serverId, "decline"))
                    addAction(0, "Allow", respond(approving.id, serverId, "accept"))
                }
            }
            .build()
    }

    private fun respond(thread: String, serverId: String, decision: String): PendingIntent {
        val intent = Intent(this, ApprovalReceiver::class.java)
            .putExtra(Notifier.EXTRA_THREAD, thread)
            .putExtra(Notifier.EXTRA_ENVIRONMENT, serverId)
            .putExtra(Notifier.EXTRA_DECISION, decision)
            .putExtra(Notifier.EXTRA_WHOLE_THREAD, true)
        return PendingIntent.getBroadcast(
            this, ("live" + thread + decision).hashCode(), intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    companion object {
        private const val ID = 7
        private const val CHANNEL = "live"

        fun live(threads: List<ThreadRowView>) = threads.filter { it.status == "working" || it.status == "approval" }

        private fun channel(context: Context) {
            context.getSystemService(NotificationManager::class.java).createNotificationChannel(
                NotificationChannel(CHANNEL, "Turns under way", NotificationManager.IMPORTANCE_LOW).apply {
                    description = "While an agent works with Domovoi in the background"
                    setShowBadge(false)
                },
            )
        }

        /** Starts the service (only allowed while the app is in front). */
        fun start(context: Context) {
            runCatching { ContextCompat.startForegroundService(context, Intent(context, LiveTurnService::class.java)) }
        }
    }
}
