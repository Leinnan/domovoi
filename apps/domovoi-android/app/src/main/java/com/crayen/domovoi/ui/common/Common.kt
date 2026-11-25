package com.crayen.domovoi.ui.common

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Error
import androidx.compose.material.icons.filled.PanTool
import androidx.compose.material.icons.outlined.StopCircle
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.State
import androidx.compose.runtime.getValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.core.Core
import com.crayen.domovoi.ui.theme.LocalTints
import com.crayen.domovoi.ui.theme.Space
import kotlinx.coroutines.flow.StateFlow

/** The core, for composables deep in rows that are not handed it
 *  (`DiagramPreview` asks it for the server's drawing). */
val LocalCore = staticCompositionLocalOf<Core?> { null }

/** Collects one slice of the core's view model. */
@Composable
fun <T> StateFlow<T>.watch(): State<T> = collectAsStateWithLifecycle()

/** A thread's state in a row: spinner, raised hand, error, stop, or an unread dot. */
@Composable
fun StatusIcon(status: String, unread: Boolean = false, modifier: Modifier = Modifier) {
    val label = when (status) {
        "working" -> "Working"
        "approval" -> "Needs your approval"
        "failed" -> "Failed"
        "interrupted" -> "Stopped"
        else -> if (unread) "Unread" else ""
    }
    Box(modifier.size(18.dp).semantics { if (label.isNotEmpty()) contentDescription = label }, contentAlignment = Alignment.Center) {
        when (status) {
            "working" -> CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 2.dp)
            "approval" -> Icon(Icons.Filled.PanTool, null, tint = LocalTints.current.caution, modifier = Modifier.size(16.dp))
            "failed" -> Icon(Icons.Filled.Error, null, tint = MaterialTheme.colorScheme.error, modifier = Modifier.size(16.dp))
            "interrupted" -> Icon(Icons.Outlined.StopCircle, null, tint = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.size(16.dp))
            else -> if (unread) Box(Modifier.size(8.dp).background(MaterialTheme.colorScheme.primary, CircleShape))
        }
    }
}

/** Which agent a thread came from (Claude Code, Codex, Pi, …): the core's label, or a driver id. */
@Composable
fun SourceBadge(source: String) {
    if (source.isEmpty() || source == "domovoi") return
    val label = when (source) {
        "Claude Code", "claude", "claudeAgent", "claude-code" -> "Claude"
        "Codex", "codex" -> "Codex"
        "Pi", "pi" -> "Pi"
        "Antigravity", "antigravity" -> "Antigravity"
        else -> source.replaceFirstChar { it.uppercase() }
    }
    val color = agentColor(source) ?: MaterialTheme.colorScheme.onSurfaceVariant
    Surface(color = color.copy(alpha = 0.14f), shape = RoundedCornerShape(6.dp)) {
        Text(label, color = color, style = MaterialTheme.typography.labelSmall, modifier = Modifier.padding(horizontal = 6.dp, vertical = 1.dp))
    }
}

/** An agent's own colour (Claude, Codex, Pi, Antigravity), by its label or
 *  provider key; null for the others. */
@Composable
fun agentColor(source: String): androidx.compose.ui.graphics.Color? {
    val tints = LocalTints.current
    return when (source) {
        "Claude Code", "Claude", "claude", "claudeAgent", "claude-code" -> tints.claude
        "Codex", "codex" -> tints.codex
        "Pi", "pi" -> tints.pi
        "Antigravity", "antigravity" -> tints.antigravity
        else -> null
    }
}

/** A section title in a list. */
@Composable
fun SectionHeader(text: String, modifier: Modifier = Modifier, icon: ImageVector? = null, tint: androidx.compose.ui.graphics.Color? = null, leading: @Composable (() -> Unit)? = null, trailing: @Composable (() -> Unit)? = null) {
    androidx.compose.foundation.layout.Row(
        modifier.fillMaxWidth().padding(start = Space.l, end = Space.xs, top = Space.l, bottom = Space.xs),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        val color = tint ?: MaterialTheme.colorScheme.primary
        leading?.invoke()
        icon?.let { Icon(it, null, tint = color, modifier = Modifier.size(16.dp)) }
        Text(text, style = MaterialTheme.typography.titleSmall, color = color, fontWeight = FontWeight.SemiBold, maxLines = 1, modifier = Modifier.weight(1f))
        trailing?.invoke()
    }
}

/** A centred icon, title, message and optional actions for an empty screen. */
@Composable
fun EmptyState(icon: ImageVector, title: String, message: String? = null, modifier: Modifier = Modifier, actions: @Composable () -> Unit = {}) {
    Column(
        modifier.fillMaxWidth().padding(horizontal = Space.xl, vertical = Space.xxl),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        Icon(icon, null, tint = MaterialTheme.colorScheme.outline, modifier = Modifier.size(48.dp))
        Text(title, style = MaterialTheme.typography.titleMedium, textAlign = TextAlign.Center)
        message?.let { Text(it, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, textAlign = TextAlign.Center) }
        actions()
    }
}

fun copyText(context: Context, text: String, label: String = "Domovoi") {
    context.getSystemService(ClipboardManager::class.java)?.setPrimaryClip(ClipData.newPlainText(label, text))
    // Android 13+ shows its own confirmation; before, the app's toast says it.
    if (android.os.Build.VERSION.SDK_INT < 33) com.crayen.domovoi.DomovoiApp.core(context).update(Event.ShowNotice("success", "Copied"))
}

fun clipboardText(context: Context): String? =
    context.getSystemService(ClipboardManager::class.java)?.primaryClip?.takeIf { it.itemCount > 0 }?.getItemAt(0)?.coerceToText(context)?.toString()

/** "42 s", "3 min 5 s", "1 h 4 min". */
fun duration(ms: Long): String {
    val s = (ms / 1000).toInt()
    return when {
        s < 60 -> "$s s"
        s < 3600 -> "${s / 60} min ${s % 60} s"
        else -> "${s / 3600} h ${s % 3600 / 60} min"
    }
}
