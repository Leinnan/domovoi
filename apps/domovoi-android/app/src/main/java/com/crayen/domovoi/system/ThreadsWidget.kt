package com.crayen.domovoi.system

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.glance.GlanceId
import androidx.glance.GlanceModifier
import androidx.glance.GlanceTheme
import androidx.glance.action.clickable
import androidx.glance.appwidget.GlanceAppWidget
import androidx.glance.appwidget.GlanceAppWidgetReceiver
import androidx.glance.appwidget.action.actionStartActivity
import androidx.glance.appwidget.cornerRadius
import androidx.glance.appwidget.lazy.LazyColumn
import androidx.glance.appwidget.lazy.items
import androidx.glance.appwidget.provideContent
import androidx.glance.background
import androidx.glance.layout.Alignment
import androidx.glance.layout.Box
import androidx.glance.layout.Column
import androidx.glance.layout.Row
import androidx.glance.layout.Spacer
import androidx.glance.layout.fillMaxSize
import androidx.glance.layout.fillMaxWidth
import androidx.glance.layout.height
import androidx.glance.layout.padding
import androidx.glance.layout.size
import androidx.glance.layout.width
import androidx.glance.text.FontWeight
import androidx.glance.text.Text
import androidx.glance.text.TextStyle
import androidx.glance.unit.ColorProvider
import com.crayen.domovoi.MainActivity
import org.json.JSONObject

/** The threads on the home screen: what needs you, what runs, what is new. */
class ThreadsWidget : GlanceAppWidget() {
    private data class Row(val id: String, val title: String, val project: String, val status: String, val unread: Boolean)

    override suspend fun provideGlance(context: Context, id: GlanceId) {
        val (server, attention, rows) = load(context)
        provideContent {
            GlanceTheme {
                Column(
                    GlanceModifier.fillMaxSize().background(GlanceTheme.colors.widgetBackground).cornerRadius(24.dp).padding(12.dp),
                ) {
                    Row(GlanceModifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                        Text(server.ifEmpty { "Domovoi" }, maxLines = 1, style = TextStyle(fontWeight = FontWeight.Bold, fontSize = 14.sp, color = GlanceTheme.colors.onSurface), modifier = GlanceModifier.defaultWeight())
                        // Threads waiting on the user: approvals and questions.
                        if (attention > 0) {
                            Box(GlanceModifier.cornerRadius(10.dp).background(ColorProvider(Color(0xFFE08A1E))).padding(horizontal = 8.dp, vertical = 2.dp)) {
                                Text("$attention need${if (attention == 1) "s" else ""} you", style = TextStyle(fontSize = 11.sp, fontWeight = FontWeight.Medium, color = ColorProvider(Color.White)))
                            }
                        }
                    }
                    Spacer(GlanceModifier.height(8.dp))
                    if (rows.isEmpty()) {
                        Text("Open Domovoi to see your threads.", style = TextStyle(fontSize = 12.sp, color = GlanceTheme.colors.onSurfaceVariant))
                    } else {
                        LazyColumn {
                            items(rows, itemId = { it.id.hashCode().toLong() }) { row -> ThreadRow(context, row) }
                        }
                    }
                }
            }
        }
    }

    @Composable
    private fun ThreadRow(context: Context, row: Row) {
        val open = Intent(Intent.ACTION_VIEW, Uri.parse("domovoi://thread?id=${Uri.encode(row.id)}"), context, MainActivity::class.java)
        Row(
            GlanceModifier.fillMaxWidth().padding(vertical = 6.dp).clickable(actionStartActivity(open)),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            val dot = when {
                row.status == "approval" -> ColorProvider(Color(0xFFE08A1E))
                row.status == "working" -> GlanceTheme.colors.primary
                row.status == "failed" -> GlanceTheme.colors.error
                row.unread -> GlanceTheme.colors.primary
                else -> ColorProvider(Color.Transparent)
            }
            Box(GlanceModifier.size(8.dp).cornerRadius(4.dp).background(dot)) {}
            Spacer(GlanceModifier.width(8.dp))
            Column {
                Text(row.title, maxLines = 1, style = TextStyle(fontSize = 13.sp, color = GlanceTheme.colors.onSurface, fontWeight = if (row.unread) FontWeight.Bold else FontWeight.Normal))
                Text(
                    listOf(row.project, when (row.status) { "approval" -> "Needs you"; "working" -> "Working"; "failed" -> "Failed"; else -> "" }).filter { it.isNotEmpty() }.joinToString(" · "),
                    maxLines = 1, style = TextStyle(fontSize = 11.sp, color = GlanceTheme.colors.onSurfaceVariant),
                )
            }
        }
    }

    private fun load(context: Context): Triple<String, Int, List<Row>> = runCatching {
        val o = JSONObject(SystemSurfaces.snapshotFile(context).readText())
        val threads = o.getJSONArray("threads")
        Triple(o.optString("server"), o.optInt("attention"), (0 until threads.length()).map { i ->
            val t = threads.getJSONObject(i)
            Row(t.getString("id"), t.optString("title"), t.optString("project"), t.optString("status"), t.optBoolean("unread"))
        })
    }.getOrDefault(Triple("", 0, emptyList()))
}

class ThreadsWidgetReceiver : GlanceAppWidgetReceiver() {
    override val glanceAppWidget: GlanceAppWidget = ThreadsWidget()
}
