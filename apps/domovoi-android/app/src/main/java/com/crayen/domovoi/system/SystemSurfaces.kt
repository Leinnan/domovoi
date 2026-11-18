package com.crayen.domovoi.system

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.core.content.pm.ShortcutInfoCompat
import androidx.core.content.pm.ShortcutManagerCompat
import androidx.core.graphics.drawable.IconCompat
import androidx.glance.appwidget.updateAll
import com.crayen.domovoi.MainActivity
import com.crayen.domovoi.R
import com.crayen.domovoi.app.SidebarView
import com.crayen.domovoi.core.Core
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject
import java.io.File

/**
 * Keeps what Android shows outside the app in step with the thread list
 * (`SystemSurfaces.swift`): the turn service, the widget's snapshot and the
 * launcher shortcuts.
 */
class SystemSurfaces(private val context: Context, private val core: Core) {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private var lastSnapshot: String? = null

    fun start() {
        scope.launch {
            combine(core.view.sidebar, core.view.connection) { s, c -> s to c.environmentLabel }
                .distinctUntilChanged()
                .collect { (sidebar, server) ->
                    // A turn under way while the app is in front: the service keeps it going if the user leaves.
                    if (core.foreground && LiveTurnService.live(sidebar.surfaceThreads).isNotEmpty()) LiveTurnService.start(context)
                    writeSnapshot(sidebar, server)
                }
        }
    }

    /** The app went to the background: the launcher's shortcuts follow the threads most worth a glance. */
    fun refreshShortcuts() {
        val sidebar = core.view.sidebar.value
        val recent = sidebar.surfaceThreads.take(3)
        val shortcuts = mutableListOf(
            ShortcutInfoCompat.Builder(context, "new")
                .setShortLabel("New thread")
                .setIcon(IconCompat.createWithResource(context, R.drawable.ic_shortcut_new))
                .setIntent(Intent(Intent.ACTION_VIEW, Uri.parse("domovoi://new"), context, MainActivity::class.java))
                .build(),
        )
        recent.forEachIndexed { i, t ->
            shortcuts += ShortcutInfoCompat.Builder(context, "thread-${t.id}")
                .setShortLabel(t.title.take(24).ifEmpty { "Thread" })
                .setLongLabel(t.title.take(60).ifEmpty { "Thread" })
                .setRank(i + 1)
                .setIcon(IconCompat.createWithResource(context, R.drawable.ic_shortcut_thread))
                .setIntent(MainActivity.threadIntent(context, t.id, ""))
                .build()
        }
        runCatching { ShortcutManagerCompat.setDynamicShortcuts(context, shortcuts) }
    }

    private suspend fun writeSnapshot(sidebar: SidebarView, server: String) {
        val threads = JSONArray()
        sidebar.surfaceThreads.take(8).forEach { t ->
            threads.put(JSONObject().put("id", t.id).put("title", t.title).put("project", t.projectTitle).put("status", t.status).put("unread", t.unread))
        }
        val json = JSONObject().put("server", server).put("threads", threads).toString()
        if (json == lastSnapshot) return
        lastSnapshot = json
        withContext(Dispatchers.IO) { snapshotFile(context).writeText(json) }
        runCatching { ThreadsWidget().updateAll(context) }
    }

    companion object {
        fun snapshotFile(context: Context) = File(context.filesDir, "widget-snapshot.json")
    }
}
