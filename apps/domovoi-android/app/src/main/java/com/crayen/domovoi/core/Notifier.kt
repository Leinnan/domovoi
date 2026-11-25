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
import androidx.core.app.RemoteInput
import androidx.core.content.ContextCompat
import com.crayen.domovoi.MainActivity
import com.crayen.domovoi.R
import com.crayen.domovoi.DomovoiApp
import com.crayen.domovoi.app.NotifyOperation
import com.crayen.domovoi.app.QuestionView
import com.crayen.domovoi.app.ThreadPageView

/**
 * Posts the core's notifications (a thread needs approval, finished or
 * failed while the user looks elsewhere). An approval carries Allow and
 * Decline, answered without opening the app; a question with one typed
 * answer takes it in the notification, once the thread's page shows the
 * question (the core's notice doesn't carry it).
 */
class Notifier(private val context: Context) {
    private val manager = NotificationManagerCompat.from(context)

    /** Question notifications posted without an answer field, by thread. */
    private val unanswerable = HashMap<String, NotifyOperation>()

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

    fun post(note: NotifyOperation, foreground: Boolean, page: ThreadPageView?) {
        if (foreground) haptic(note.body)
        if (!allowed) return
        build(note, page?.takeIf { it.id == note.threadId }?.let(::answerable))
    }

    /** The thread page changed: a question notified before its page showed
     *  it gets its answer field now (quietly), if it is still up. */
    fun pageShown(page: ThreadPageView?) {
        if (page == null || unanswerable.isEmpty()) return
        val note = unanswerable[page.id] ?: return
        val question = answerable(page) ?: return
        val up = context.getSystemService(NotificationManager::class.java).activeNotifications.any { it.id == note.threadId.hashCode() }
        if (!up) { unanswerable.remove(page.id); return }
        build(note, question, quiet = true)
    }

    private fun build(note: NotifyOperation, question: Pair<String, QuestionView>?, quiet: Boolean = false) {
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
            .setOnlyAlertOnce(quiet)
        note.requestId?.let { request ->
            builder.addAction(0, "Decline", action(note, request, "decline", id))
            builder.addAction(0, "Allow", action(note, request, "accept", id))
        }
        unanswerable.remove(note.threadId)
        if (approval && note.requestId == null) {
            if (question != null) {
                val (request, q) = question
                builder.setContentText(q.question.ifEmpty { note.body })
                    .setStyle(NotificationCompat.BigTextStyle().bigText(q.question.ifEmpty { note.body }))
                    .addAction(answerAction(note, request, q, id))
            } else {
                unanswerable[note.threadId] = note
            }
        }
        manager.notify(id, builder.build())
    }

    /** "Answer": typed in the notification, sent as `AnswerFromNotification`. */
    private fun answerAction(note: NotifyOperation, request: String, question: QuestionView, id: Int): NotificationCompat.Action {
        val intent = Intent(context, ApprovalReceiver::class.java)
            .putExtra(EXTRA_THREAD, note.threadId)
            .putExtra(EXTRA_ENVIRONMENT, note.environmentId)
            .putExtra(EXTRA_REQUEST, request)
            .putExtra(EXTRA_QUESTION, question.id)
            .putExtra(EXTRA_NOTIFICATION, id)
        // Mutable: the system adds the typed answer to it.
        val pending = PendingIntent.getBroadcast(
            context, (request + question.id).hashCode(), intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_MUTABLE,
        )
        val input = RemoteInput.Builder(KEY_ANSWER).setLabel(question.header.ifEmpty { "Your answer" }).build()
        return NotificationCompat.Action.Builder(0, "Answer", pending)
            .addRemoteInput(input)
            .setSemanticAction(NotificationCompat.Action.SEMANTIC_ACTION_REPLY)
            .setAllowGeneratedReplies(false)
            .build()
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
        /**
         * The pending question a notification can answer: a `user_input`
         * request with one question that takes free text (as the server's
         * own pushes offer), with its request id.
         */
        fun answerable(page: ThreadPageView): Pair<String, QuestionView>? {
            val request = page.pendingRequests.firstOrNull { it.kind == "user_input" } ?: return null
            val question = request.questions.singleOrNull() ?: return null
            if (question.multiSelect || (question.options.isNotEmpty() && !question.allowCustomAnswer)) return null
            return request.id to question
        }

        const val APPROVALS = "approvals"
        const val TURNS = "turns"
        const val EXTRA_THREAD = "threadId"
        const val EXTRA_ENVIRONMENT = "environmentId"
        const val EXTRA_REQUEST = "requestId"
        const val EXTRA_DECISION = "decision"
        const val EXTRA_NOTIFICATION = "notificationId"
        const val EXTRA_QUESTION = "questionId"
        /** The turn notification answers the thread's approval, not a request. */
        const val EXTRA_WHOLE_THREAD = "wholeThread"
        const val KEY_ANSWER = "answer"
    }
}

/** Allow or Decline on an approval's (or the turn's) notification, or a
 *  question's typed answer: the core queues the answer (reconnecting if it
 *  must) and sends it. */
class ApprovalReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val thread = intent.getStringExtra(Notifier.EXTRA_THREAD) ?: return
        val environment = intent.getStringExtra(Notifier.EXTRA_ENVIRONMENT).orEmpty()
        val core = DomovoiApp.core(context)
        val request = intent.getStringExtra(Notifier.EXTRA_REQUEST)
        val question = intent.getStringExtra(Notifier.EXTRA_QUESTION)
        val decision = intent.getStringExtra(Notifier.EXTRA_DECISION)
        val answer = RemoteInput.getResultsFromIntent(intent)?.getCharSequence(Notifier.KEY_ANSWER)?.toString()?.trim()
        val pending = goAsync()
        val done = { pending.finish() }
        when {
            request != null && question != null && !answer.isNullOrEmpty() -> core.answerFromNotification(environment, thread, request, question, answer, done)
            decision != null && intent.getBooleanExtra(Notifier.EXTRA_WHOLE_THREAD, false) -> core.respondToThread(environment, thread, decision, done)
            request != null && decision != null -> core.respondFromNotification(environment, thread, request, decision, done)
            else -> { pending.finish(); return }
        }
        // The turn notification stays: it follows the thread on its own.
        if (!intent.getBooleanExtra(Notifier.EXTRA_WHOLE_THREAD, false)) {
            NotificationManagerCompat.from(context).cancel(intent.getIntExtra(Notifier.EXTRA_NOTIFICATION, 0))
        }
    }
}
