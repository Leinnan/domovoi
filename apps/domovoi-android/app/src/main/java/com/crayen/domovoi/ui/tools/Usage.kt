package com.crayen.domovoi.ui.tools

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.OpenInNew
import androidx.compose.material.icons.outlined.Warning
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.drawText
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.app.ProviderLimitsView
import com.crayen.domovoi.app.UsageRowView
import com.crayen.domovoi.app.UsageSeriesPointView
import com.crayen.domovoi.app.UsageSliceView
import com.crayen.domovoi.app.UsageView
import com.crayen.domovoi.core.Core
import com.crayen.domovoi.ui.common.LimitWindowRow
import com.crayen.domovoi.ui.common.SectionHeader
import com.crayen.domovoi.ui.common.agentColor
import com.crayen.domovoi.ui.common.watch
import com.crayen.domovoi.ui.theme.LocalTints
import com.crayen.domovoi.ui.theme.Space

/**
 * Tokens used over a span: by provider, a chart by day (or hour, for
 * Today), what they were made of, each provider's account limits, and the
 * totals by model and by day.
 */
@Composable
fun UsageScreen(core: Core) {
    val usage by core.view.usage.watch()
    val uri = LocalUriHandler.current
    // The span the core has (`LoadUsage` sets it); a week the first time.
    LaunchedEffect(Unit) { core.update(Event.LoadUsage(if (usage.days > 0u) usage.days else 7u)) }
    ToolScaffold("Usage", onBack = null, actions = {
        TextButton(onClick = { core.update(Event.RefreshUsageLimits) }, enabled = !usage.limitsRefreshing) {
            Text(if (usage.limitsRefreshing) "Refreshing…" else "Refresh limits")
        }
    }) { padding ->
        LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(top = padding.calculateTopPadding(), bottom = Space.xl)) {
            item("range") {
                val ranges = listOf(1u to "Today", 7u to "7 days", 30u to "30 days", 90u to "90 days")
                SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth().padding(horizontal = Space.l, vertical = Space.s)) {
                    ranges.forEachIndexed { i, (d, label) ->
                        SegmentedButton(
                            selected = usage.days == d, onClick = { if (usage.days != d) core.update(Event.LoadUsage(d)) },
                            shape = SegmentedButtonDefaults.itemShape(i, ranges.size),
                        ) { Text(label, maxLines = 1) }
                    }
                }
            }
            if (usage.providers.isNotEmpty()) item("providers") { ProviderFilter(usage) { core.update(Event.FilterUsageProvider(it)) } }
            if (usage.loading) item("loading") { LinearProgressIndicator(Modifier.fillMaxWidth().padding(horizontal = Space.l)) }
            usage.error?.let { error -> item("error") { Notice(error, MaterialTheme.colorScheme.error) } }
            if (usage.notice.isNotEmpty()) item("notice") { Text(usage.notice, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(horizontal = Space.l, vertical = Space.xs)) }
            // Log sources that couldn't be read in full: the totals may be short.
            usage.sourceNotices.forEachIndexed { i, note -> item("source/$i") { Notice(note, LocalTints.current.caution) } }
            item("total") { Totals(usage) }
            if (usage.series.isNotEmpty()) item("chart") { ActivityChart(usage) }
            if (usage.tokenMix.isNotEmpty()) item("mix") { TokenMix(usage.tokenMix) }
            if (usage.limits.isNotEmpty()) {
                item("limits") {
                    SectionHeader("Limits")
                }
                usage.limits.forEach { provider ->
                    item("limits/${provider.instanceId}") { ProviderLimits(provider) { runCatching { uri.openUri(it) } } }
                }
            }
            if (usage.byModel.isNotEmpty()) {
                item("by-model") { SectionHeader("By model") }
                usage.byModel.forEachIndexed { i, row -> item("model/$i") { UsageRow(row, row.model) } }
            }
            // Newest first, under a header per day (Today: per hour).
            var lastGroup = ""
            usage.rows.forEachIndexed { i, row ->
                val group = if (usage.hourly && row.hourLabel.isNotEmpty()) row.hourLabel else row.day
                if (group != lastGroup) {
                    lastGroup = group
                    item("group/$i") { SectionHeader(group) }
                }
                item("row/$i") { UsageRow(row, row.model) }
            }
        }
    }
}

/** A provider's colour in the chart, the chips and the rows. */
@Composable
private fun providerColor(key: String): Color = agentColor(key) ?: MaterialTheme.colorScheme.primary

/** `42%`, `<1%` for a share that rounds to nothing, or `—` with no tokens. */
private fun percentLabel(percent: UInt, tokens: ULong): String = when {
    tokens == 0uL -> "—"
    percent == 0u -> "<1%"
    else -> "$percent%"
}

@Composable
private fun Notice(text: String, color: Color) {
    Surface(color = color.copy(alpha = 0.12f), shape = MaterialTheme.shapes.small, modifier = Modifier.fillMaxWidth().padding(horizontal = Space.l, vertical = Space.xs)) {
        Row(Modifier.padding(Space.s), horizontalArrangement = Arrangement.spacedBy(Space.s), verticalAlignment = Alignment.CenterVertically) {
            Icon(Icons.Outlined.Warning, null, tint = color, modifier = Modifier.size(16.dp))
            Text(text, style = MaterialTheme.typography.bodySmall)
        }
    }
}

/** All providers, or one: each chip with its tokens and share; a bar of the shares under them. */
@Composable
private fun ProviderFilter(usage: UsageView, select: (String) -> Unit) {
    Column(Modifier.fillMaxWidth().padding(vertical = Space.xs), verticalArrangement = Arrangement.spacedBy(Space.xs)) {
        Row(Modifier.horizontalScroll(rememberScrollState()).padding(horizontal = Space.l), horizontalArrangement = Arrangement.spacedBy(Space.s)) {
            FilterChip(selected = usage.selectedProvider.isEmpty(), onClick = { select("") }, label = { Text("All providers") })
            usage.providers.forEach { p ->
                FilterChip(
                    selected = usage.selectedProvider == p.key,
                    onClick = { select(if (usage.selectedProvider == p.key) "" else p.key) },
                    leadingIcon = { Box(Modifier.size(8.dp).background(providerColor(p.key), CircleShape)) },
                    label = { Text("${p.label} · ${p.tokensLabel} · ${percentLabel(p.sharePercent, p.tokens)}", maxLines = 1) },
                )
            }
        }
        val shares = usage.providers.filter { it.sharePercent > 0u }
        if (usage.providers.size > 1 && shares.isNotEmpty()) {
            val colors = shares.map { providerColor(it.key) }
            Canvas(Modifier.fillMaxWidth().padding(horizontal = Space.l).height(6.dp).clip(CircleShape)) {
                val total = shares.sumOf { it.sharePercent.toInt() }.coerceAtLeast(1).toFloat()
                var x = 0f
                shares.forEachIndexed { i, p ->
                    val w = size.width * p.sharePercent.toFloat() / total
                    drawRect(colors[i], Offset(x, 0f), Size(w, size.height))
                    x += w
                }
            }
        }
    }
}

/** The span's total, its cost, the cache hit rate and the average per active day. */
@Composable
private fun Totals(usage: UsageView) {
    val activity = if (usage.hourly || usage.activeDays == 0u) {
        "${usage.sessions} sessions · ${usage.turns} turns"
    } else {
        "${usage.activeDays} active ${if (usage.activeDays == 1u) "day" else "days"} · ${usage.sessions} sessions · ${usage.turns} turns"
    }
    Column(Modifier.fillMaxWidth().padding(Space.l), verticalArrangement = Arrangement.spacedBy(Space.s)) {
        Surface(color = MaterialTheme.colorScheme.primaryContainer, shape = MaterialTheme.shapes.large, modifier = Modifier.fillMaxWidth()) {
            Row(Modifier.padding(Space.l), verticalAlignment = Alignment.Bottom) {
                Column(Modifier.weight(1f).semantics(mergeDescendants = true) {
                    if (usage.totalTokensExact.isNotEmpty()) contentDescription = "${usage.totalTokensExact} tokens"
                }) {
                    Text(usage.rangeLabel.ifEmpty { "Total" }, style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.onPrimaryContainer)
                    Text(usage.totalTokens.ifEmpty { "—" }, style = MaterialTheme.typography.displaySmall, color = MaterialTheme.colorScheme.onPrimaryContainer)
                    Text(
                        if (usage.totalTokensExact.isNotEmpty()) "${usage.totalTokensExact} tokens" else "tokens",
                        style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onPrimaryContainer,
                    )
                }
                if (usage.totalCost.isNotEmpty()) {
                    Column(horizontalAlignment = Alignment.End) {
                        Text(usage.totalCost, style = MaterialTheme.typography.headlineSmall, color = MaterialTheme.colorScheme.onPrimaryContainer, fontWeight = FontWeight.SemiBold)
                        Text("estimated cost", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onPrimaryContainer)
                    }
                }
            }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(Space.s)) {
            Kpi("Cache hit", usage.cacheHitLabel, "Input read from cache", Modifier.weight(1f))
            Kpi(if (usage.hourly) "Per active hour" else "Per active day", usage.dailyAvgTokens, activity, Modifier.weight(1f))
        }
    }
}

@Composable
private fun Kpi(label: String, value: String, sub: String, modifier: Modifier = Modifier) {
    Surface(color = MaterialTheme.colorScheme.surfaceContainer, shape = MaterialTheme.shapes.medium, modifier = modifier) {
        Column(Modifier.padding(Space.m), verticalArrangement = Arrangement.spacedBy(Space.xxs)) {
            Text(label, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            Text(value.ifEmpty { "—" }, style = MaterialTheme.typography.titleLarge, fontWeight = FontWeight.SemiBold, maxLines = 1)
            Text(sub, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 2)
        }
    }
}

/**
 * Tokens per day (per hour for Today), each bar stacked by provider; a
 * tap on a bar shows its day and total above the chart.
 */
@Composable
private fun ActivityChart(usage: UsageView) {
    val series = usage.series
    var selected by remember(series) { mutableStateOf<Int?>(null) }
    val measurer = rememberTextMeasurer()
    val axisStyle = MaterialTheme.typography.labelSmall.copy(color = MaterialTheme.colorScheme.onSurfaceVariant)
    val highlight = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.06f)
    val accent = MaterialTheme.colorScheme.primary
    val keys = series.flatMap { p -> p.byProvider.map { it.providerKey } }.distinct()
    val colors = keys.associateWith { providerColor(it) }
    val point = selected?.let { series.getOrNull(it) }
    val peak = series.maxByOrNull { it.totalTokens }
    Surface(color = MaterialTheme.colorScheme.surfaceContainer, shape = MaterialTheme.shapes.medium, modifier = Modifier.fillMaxWidth().padding(horizontal = Space.l)) {
        Column(Modifier.padding(Space.m), verticalArrangement = Arrangement.spacedBy(Space.s)) {
            Text(if (usage.hourly) "Today by hour" else "Daily activity", style = MaterialTheme.typography.titleSmall)
            Text(
                point?.let { callout(it) } ?: "Tap a bar for its total",
                style = MaterialTheme.typography.labelMedium,
                color = if (point != null) MaterialTheme.colorScheme.onSurface else MaterialTheme.colorScheme.onSurfaceVariant,
                fontWeight = if (point != null) FontWeight.SemiBold else FontWeight.Normal,
                maxLines = 1, overflow = TextOverflow.Ellipsis,
            )
            Canvas(
                Modifier.fillMaxWidth().height(170.dp)
                    .semantics { contentDescription = (if (usage.hourly) "Tokens by hour" else "Tokens by day") + (peak?.let { ", most on ${it.detailLabel}: ${it.totalLabel}" } ?: "") }
                    .pointerInput(series) {
                        detectTapGestures { at ->
                            val i = (at.x / (size.width.toFloat() / series.size)).toInt().coerceIn(0, series.lastIndex)
                            selected = if (selected == i) null else i
                        }
                    },
            ) {
                val bottom = 18.dp.toPx()
                val chartH = size.height - bottom
                val slot = size.width / series.size
                val barW = (slot * 0.72f).coerceIn(2.dp.toPx(), 24.dp.toPx())
                val max = (series.maxOfOrNull { it.totalTokens } ?: 1uL).coerceAtLeast(1uL).toFloat()
                val radius = CornerRadius(2.dp.toPx())
                series.forEachIndexed { i, p ->
                    val x = i * slot + (slot - barW) / 2
                    if (selected == i) drawRect(highlight, Offset(x - 2.dp.toPx(), 0f), Size(barW + 4.dp.toPx(), chartH))
                    var y = chartH
                    if (p.byProvider.isEmpty()) {
                        val h = p.totalTokens.toFloat() / max * chartH
                        if (h > 0f) drawRoundRect(accent, Offset(x, y - h), Size(barW, h), radius)
                    } else {
                        p.byProvider.forEach { part ->
                            val h = part.tokens.toFloat() / max * chartH
                            if (h > 0.5f) {
                                y -= h
                                drawRect(colors[part.providerKey] ?: accent, Offset(x, y), Size(barW, h))
                            }
                        }
                    }
                }
                // A handful of labels along the bottom, whatever the span.
                val step = (series.size / 5).coerceAtLeast(1)
                for (i in series.indices step step) {
                    val text = measurer.measure(series[i].label, axisStyle)
                    val x = (i * slot + slot / 2 - text.size.width / 2).coerceIn(0f, (size.width - text.size.width).coerceAtLeast(0f))
                    drawText(text, topLeft = Offset(x, chartH + 4.dp.toPx()))
                }
            }
            // Which colour is which provider.
            if (keys.size > 1) {
                val names = series.flatMap { it.byProvider }.associate { it.providerKey to it.provider }
                FlowRow(horizontalArrangement = Arrangement.spacedBy(Space.m), verticalArrangement = Arrangement.spacedBy(Space.xxs)) {
                    keys.forEach { key -> Legend(colors.getValue(key), names[key].orEmpty().ifEmpty { key }) }
                }
            }
        }
    }
}

/** `Nov 22, 2026: 1.2M · Claude 900k · Codex 300k ($4.10)`. */
private fun callout(p: UsageSeriesPointView): String {
    val parts = if (p.byProvider.size > 1) " · " + p.byProvider.filter { it.tokens > 0uL }.joinToString(" · ") { "${it.provider} ${compact(it.tokens)}" } else ""
    return "${p.detailLabel}: ${p.totalLabel}$parts" + if (p.cost.isNotEmpty()) " (${p.cost})" else ""
}

/** `1.2M`, `340k`, `512`. */
private fun compact(tokens: ULong): String {
    val n = tokens.toDouble()
    return when {
        n >= 1e9 -> "%.1fB".format(n / 1e9)
        n >= 1e6 -> "%.1fM".format(n / 1e6)
        n >= 1e3 -> "%.0fk".format(n / 1e3)
        else -> tokens.toString()
    }
}

@Composable
private fun Legend(color: Color, label: String) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.xs)) {
        Box(Modifier.size(8.dp).background(color, CircleShape))
        Text(label, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

/** What the tokens were: input, cache reads and writes, output, reasoning, as one bar. */
@Composable
private fun TokenMix(slices: List<UsageSliceView>) {
    val colors = slices.map { sliceColor(it.kind) }
    Surface(color = MaterialTheme.colorScheme.surfaceContainer, shape = MaterialTheme.shapes.medium, modifier = Modifier.fillMaxWidth().padding(Space.l)) {
        Column(Modifier.padding(Space.m), verticalArrangement = Arrangement.spacedBy(Space.s)) {
            Text("Token composition", style = MaterialTheme.typography.titleSmall)
            Canvas(Modifier.fillMaxWidth().height(10.dp).clip(CircleShape)) {
                val total = slices.sumOf { it.tokens.toDouble() }.coerceAtLeast(1.0).toFloat()
                var x = 0f
                slices.forEachIndexed { i, s ->
                    val w = size.width * s.tokens.toFloat() / total
                    drawRect(colors[i], Offset(x, 0f), Size(w, size.height))
                    x += w
                }
            }
            slices.forEachIndexed { i, s ->
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                    Box(Modifier.size(8.dp).background(colors[i], CircleShape))
                    Text(s.label, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f))
                    Text(s.tokensLabel, style = MaterialTheme.typography.bodyMedium, fontWeight = FontWeight.Medium)
                    Text("${s.percent}%", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
        }
    }
}

@Composable
private fun sliceColor(kind: String): Color {
    val tints = LocalTints.current
    return when (kind) {
        "input" -> MaterialTheme.colorScheme.primary
        "cached" -> MaterialTheme.colorScheme.tertiary
        "cache_write" -> tints.question
        "output" -> tints.success
        "reasoning" -> tints.plan
        else -> MaterialTheme.colorScheme.outline
    }
}

/** A provider's account windows, when they were read, and why they are missing. */
@Composable
private fun ProviderLimits(provider: ProviderLimitsView, open: (String) -> Unit) {
    Column(Modifier.fillMaxWidth().padding(horizontal = Space.l, vertical = Space.s), verticalArrangement = Arrangement.spacedBy(Space.m)) {
        Row(verticalAlignment = Alignment.Bottom) {
            Text(provider.provider, style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
            if (provider.checked.isNotEmpty()) {
                Text(
                    if (provider.checked == "now") "Checked now" else "Checked ${provider.checked} ago",
                    style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        if (provider.notice.isNotEmpty()) Text(provider.notice, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        provider.windows.forEach { LimitWindowRow(it) }
        if (provider.externalUrl.isNotEmpty()) {
            TextButton(onClick = { open(provider.externalUrl) }, contentPadding = PaddingValues(0.dp)) {
                Text(provider.externalLabel.ifEmpty { "Usage on the web" })
                Icon(Icons.AutoMirrored.Outlined.OpenInNew, null, modifier = Modifier.padding(start = Space.xs).size(16.dp))
            }
        }
    }
}

/**
 * A model's (or a day's) tokens: by kind, its share of the span, sessions
 * and cache hit rate, and cost when the server has prices.
 */
@Composable
private fun UsageRow(row: UsageRowView, title: String) {
    val kinds = listOf("in" to row.input, "cached" to row.cached, "cache write" to row.cacheWrite, "out" to row.output, "reasoning" to row.reasoning)
        .filter { (_, v) -> v.isNotEmpty() && v != "0" }
        .joinToString(" · ") { (k, v) -> "$k $v" }
    val stats = buildList {
        if (row.sessions > 0uL) add("${row.sessions} ${if (row.sessions == 1uL) "session" else "sessions"}")
        if (row.cacheHitPercent > 0u) add("cache hit ${row.cacheHitPercent}%")
    }.joinToString(" · ")
    ListItem(
        leadingContent = { Box(Modifier.size(10.dp).background(providerColor(row.providerKey), CircleShape)) },
        headlineContent = { Text(title, maxLines = 1, overflow = TextOverflow.Ellipsis) },
        supportingContent = {
            Column {
                Text(listOf(row.provider, kinds).filter { it.isNotEmpty() }.joinToString(" · "), maxLines = 2, overflow = TextOverflow.Ellipsis)
                if (stats.isNotEmpty()) Text(stats, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        },
        trailingContent = {
            Column(horizontalAlignment = Alignment.End) {
                Text(row.total, style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.SemiBold)
                Text(percentLabel(row.sharePercent, row.totalTokens), style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                if (row.cost.isNotEmpty()) Text(row.cost, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        },
        colors = ListItemDefaults.colors(containerColor = Color.Transparent),
    )
}
