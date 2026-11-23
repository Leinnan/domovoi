package com.crayen.domovoi.ui.common

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.crayen.domovoi.app.LimitWindowView
import com.crayen.domovoi.ui.theme.LocalTints
import com.crayen.domovoi.ui.theme.Space
import java.time.Instant

/** A window's colour: red all but used up (90 %), orange nearly (75 %). */
@Composable
fun limitColor(window: LimitWindowView): Color = when {
    window.percent >= 90u -> MaterialTheme.colorScheme.error
    window.high -> LocalTints.current.caution
    else -> MaterialTheme.colorScheme.primary
}

/** `Ahead of pace`, `On pace`, `Under pace`; empty when not known. */
val LimitWindowView.paceLabel: String
    get() = when (pace) {
        "ahead" -> "Ahead of pace"
        "on" -> "On pace"
        "under" -> "Under pace"
        else -> ""
    }

/** `Resets in 2d · Tue 14:00`; empty when not known. */
val LimitWindowView.resetsLabel: String
    get() {
        if (resets.isEmpty()) return ""
        if (resets == "now") return "Resets now"
        val at = runCatching { Instant.parse(resetsAt).toEpochMilli() }.getOrNull()
        return if (at == null) "Resets $resets" else "Resets $resets · ${SnoozeTimes.label(at)}"
    }

/** A bar of how full something is; `tick` (0–100, 0: none) marks a point on it. */
@Composable
fun PercentBar(percent: UInt, color: Color, modifier: Modifier = Modifier, tick: UInt = 0u) {
    val track = MaterialTheme.colorScheme.surfaceContainerHighest
    val mark = MaterialTheme.colorScheme.onSurface
    Canvas(modifier.fillMaxWidth().height(6.dp)) {
        val radius = CornerRadius(size.height / 2, size.height / 2)
        drawRoundRect(track, cornerRadius = radius)
        val used = (percent.toFloat() / 100f).coerceIn(0f, 1f)
        if (used > 0f) drawRoundRect(color, size = Size(size.width * used, size.height), cornerRadius = radius)
        if (tick in 1u..99u) {
            val x = size.width * tick.toFloat() / 100f
            drawLine(mark.copy(alpha = 0.7f), Offset(x, -2.dp.toPx()), Offset(x, size.height + 2.dp.toPx()), strokeWidth = 2.dp.toPx())
        }
    }
}

/**
 * One account usage window: its name and percent, a bar (its tick how far
 * through the period it is), when it refills and how its use keeps pace.
 */
@Composable
fun LimitWindowRow(window: LimitWindowView, modifier: Modifier = Modifier) {
    val color = limitColor(window)
    val details = listOf(window.resetsLabel, window.paceLabel).filter { it.isNotEmpty() }
    val description = (listOf("${window.label}: ${window.percent} percent used") + details).joinToString(", ")
    Column(modifier.fillMaxWidth().semantics(mergeDescendants = true) { contentDescription = description }, verticalArrangement = Arrangement.spacedBy(Space.xxs)) {
        Row(horizontalArrangement = Arrangement.spacedBy(Space.s)) {
            Text(window.label, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f))
            Text("${window.percent}%", style = MaterialTheme.typography.labelLarge, color = color, fontWeight = FontWeight.Medium)
        }
        PercentBar(window.percent, color, tick = window.elapsedPercent)
        if (details.isNotEmpty()) {
            Text(
                details.joinToString(" · "),
                style = MaterialTheme.typography.labelSmall,
                color = if (window.pace == "ahead" && window.high) color else MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}
