package com.crayen.domovoi.core

import android.Manifest
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.VibrationEffect
import android.os.VibratorManager
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import com.crayen.domovoi.MainActivity
import com.crayen.domovoi.R
import com.crayen.domovoi.DomovoiApp
import com.crayen.domovoi.app.NotifyOperation

/**
 * Posts the core's notifications (a thread needs approval, finished or
 * failed while the user looks elsewhere). An approval carries Allow and
 * Decline, answered without opening the app.
 */
class Notifier(private val context: Context) {
    private val manager = NotificationManagerCompat.from(context)

    init {
        val system = context.getSystemService(NotificationManager::class.java)
        system.createNotificationChannels(
            listOf(
                NotificationChannel(APPROVALS, "Approvals", NotificationManager.IMPORTANCE_HIGH).apply {
                    description = "An agent waits for your approval or answer"
                },
                NotificationChannel(TURNS, "Finished turns", NotificationManager.IMPORTANCE_DEFAULT).apply {
                    description = "An agent finished or failed"
                },
            ),
        )
    }

    val allowed: Boolean
        get() = ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED

    fun post(note: NotifyOperation, foreground: Boolean) {
        if (foreground) haptic(note.body)
        if (!allowed) return
        val id = note.threadId.hashCode()
        val open = PendingIntent.getActivity(
            context, id,
            MainActivity.threadIntent(context, note.threadId, note.environmentId),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val approval = note.body == "Needs your approval"
        val builder = NotificationCompat.Builder(context, if (approval) APPROVALS else TURNS)
            .setSmallIcon(R.drawable.ic_stat_domovoi)
            .setContentTitle(note.title)
            .setContentText(note.body)
            .setContentIntent(open)
            .setAutoCancel(true)
            .setCategory(if (approval) NotificationCompat.CATEGORY_CALL else NotificationCompat.CATEGORY_STATUS)
        note.requestId?.let { request ->
            builder.addAction(0, "Decline", action(note, request, "decline", id))
            builder.addAction(0, "Allow", action(note, request, "accept", id))
        }
        manager.notify(id, builder.build())
    }

    private fun action(note: NotifyOperation, request: String, decision: String, id: Int): PendingIntent {
        val intent = Intent(context, ApprovalReceiver::class.java)
            .putExtra(EXTRA_THREAD, note.threadId)
            .putExtra(EXTRA_ENVIRONMENT, note.environmentId)
            .putExtra(EXTRA_REQUEST, request)
            .putExtra(EXTRA_DECISION, decision)
            .putExtra(EXTRA_NOTIFICATION, id)
        return PendingIntent.getBroadcast(
            context, (request + decision).hashCode(), intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    /** In the foreground, a tap of the phone says the same. */
    private fun haptic(body: String) {
        val vibrator = context.getSystemService(VibratorManager::class.java)?.defaultVibrator ?: return
        val effect = when (body) {
            "Failed" -> VibrationEffect.EFFECT_DOUBLE_CLICK
            "Finished" -> VibrationEffect.EFFECT_TICK
            else -> VibrationEffect.EFFECT_HEAVY_CLICK
        }
        runCatching { vibrator.vibrate(VibrationEffect.createPredefined(effect)) }
    }

    companion object {
        const val APPROVALS = "approvals"
        const val TURNS = "turns"
        const val EXTRA_THREAD = "threadId"
        const val EXTRA_ENVIRONMENT = "environmentId"
        const val EXTRA_REQUEST = "requestId"
        const val EXTRA_DECISION = "decision"
        const val EXTRA_NOTIFICATION = "notificationId"
    }
}

/** Allow or Decline on an approval's notification: the core queues the
 *  answer (reconnecting if it must) and sends it. */
class ApprovalReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val thread = intent.getStringExtra(Notifier.EXTRA_THREAD) ?: return
        val request = intent.getStringExtra(Notifier.EXTRA_REQUEST) ?: return
        val decision = intent.getStringExtra(Notifier.EXTRA_DECISION) ?: return
        val environment = intent.getStringExtra(Notifier.EXTRA_ENVIRONMENT).orEmpty()
        NotificationManagerCompat.from(context).cancel(intent.getIntExtra(Notifier.EXTRA_NOTIFICATION, 0))
        val pending = goAsync()
        DomovoiApp.core(context).respondFromNotification(environment, thread, request, decision) { pending.finish() }
    }
}
