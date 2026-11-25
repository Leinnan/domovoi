package com.crayen.domovoi.system

import android.app.Activity
import com.crayen.domovoi.DomovoiApp
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.app.NotifyOperation
import org.json.JSONObject
import org.unifiedpush.android.connector.FailedReason
import org.unifiedpush.android.connector.PushService
import org.unifiedpush.android.connector.UnifiedPush
import org.unifiedpush.android.connector.data.PushEndpoint
import org.unifiedpush.android.connector.data.PushMessage

/**
 * Push through UnifiedPush (no Google services): when a UnifiedPush
 * distributor (ntfy, NextPush, Sunup, …) is installed, the app registers
 * with it and hands the endpoint and its Web Push keys to the core
 * (`PushEndpoint`), which registers them with each server it connects to
 * (`/api/domovoi/push/webpush`). The server pushes "Needs your approval",
 * "Finished" and "Failed" while the app has no connection open; the
 * connector decrypts the message (RFC 8291) and it is posted like the
 * core's own notices (`Notifier`), approvals with Allow and Decline.
 *
 * Limits: no VAPID key is given to the distributor (one endpoint serves
 * every paired server, each with its own key); no distributor installed
 * means no push, as before (local notices from the live-turn service only);
 * FCM is not used.
 */
class UnifiedPushService : PushService() {
    override fun onNewEndpoint(endpoint: PushEndpoint, instance: String) {
        // Without keys the server could not encrypt for this device.
        val keys = endpoint.pubKeySet ?: return
        DomovoiApp.core(this).update(Event.PushEndpoint(endpoint.url, keys.pubKey, keys.auth))
    }

    override fun onMessage(message: PushMessage, instance: String) {
        if (!message.decrypted) return
        val note = parse(String(message.content, Charsets.UTF_8)) ?: return
        DomovoiApp.core(this).notifyFromPush(note)
    }

    override fun onRegistrationFailed(reason: FailedReason, instance: String) {}

    override fun onUnregistered(instance: String) {
        DomovoiApp.core(this).update(Event.PushEndpoint("", "", ""))
    }

    companion object {
        /** Registers with the distributor in use (or the only one installed); nothing when there is none. */
        fun start(activity: Activity) {
            UnifiedPush.tryUseCurrentOrDefaultDistributor(activity) { ok ->
                if (ok) UnifiedPush.register(activity)
            }
        }

        /** The server's message (`push_web.rs`): `{ environmentId, threadId, title, body, requestId? }`. */
        fun parse(text: String): NotifyOperation? = runCatching {
            val json = JSONObject(text)
            val thread = json.optString("threadId")
            if (thread.isEmpty()) return null
            NotifyOperation(
                title = json.optString("title"),
                body = json.optString("body"),
                threadId = thread,
                environmentId = json.optString("environmentId"),
                requestId = json.optString("requestId").ifEmpty { null },
            )
        }.getOrNull()
    }
}
