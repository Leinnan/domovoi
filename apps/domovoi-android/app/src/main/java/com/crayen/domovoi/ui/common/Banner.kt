package com.crayen.domovoi.ui.common

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.expandVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.shrinkVertically
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.CloudOff
import androidx.compose.material.icons.outlined.PowerSettingsNew
import androidx.compose.material.icons.outlined.Sync
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.SnackbarDuration
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.SnackbarResult
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.crayen.domovoi.app.ConnectionView
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.core.Core
import com.crayen.domovoi.ui.theme.Space
import kotlinx.coroutines.delay

private enum class BannerKind { Offline, Waiting, Trying, Disconnected }

private fun bannerKind(c: ConnectionView, retryAt: Long?): BannerKind? {
    if (c.activeEnvironmentId.isEmpty() || c.needsPairing || c.state == "connected") return null
    if (c.offline) return BannerKind.Offline
    return when (c.state) {
        "reconnecting" -> if (c.attempting || retryAt == null) BannerKind.Trying else BannerKind.Waiting
        "connecting" -> BannerKind.Trying
        "disconnected" -> BannerKind.Disconnected
        else -> null
    }
}

/** What the connection is doing when it isn't simply connected (`ConnectionBanner`). */
@Composable
fun ConnectionBanner(core: Core, modifier: Modifier = Modifier) {
    val c by core.view.connection.watch()
    val retryAt by core.retryAt.watch()
    val kind = bannerKind(c, retryAt)
    AnimatedVisibility(kind != null, enter = expandVertically() + fadeIn(), exit = shrinkVertically() + fadeOut(), modifier = modifier) {
        val k = kind ?: return@AnimatedVisibility
        val server = c.environments.firstOrNull { it.id == c.activeEnvironmentId }?.label ?: "the server"
        var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
        LaunchedEffect(k, retryAt) {
            while (k == BannerKind.Waiting) { now = System.currentTimeMillis(); delay(1000) }
        }
        val headline = when (k) {
            BannerKind.Offline -> "No network"
            BannerKind.Trying -> if (c.state == "connecting") "Connecting…" else "Reconnecting…"
            BannerKind.Disconnected -> "Disconnected"
            BannerKind.Waiting -> retryAt?.let { "Reconnecting in ${maxOf(1, ((it - now + 999) / 1000).toInt())} s" } ?: "Reconnecting…"
        }
        val base = when (k) {
            BannerKind.Offline -> "Waiting for Wi-Fi or mobile data. It reconnects by itself."
            BannerKind.Disconnected -> "You disconnected from $server."
            else -> listOfNotNull(c.error, c.hint.ifEmpty { null }).joinToString(" ").ifEmpty { null }
        }
        val detail = if (c.showingSaved) listOfNotNull(base, "Showing what was saved when last connected.").joinToString(" ") else base
        val container = if (k == BannerKind.Offline || k == BannerKind.Disconnected) MaterialTheme.colorScheme.surfaceContainerHighest else MaterialTheme.colorScheme.secondaryContainer
        Surface(color = container, shape = MaterialTheme.shapes.large, modifier = Modifier.fillMaxWidth().padding(horizontal = Space.m, vertical = Space.xs)) {
            Row(Modifier.padding(horizontal = Space.l, vertical = Space.m), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.m)) {
                when (k) {
                    BannerKind.Trying -> CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                    BannerKind.Offline -> Icon(Icons.Outlined.CloudOff, null)
                    BannerKind.Waiting -> Icon(Icons.Outlined.Sync, null)
                    BannerKind.Disconnected -> Icon(Icons.Outlined.PowerSettingsNew, null)
                }
                Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(Space.xxs)) {
                    Text(headline, style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.SemiBold)
                    detail?.let { Text(it, style = MaterialTheme.typography.bodySmall, maxLines = 4, color = MaterialTheme.colorScheme.onSurfaceVariant) }
                }
                when (k) {
                    BannerKind.Waiting -> FilledTonalButton(onClick = { core.update(Event.Reconnect) }) { Text("Retry now") }
                    BannerKind.Disconnected -> FilledTonalButton(onClick = { core.update(Event.Reconnect) }) { Text("Connect") }
                    else -> Unit
                }
            }
        }
    }
}

/** The core's toast as a Material snackbar, with its action (Undo) when it has one. */
@Composable
fun ToastEffect(core: Core, host: SnackbarHostState) {
    val toast by core.view.toast.watch()
    LaunchedEffect(toast?.id) {
        val t = toast ?: return@LaunchedEffect
        val extra = t.message.length / 40 * 1000L
        val base = when {
            t.actionLabel.isNotEmpty() -> 7_000L
            t.kind == "error" -> 6_000L
            else -> 3_000L
        }
        host.currentSnackbarData?.dismiss()
        val shown = kotlinx.coroutines.withTimeoutOrNull(base + extra) {
            host.showSnackbar(
                message = if (t.queued > 0u) "${t.message}  (+${t.queued})" else t.message,
                actionLabel = t.actionLabel.ifEmpty { null },
                withDismissAction = t.actionLabel.isEmpty(),
                duration = SnackbarDuration.Indefinite,
            )
        }
        if (shown == SnackbarResult.ActionPerformed) core.update(Event.ToastAction) else core.update(Event.DismissToast)
    }
}
