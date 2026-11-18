package com.crayen.domovoi.ui.thread

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.animation.expandVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.shrinkVertically
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.CallSplit
import androidx.compose.material.icons.automirrored.outlined.OpenInNew
import androidx.compose.material.icons.automirrored.outlined.Undo
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.PanTool
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material.icons.outlined.AttachFile
import androidx.compose.material.icons.outlined.AutoAwesome
import androidx.compose.material.icons.outlined.Build
import androidx.compose.material.icons.outlined.ChatBubbleOutline
import androidx.compose.material.icons.outlined.Checklist
import androidx.compose.material.icons.outlined.ChevronRight
import androidx.compose.material.icons.outlined.ContentCopy
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material.icons.outlined.Flag
import androidx.compose.material.icons.outlined.Groups
import androidx.compose.material.icons.outlined.HelpOutline
import androidx.compose.material.icons.outlined.Info
import androidx.compose.material.icons.outlined.KeyboardArrowDown
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material.icons.outlined.Pause
import androidx.compose.material.icons.outlined.Psychology
import androidx.compose.material.icons.outlined.RadioButtonUnchecked
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.Schedule
import androidx.compose.material.icons.outlined.StopCircle
import androidx.compose.material.icons.outlined.Terminal
import androidx.compose.material.icons.outlined.Timelapse
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Badge
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SmallFloatingActionButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import coil3.compose.AsyncImage
import com.crayen.domovoi.app.DiffFileView
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.app.ImageView
import com.crayen.domovoi.app.ItemView
import com.crayen.domovoi.app.PendingSendView
import com.crayen.domovoi.app.QueuedMessageView
import com.crayen.domovoi.app.ThreadLinkView
import com.crayen.domovoi.app.ThreadPageView
import com.crayen.domovoi.ui.common.MarkdownBlocks
import com.crayen.domovoi.ui.common.copyText
import com.crayen.domovoi.ui.common.duration
import com.crayen.domovoi.ui.theme.LocalTints
import com.crayen.domovoi.ui.theme.Mono
import com.crayen.domovoi.ui.theme.Space
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import java.text.DateFormat
import java.util.Date

/** What transcript rows can ask: the core's update, opening a thread, the diff. */
@Immutable
class TranscriptActions(val send: (Event) -> Unit, val openThread: (String) -> Unit, val openDiff: () -> Unit)

/** One line of the transcript: an item, or a turn's steps folded together. */
private sealed interface Line {
    val key: String
    data class Item(val item: ItemView) : Line { override val key get() = item.id }
    data class Group(val id: String, val summary: String, val items: List<ItemView>) : Line { override val key get() = "group-$id" }
}

private fun lines(items: List<ItemView>): List<Line> {
    val out = mutableListOf<Line>()
    var i = 0
    while (i < items.size) {
        val group = items[i].group
        if (group.isEmpty()) { out += Line.Item(items[i]); i++; continue }
        val start = i
        while (i < items.size && items[i].group == group) i++
        val members = items.subList(start, i)
        out += Line.Group(group, members.first().groupSummary, members.toList())
    }
    return out
}

/**
 * The conversation, newest at the bottom. The list is laid out from the
 * bottom (reversed), so output growing at the end keeps the end in view;
 * a reader who scrolled up stays where they are, with a jump button that
 * dots when something new arrived.
 */
@Composable
fun Transcript(thread: ThreadPageView, retryBlocked: Boolean, actions: TranscriptActions, modifier: Modifier = Modifier, contentPadding: PaddingValues = PaddingValues()) {
    val state = rememberLazyListState()
    val scope = rememberCoroutineScope()
    val lines = remember(thread.items) { lines(thread.items) }
    val atEnd by remember { derivedStateOf { state.firstVisibleItemIndex == 0 && state.firstVisibleItemScrollOffset < 80 } }
    var unseen by remember(thread.id) { mutableStateOf(false) }
    val signature = Triple(thread.items.size, thread.pendingSends.size, thread.working)
    LaunchedEffect(signature) {
        if (atEnd) state.scrollToItem(0) else unseen = true
    }
    LaunchedEffect(atEnd) { if (atEnd) unseen = false }
    // Find: bring the active match into view.
    val active = thread.find?.activeItemId
    LaunchedEffect(active) {
        val id = active ?: return@LaunchedEffect
        val index = state.layoutInfo.totalItemsCount
        val target = lines.indexOfFirst { l -> l is Line.Item && l.item.id == id || l is Line.Group && l.items.any { it.id == id } }
        if (target >= 0 && index > 0) state.animateScrollToItem(tailCount(thread) + (lines.size - 1 - target))
    }

    Box(modifier) {
        LazyColumn(
            state = state,
            reverseLayout = true,
            contentPadding = PaddingValues(
                start = Space.l, end = Space.l,
                top = contentPadding.calculateTopPadding() + Space.l,
                bottom = contentPadding.calculateBottomPadding() + Space.l,
            ),
            verticalArrangement = Arrangement.spacedBy(Space.l, Alignment.Bottom),
            modifier = Modifier.fillMaxSize(),
        ) {
            // Reversed: the end first.
            item("bottom") { Spacer(Modifier.height(1.dp)) }
            if (thread.working) {
                item("working") { WorkingRow(thread.status, thread.workingSinceMs, thread.background.ifEmpty { liveSummary(lines) }) }
            } else if (thread.status == "interrupted" && thread.pendingSends.isEmpty() && thread.items.isNotEmpty()) {
                item("stopped") { StoppedRow(retryBlocked) { actions.send(Event.Retry) } }
            }
            if (thread.queued.isNotEmpty()) item("queued") { QueuedSection(thread.queued, thread.queueHeld, actions) }
            for (pending in thread.pendingSends.asReversed()) item("pending-${pending.id}") { PendingSendRow(pending) { actions.send(Event.CancelSend(pending.id)) } }
            for (index in lines.indices.reversed()) {
                val line = lines[index]
                item(line.key, contentType = (line as? Line.Item)?.item?.kind ?: "group") {
                    Column(Modifier.widthIn(max = Space.column)) {
                        when (line) {
                            is Line.Item -> ItemRow(line.item, retryBlocked, actions)
                            is Line.Group -> StepGroup(line.summary, line.items, live = thread.working && index == lines.lastIndex, actions)
                        }
                    }
                }
            }
            if (thread.loading && thread.items.isEmpty()) {
                item("loading") { Box(Modifier.fillMaxWidth().padding(Space.xxl), contentAlignment = Alignment.Center) { CircularProgressIndicator() } }
            } else if (thread.items.isEmpty() && thread.pendingSends.isEmpty() && thread.queued.isEmpty() && !thread.working) {
                item("empty") { EmptyThreadHint(thread) }
            }
            thread.parent?.let { parent -> item("parent") { ParentBanner(parent, thread.canMergeBack, actions) } }
        }
        AnimatedVisibility(!atEnd, enter = fadeIn(), exit = fadeOut(), modifier = Modifier.align(Alignment.BottomEnd).padding(end = Space.l, bottom = contentPadding.calculateBottomPadding() + Space.l)) {
            Box {
                SmallFloatingActionButton(onClick = { scope.launch { state.animateScrollToItem(0) } }) { Icon(Icons.Outlined.KeyboardArrowDown, "Jump to the end") }
                if (unseen) Badge(Modifier.align(Alignment.TopEnd))
            }
        }
    }
}

/** How many rows sit below the items in the reversed list. */
private fun tailCount(t: ThreadPageView): Int =
    1 + (if (t.working || (t.status == "interrupted" && t.pendingSends.isEmpty() && t.items.isNotEmpty())) 1 else 0) +
        (if (t.queued.isNotEmpty()) 1 else 0) + t.pendingSends.size

private fun liveSummary(lines: List<Line>): String? = (lines.lastOrNull() as? Line.Group)?.summary?.ifEmpty { null }

// ---- rows ----

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun ItemRow(item: ItemView, retryBlocked: Boolean, actions: TranscriptActions) {
    var menu by remember { mutableStateOf(false) }
    var selecting by remember { mutableStateOf(false) }
    val haptics = LocalHapticFeedback.current
    val context = LocalContext.current
    val hasMenu = item.kind in setOf("user", "assistant", "plan", "error", "command")
    Box {
        Column(
            Modifier.then(
                if (hasMenu) Modifier.combinedClickable(
                    interactionSource = null, indication = null,
                    onClick = {}, onLongClick = { haptics.performHapticFeedback(HapticFeedbackType.LongPress); menu = true },
                ) else Modifier,
            ),
            verticalArrangement = Arrangement.spacedBy(Space.xs),
        ) {
            ItemContent(item, retryBlocked, actions)
            if (item.runDurationMs > 0 && item.group.isEmpty()) TurnFooter(item.runStartedMs, item.runDurationMs)
        }
        if (item.findMatches > 0u) {
            Surface(
                color = if (item.findActive) Color(0xFFFFD54F) else Color(0x80FFD54F),
                shape = CircleShape,
                modifier = Modifier.align(Alignment.TopEnd),
            ) { Text("${item.findMatches}", style = MaterialTheme.typography.labelSmall, color = Color.Black, modifier = Modifier.padding(horizontal = 6.dp)) }
        }
        DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
            when (item.kind) {
                "user", "assistant", "plan" -> {
                    DropdownMenuItem(text = { Text("Copy") }, leadingIcon = { Icon(Icons.Outlined.ContentCopy, null) }, onClick = { menu = false; copyText(context, item.text) })
                    DropdownMenuItem(text = { Text("Select text") }, leadingIcon = { Icon(Icons.Outlined.Description, null) }, onClick = { menu = false; selecting = true })
                    if (item.kind == "user") DropdownMenuItem(text = { Text("Edit and send again") }, leadingIcon = { Icon(Icons.Outlined.Edit, null) }, onClick = { menu = false; actions.send(Event.EditMessage(item.text)) })
                }
                "error" -> DropdownMenuItem(text = { Text("Copy error") }, leadingIcon = { Icon(Icons.Outlined.ContentCopy, null) }, onClick = { menu = false; copyText(context, item.title) })
                "command" -> {
                    DropdownMenuItem(text = { Text("Copy command") }, leadingIcon = { Icon(Icons.Outlined.ContentCopy, null) }, onClick = { menu = false; copyText(context, item.title) })
                    item.output?.let { out -> DropdownMenuItem(text = { Text("Copy output") }, leadingIcon = { Icon(Icons.Outlined.ContentCopy, null) }, onClick = { menu = false; copyText(context, out) }) }
                }
            }
        }
    }
    if (selecting) SelectTextSheet(item.text) { selecting = false }
}

@Composable
private fun ItemContent(item: ItemView, retryBlocked: Boolean, actions: TranscriptActions) {
    val tints = LocalTints.current
    when (item.kind) {
        "todo" -> Card {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                Icon(Icons.Outlined.Checklist, null, modifier = Modifier.size(18.dp))
                Text(item.title.ifEmpty { "To-dos" }, style = MaterialTheme.typography.titleSmall)
            }
            item.todos.forEach { todo ->
                Row(horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                    val done = todo.status == "completed"
                    val doing = todo.status == "in_progress"
                    Icon(
                        when { done -> Icons.Filled.CheckCircle; doing -> Icons.Outlined.Timelapse; else -> Icons.Outlined.RadioButtonUnchecked },
                        null,
                        tint = when { done -> tints.success; doing -> MaterialTheme.colorScheme.primary; else -> MaterialTheme.colorScheme.outline },
                        modifier = Modifier.size(18.dp),
                    )
                    Text(
                        todo.text,
                        style = MaterialTheme.typography.bodyMedium,
                        textDecoration = if (done) TextDecoration.LineThrough else null,
                        color = if (done) MaterialTheme.colorScheme.onSurfaceVariant else MaterialTheme.colorScheme.onSurface,
                        fontWeight = if (doing) FontWeight.Medium else FontWeight.Normal,
                    )
                }
            }
        }
        "user" -> Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
            Spacer(Modifier.width(48.dp))
            Surface(color = MaterialTheme.colorScheme.primaryContainer, shape = RoundedCornerShape(20.dp, 20.dp, 6.dp, 20.dp)) {
                Column(Modifier.padding(horizontal = Space.l, vertical = Space.m), horizontalAlignment = Alignment.End, verticalArrangement = Arrangement.spacedBy(Space.s)) {
                    if (item.images.isNotEmpty()) MessageImages(item.images)
                    if (item.text.isNotEmpty()) Text(item.text, style = MaterialTheme.typography.bodyLarge, color = MaterialTheme.colorScheme.onPrimaryContainer)
                    item.attachments.filter { name -> item.images.none { it.name == name } }.forEach { name ->
                        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.xs)) {
                            Icon(Icons.Outlined.AttachFile, null, modifier = Modifier.size(14.dp))
                            Text(name, style = MaterialTheme.typography.labelMedium)
                        }
                    }
                }
            }
        }
        "assistant" -> Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
            MarkdownBlocks(item.blocks, streaming = item.streaming)
            if (item.streaming && item.blocks.lastOrNull()?.kind.let { it == null || it in setOf("code", "table_row", "rule") }) StreamingCaret()
        }
        "reasoning" -> Collapsible("Thinking", Icons.Outlined.Psychology, startCollapsed = item.collapsed) {
            Text(item.text, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        "command" -> Collapsible(
            item.title, Icons.Outlined.Terminal, monospaced = true, status = item.status, startCollapsed = item.collapsed,
            trailing = item.exitCode?.takeIf { it != 0L }?.let { "exit $it" },
        ) {
            val output = item.output
            if (!output.isNullOrEmpty()) {
                val v = rememberScrollState(Int.MAX_VALUE)
                Box(
                    Modifier.fillMaxWidth().heightIn(max = 280.dp)
                        .background(MaterialTheme.colorScheme.surfaceContainerHighest, MaterialTheme.shapes.small)
                        .verticalScroll(v).horizontalScroll(rememberScrollState()).padding(Space.s),
                ) { Text(output, fontFamily = Mono, fontSize = 12.sp, lineHeight = 16.sp, softWrap = false) }
                if (item.truncated) ShowAll { actions.send(Event.ExpandItem(item.id)) }
            } else {
                Text(if (item.status == "running") "Running…" else "No output", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
        "file_change" -> Collapsible(
            item.title, Icons.Outlined.Description, monospaced = true, status = item.status, startCollapsed = item.collapsed,
            trailing = item.diff?.let { "+${it.additions} −${it.deletions}" },
        ) { item.diff?.let { DiffFileBody(it) } }
        "tool" -> Collapsible(item.title, Icons.Outlined.Build, status = item.status, startCollapsed = item.collapsed) {
            if (item.text.isNotEmpty()) Text(item.text, fontFamily = Mono, fontSize = 12.sp)
            item.output?.let {
                HorizontalDivider()
                Text(it, fontFamily = Mono, fontSize = 12.sp, maxLines = if (item.truncated) 40 else Int.MAX_VALUE)
            }
            if (item.truncated) ShowAll { actions.send(Event.ExpandItem(item.id)) }
        }
        "question" -> {
            val waiting = item.status == "waiting"
            StateCard(
                icon = Icons.Outlined.HelpOutline, tint = if (waiting) tints.question else null,
                title = item.title, subtitle = if (waiting) "Waiting for your answer" else item.text.ifEmpty { "Skipped" },
            )
        }
        "approval" -> {
            val waiting = item.status == "waiting"
            StateCard(
                icon = Icons.Filled.PanTool, tint = if (waiting) tints.caution else null,
                title = item.title, subtitle = if (waiting) "Waiting for approval" else "Answered",
            )
        }
        "plan" -> Surface(color = tints.planContainer, shape = MaterialTheme.shapes.medium, modifier = Modifier.fillMaxWidth()) {
            Column(Modifier.padding(Space.l), verticalArrangement = Arrangement.spacedBy(Space.s)) {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                    Icon(Icons.Outlined.Checklist, null, tint = tints.plan)
                    Text("Plan", style = MaterialTheme.typography.titleMedium, color = tints.plan)
                }
                MarkdownBlocks(item.blocks)
                if (item.canImplement) {
                    Button(
                        onClick = { actions.send(Event.ImplementPlan(item.planId)) },
                        colors = ButtonDefaults.buttonColors(containerColor = tints.plan, contentColor = tints.onPlan),
                    ) {
                        Icon(Icons.Filled.PlayArrow, null, modifier = Modifier.size(18.dp)); Spacer(Modifier.width(Space.xs)); Text("Implement plan")
                    }
                }
            }
        }
        "checkpoint" -> CheckpointRow(item, actions)
        "subagent" -> SubagentRow(item, actions)
        "error" -> Surface(color = MaterialTheme.colorScheme.errorContainer, shape = MaterialTheme.shapes.medium, modifier = Modifier.fillMaxWidth()) {
            Column(Modifier.padding(Space.m), verticalArrangement = Arrangement.spacedBy(Space.s)) {
                Row(horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                    Icon(Icons.Filled.Warning, null, tint = MaterialTheme.colorScheme.onErrorContainer, modifier = Modifier.size(18.dp))
                    SelectionContainer { Text(item.title, color = MaterialTheme.colorScheme.onErrorContainer, style = MaterialTheme.typography.bodyMedium) }
                }
                if (item.canRetry) {
                    OutlinedButton(onClick = { actions.send(Event.Retry) }, enabled = !retryBlocked) {
                        Icon(Icons.Outlined.Refresh, null, modifier = Modifier.size(18.dp)); Spacer(Modifier.width(Space.xs)); Text("Try again")
                    }
                }
            }
        }
        else -> Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.s)) {
            Icon(Icons.Outlined.Info, null, tint = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.size(16.dp))
            Text(item.title, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

@Composable
private fun Card(content: @Composable ColumnScope.() -> Unit) {
    Surface(color = MaterialTheme.colorScheme.surfaceContainerLow, shape = MaterialTheme.shapes.medium, modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(Space.m), verticalArrangement = Arrangement.spacedBy(Space.s), content = content)
    }
}

@Composable
private fun StateCard(icon: ImageVector, tint: Color?, title: String, subtitle: String) {
    Surface(
        color = tint?.copy(alpha = 0.12f) ?: MaterialTheme.colorScheme.surfaceContainerLow,
        shape = MaterialTheme.shapes.medium,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Row(Modifier.padding(Space.m), horizontalArrangement = Arrangement.spacedBy(Space.m)) {
            Icon(icon, null, tint = tint ?: MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.size(20.dp))
            Column(verticalArrangement = Arrangement.spacedBy(Space.xxs)) {
                Text(title, style = MaterialTheme.typography.bodyMedium, maxLines = 4, overflow = TextOverflow.Ellipsis)
                Text(subtitle, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
    }
}

@Composable
private fun ShowAll(onClick: () -> Unit) = TextButton(onClick = onClick) { Text("Show all") }

@Composable
private fun TurnFooter(started: Long, durationMs: Long) {
    val date = Date(started)
    val today = DateFormat.getDateInstance().format(date) == DateFormat.getDateInstance().format(Date())
    val time = if (today) DateFormat.getTimeInstance(DateFormat.SHORT).format(date) else DateFormat.getDateTimeInstance(DateFormat.MEDIUM, DateFormat.SHORT).format(date)
    Text("$time · ${duration(durationMs)}", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.outline)
}

/** A row with a disclosure header (commands, file changes, tools, reasoning). */
@Composable
private fun Collapsible(
    title: String,
    icon: ImageVector,
    monospaced: Boolean = false,
    status: String = "",
    startCollapsed: Boolean,
    trailing: String? = null,
    content: @Composable ColumnScope.() -> Unit,
) {
    var expanded by rememberSaveable(title) { mutableStateOf<Boolean?>(null) }
    val open = expanded ?: !startCollapsed
    val tints = LocalTints.current
    val iconColor = when (status) {
        "failed" -> MaterialTheme.colorScheme.error
        "waiting" -> tints.caution
        "running" -> MaterialTheme.colorScheme.primary
        else -> MaterialTheme.colorScheme.onSurfaceVariant
    }
    Surface(color = MaterialTheme.colorScheme.surfaceContainerLow, shape = MaterialTheme.shapes.medium, modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(horizontal = Space.m, vertical = Space.s + Space.xxs), verticalArrangement = Arrangement.spacedBy(Space.s)) {
            Row(
                Modifier.fillMaxWidth().clip(MaterialTheme.shapes.small).clickable { expanded = !open },
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Space.s),
            ) {
                Icon(Icons.Outlined.ChevronRight, null, tint = MaterialTheme.colorScheme.outline, modifier = Modifier.size(16.dp).rotate(if (open) 90f else 0f))
                Icon(icon, null, tint = iconColor, modifier = Modifier.size(18.dp))
                Text(
                    title.ifEmpty { "…" },
                    fontFamily = if (monospaced) Mono else null,
                    style = if (monospaced) MaterialTheme.typography.bodySmall else MaterialTheme.typography.bodyMedium,
                    color = if (status == "interrupted") MaterialTheme.colorScheme.onSurfaceVariant else MaterialTheme.colorScheme.onSurface,
                    maxLines = if (open) 4 else 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f),
                )
                trailing?.let { Text(it, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
                StatusDot(status)
            }
            AnimatedVisibility(open, enter = expandVertically() + fadeIn(), exit = shrinkVertically() + fadeOut()) {
                Column(Modifier.padding(start = Space.l + Space.xxs), verticalArrangement = Arrangement.spacedBy(Space.s), content = content)
            }
        }
    }
}

/** A step's state: a pulsing dot while it runs or waits, "Stopped" when cut short. */
@Composable
private fun StatusDot(status: String) {
    when (status) {
        "running", "waiting" -> {
            val color = if (status == "running") MaterialTheme.colorScheme.primary else LocalTints.current.caution
            val pulse = rememberInfiniteTransition(label = "dot").animateFloat(0.35f, 1f, infiniteRepeatable(tween(700), RepeatMode.Reverse), label = "a")
            Box(Modifier.size(7.dp).alpha(pulse.value).background(color, CircleShape))
        }
        "interrupted" -> Text("Stopped", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

@Composable
private fun StreamingCaret() {
    val pulse = rememberInfiniteTransition(label = "caret").animateFloat(0.3f, 1f, infiniteRepeatable(tween(600), RepeatMode.Reverse), label = "a")
    Box(Modifier.size(8.dp, 16.dp).alpha(pulse.value).background(MaterialTheme.colorScheme.primary, CircleShape))
}

/** Steps of a turn folded under one summary; open while the agent is on them. */
@Composable
private fun StepGroup(summary: String, items: List<ItemView>, live: Boolean, actions: TranscriptActions) {
    var expanded by rememberSaveable(summary + items.first().id) { mutableStateOf<Boolean?>(null) }
    val open = expanded ?: live
    val running = items.any { it.status == "running" || it.status == "waiting" }
    val stopped = items.any { it.status == "interrupted" }
    val failed = items.any { it.status == "failed" }
    val icon = when {
        items.any { it.kind == "file_change" } -> Icons.Outlined.Description
        items.any { it.kind == "command" } -> Icons.Outlined.Terminal
        else -> Icons.Outlined.Build
    }
    Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
        Row(
            Modifier.fillMaxWidth().clip(MaterialTheme.shapes.small).clickable { expanded = !open }.padding(vertical = Space.xs),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.s),
        ) {
            Icon(Icons.Outlined.ChevronRight, null, tint = MaterialTheme.colorScheme.outline, modifier = Modifier.size(16.dp).rotate(if (open) 90f else 0f))
            Icon(icon, null, tint = if (failed) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.size(18.dp))
            Text(summary, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 2, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f))
            StatusDot(if (running) "running" else if (stopped) "interrupted" else "")
        }
        AnimatedVisibility(open, enter = expandVertically() + fadeIn(), exit = shrinkVertically() + fadeOut()) {
            Row(Modifier.height(IntrinsicSize.Min)) {
                Box(Modifier.padding(start = 7.dp).width(1.5.dp).fillMaxHeight().background(MaterialTheme.colorScheme.outlineVariant))
                Column(Modifier.padding(start = Space.m), verticalArrangement = Arrangement.spacedBy(Space.s)) {
                    items.forEach { ItemRow(it, retryBlocked = false, actions = actions) }
                }
            }
        }
        items.lastOrNull()?.takeIf { it.runDurationMs > 0 }?.let { TurnFooter(it.runStartedMs, it.runDurationMs) }
    }
}

@Composable
private fun CheckpointRow(item: ItemView, actions: TranscriptActions) {
    var confirm by remember { mutableStateOf(false) }
    val cp = item.checkpoint
    Surface(color = MaterialTheme.colorScheme.surfaceContainerLow, shape = MaterialTheme.shapes.small, modifier = Modifier.fillMaxWidth()) {
        FlowRow(Modifier.padding(horizontal = Space.m, vertical = Space.xs), verticalArrangement = Arrangement.Center, horizontalArrangement = Arrangement.spacedBy(Space.s)) {
            Row(Modifier.align(Alignment.CenterVertically), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                Icon(Icons.Outlined.Flag, null, tint = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.size(16.dp))
                Text(item.title, style = MaterialTheme.typography.bodyMedium, maxLines = 1)
                cp?.let { Text("+${it.additions} −${it.deletions}", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
            }
            if (cp != null) {
                TextButton(onClick = {
                    actions.send(Event.SelectDiffSource("turn:${cp.turnCount}"))
                    actions.send(Event.ShowPanel("diff"))
                    actions.openDiff()
                }) { Text("View diff") }
                if (cp.canRevert) TextButton(onClick = { confirm = true }) { Text("Revert to here") }
            }
        }
    }
    if (confirm && cp != null) {
        AlertDialog(
            onDismissRequest = { confirm = false },
            title = { Text("Revert files and the conversation to ${cp.label}?") },
            text = { Text("Later changes and messages are discarded.") },
            confirmButton = { TextButton(onClick = { confirm = false; actions.send(Event.Revert(cp.id)) }) { Text("Revert", color = MaterialTheme.colorScheme.error) } },
            dismissButton = { TextButton(onClick = { confirm = false }) { Text("Cancel") } },
        )
    }
}

@Composable
private fun SubagentRow(item: ItemView, actions: TranscriptActions) {
    var expanded by remember { mutableStateOf(false) }
    Card {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.s)) {
            StatusDot(item.status)
            Icon(Icons.Outlined.Groups, null, tint = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.size(18.dp))
            Text(item.title, style = MaterialTheme.typography.bodyMedium, fontWeight = FontWeight.Medium, maxLines = 2, modifier = Modifier.weight(1f))
        }
        if (item.canStop || item.childThreadId.isNotEmpty()) {
            Row(horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                if (item.canStop) OutlinedButton(onClick = { actions.send(Event.StopSubagent(item.id)) }) {
                    Icon(Icons.Outlined.StopCircle, null, modifier = Modifier.size(16.dp)); Spacer(Modifier.width(Space.xs)); Text("Stop")
                }
                if (item.childThreadId.isNotEmpty()) OutlinedButton(onClick = { actions.openThread(item.childThreadId) }) {
                    Icon(Icons.AutoMirrored.Outlined.OpenInNew, null, modifier = Modifier.size(16.dp)); Spacer(Modifier.width(Space.xs)); Text("Open")
                }
            }
        }
        if (item.progress.isNotEmpty()) Text(item.progress, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.primary, maxLines = 1)
        if (item.text.isNotEmpty()) Text(item.text, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = if (expanded) Int.MAX_VALUE else 2, overflow = TextOverflow.Ellipsis)
        item.output?.takeIf { it.isNotEmpty() }?.let {
            HorizontalDivider()
            Text(it, style = MaterialTheme.typography.bodyMedium, maxLines = if (expanded) Int.MAX_VALUE else 4, overflow = TextOverflow.Ellipsis)
        }
        if (item.text.length > 160 || (item.output?.length ?: 0) > 300) TextButton(onClick = { expanded = !expanded }) { Text(if (expanded) "Show less" else "Show more") }
    }
}

@Composable
private fun ParentBanner(parent: ThreadLinkView, canMergeBack: Boolean, actions: TranscriptActions) {
    var confirm by remember { mutableStateOf(false) }
    val subagent = parent.relation == "subagent"
    Surface(color = MaterialTheme.colorScheme.surfaceContainerLow, shape = MaterialTheme.shapes.medium, modifier = Modifier.fillMaxWidth()) {
        Row(Modifier.padding(horizontal = Space.m, vertical = Space.xs), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.s)) {
            Icon(if (subagent) Icons.Outlined.Groups else Icons.AutoMirrored.Outlined.CallSplit, null, tint = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.size(16.dp))
            Text(if (subagent) "Subagent of" else "Forked from", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            Text(
                parent.title, style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.primary, maxLines = 1, overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f).clickable { actions.openThread(parent.threadId) },
            )
            if (canMergeBack) TextButton(onClick = { confirm = true }) {
                Icon(Icons.AutoMirrored.Outlined.Undo, null, modifier = Modifier.size(16.dp)); Spacer(Modifier.width(Space.xs)); Text("Merge back")
            }
        }
    }
    if (confirm) AlertDialog(
        onDismissRequest = { confirm = false },
        title = { Text("Merge this fork back?") },
        text = { Text("“${parent.title}” gets a summary of what was done here with its next message.") },
        confirmButton = { TextButton(onClick = { confirm = false; actions.send(Event.MergeBack) }) { Text("Merge back") } },
        dismissButton = { TextButton(onClick = { confirm = false }) { Text("Cancel") } },
    )
}

@Composable
private fun DashedBubble(content: @Composable ColumnScope.() -> Unit) {
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
        Spacer(Modifier.width(48.dp))
        Column(
            Modifier
                .border(1.dp, MaterialTheme.colorScheme.primary.copy(alpha = 0.35f), RoundedCornerShape(20.dp))
                .background(MaterialTheme.colorScheme.primary.copy(alpha = 0.06f), RoundedCornerShape(20.dp))
                .padding(horizontal = Space.l, vertical = Space.m),
            horizontalAlignment = Alignment.End,
            verticalArrangement = Arrangement.spacedBy(Space.s),
            content = content,
        )
    }
}

@Composable
private fun PendingSendRow(pending: PendingSendView, onCancel: () -> Unit) {
    DashedBubble {
        Text(pending.text, style = MaterialTheme.typography.bodyLarge)
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.xs)) {
            if (pending.state == "waiting") {
                Icon(Icons.Outlined.Schedule, null, modifier = Modifier.size(14.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
                Text("Sends when connected", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                TextButton(onClick = onCancel) { Text("Cancel") }
            } else {
                CircularProgressIndicator(Modifier.size(12.dp), strokeWidth = 1.5.dp)
                Text("Sending…", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
    }
}

@Composable
private fun QueuedSection(queued: List<QueuedMessageView>, held: Boolean, actions: TranscriptActions) {
    var editing by remember { mutableStateOf<QueuedMessageView?>(null) }
    Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.End, verticalArrangement = Arrangement.spacedBy(Space.s)) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.xs)) {
            Icon(if (held) Icons.Outlined.Pause else Icons.Outlined.Schedule, null, modifier = Modifier.size(14.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
            Text(
                if (held) "Queue paused after the stop" else if (queued.size == 1) "Queued for after this turn" else "${queued.size} queued for after this turn",
                style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (held) TextButton(onClick = { actions.send(Event.ResumeQueue) }) { Text("Resume") }
        }
        queued.forEach { item ->
            var menu by remember { mutableStateOf(false) }
            Row(verticalAlignment = Alignment.Top) {
                Box(Modifier.weight(1f, fill = false)) { DashedBubble { Text(item.text, maxLines = 6, overflow = TextOverflow.Ellipsis) } }
                Box {
                    IconButton(onClick = { menu = true }) { Icon(Icons.Outlined.MoreVert, "Queued message actions") }
                    DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                        if (item.canSteer) DropdownMenuItem(text = { Text("Send now") }, onClick = { menu = false; actions.send(Event.SteerQueued(item.runId)) })
                        DropdownMenuItem(text = { Text("Edit") }, onClick = { menu = false; editing = item })
                        if (!item.first) DropdownMenuItem(text = { Text("Move up") }, onClick = { menu = false; actions.send(Event.MoveQueued(item.runId, -1)) })
                        if (!item.last) DropdownMenuItem(text = { Text("Move down") }, onClick = { menu = false; actions.send(Event.MoveQueued(item.runId, 1)) })
                        HorizontalDivider()
                        DropdownMenuItem(text = { Text("Remove", color = MaterialTheme.colorScheme.error) }, onClick = { menu = false; actions.send(Event.CancelQueued(item.runId)) })
                    }
                }
            }
        }
    }
    editing?.let { item ->
        var text by remember(item.runId) { mutableStateOf(item.text) }
        AlertDialog(
            onDismissRequest = { editing = null },
            title = { Text("Edit queued message") },
            text = { OutlinedTextField(text, { text = it }, minLines = 2, modifier = Modifier.fillMaxWidth()) },
            confirmButton = { TextButton(onClick = { editing = null; actions.send(Event.EditQueued(item.runId, text)) }) { Text("Save") } },
            dismissButton = { TextButton(onClick = { editing = null }) { Text("Cancel") } },
        )
    }
}

/** "Working · 42 s" while a turn runs, with what the agent is on. */
@Composable
private fun WorkingRow(status: String, since: Long, doing: String?) {
    Row(Modifier.padding(vertical = Space.xs), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.s)) {
        if (status == "approval") {
            Icon(Icons.Filled.PanTool, null, tint = LocalTints.current.caution, modifier = Modifier.size(18.dp))
            Text("Waiting for you", color = MaterialTheme.colorScheme.onSurfaceVariant)
        } else {
            val pulse = rememberInfiniteTransition(label = "work").animateFloat(0.4f, 1f, infiniteRepeatable(tween(900), RepeatMode.Reverse), label = "a")
            Icon(Icons.Outlined.AutoAwesome, null, tint = MaterialTheme.colorScheme.primary, modifier = Modifier.size(18.dp).alpha(pulse.value))
            Column(verticalArrangement = Arrangement.spacedBy(Space.xxs)) {
                var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
                LaunchedEffect(since) { while (true) { now = System.currentTimeMillis(); delay(1000) } }
                Text(
                    if (since > 0) "Working · ${duration(maxOf(0, now - since))}" else "Working…",
                    style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                doing?.let { Text(it, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.outline, maxLines = 1, overflow = TextOverflow.Ellipsis) }
            }
        }
    }
}

@Composable
private fun StoppedRow(retryBlocked: Boolean, onRetry: () -> Unit) {
    Surface(color = MaterialTheme.colorScheme.surfaceContainerLow, shape = MaterialTheme.shapes.small, modifier = Modifier.fillMaxWidth()) {
        Row(Modifier.padding(start = Space.m), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.s)) {
            Icon(Icons.Outlined.StopCircle, null, tint = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.size(18.dp))
            Text("Stopped", color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.weight(1f))
            TextButton(onClick = onRetry, enabled = !retryBlocked) { Icon(Icons.Outlined.Refresh, null, modifier = Modifier.size(16.dp)); Spacer(Modifier.width(Space.xs)); Text("Retry") }
        }
    }
}

@Composable
private fun EmptyThreadHint(thread: ThreadPageView) {
    Column(Modifier.fillMaxWidth().padding(vertical = 64.dp), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(Space.m)) {
        Icon(Icons.Outlined.ChatBubbleOutline, null, tint = MaterialTheme.colorScheme.outline, modifier = Modifier.size(40.dp))
        Text(thread.projectTitle.ifEmpty { "New thread" }, style = MaterialTheme.typography.titleMedium)
        Row(horizontalArrangement = Arrangement.spacedBy(Space.m)) {
            thread.branch?.let { Text(it, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant) }
            if (thread.modelLabel.isNotEmpty()) Text(thread.modelLabel, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            if (thread.interactionMode == "plan") Text("Plan first", style = MaterialTheme.typography.labelMedium, color = LocalTints.current.plan)
        }
    }
}

/** A message's images as thumbnails; a tap shows one full size. */
@Composable
private fun MessageImages(images: List<ImageView>) {
    var shown by remember { mutableStateOf<ImageView?>(null) }
    Row(horizontalArrangement = Arrangement.spacedBy(Space.s)) {
        images.forEach { image ->
            AsyncImage(
                model = image.url, contentDescription = image.name, contentScale = ContentScale.Crop,
                modifier = Modifier.size(96.dp).clip(MaterialTheme.shapes.small).clickable { shown = image },
            )
        }
    }
    shown?.let { image ->
        ModalBottomSheet(onDismissRequest = { shown = null }) {
            AsyncImage(model = image.url, contentDescription = image.name, contentScale = ContentScale.Fit, modifier = Modifier.fillMaxWidth().padding(Space.l))
            Spacer(Modifier.height(Space.l))
        }
    }
}

/** The text of a message, selectable a word at a time. */
@Composable
private fun SelectTextSheet(text: String, onDismiss: () -> Unit) {
    ModalBottomSheet(onDismissRequest = onDismiss) {
        SelectionContainer(Modifier.verticalScroll(rememberScrollState()).padding(horizontal = Space.xl, vertical = Space.s)) {
            Text(text, style = MaterialTheme.typography.bodyLarge)
        }
        Spacer(Modifier.height(Space.xl))
    }
}

/** A file's diff: one line each, green and red backgrounds; long-press asks about a line. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun DiffFileBody(file: DiffFileView, onLine: ((String) -> Unit)? = null) {
    val tints = LocalTints.current
    Column(
        Modifier.fillMaxWidth().clip(MaterialTheme.shapes.small).background(MaterialTheme.colorScheme.surfaceContainerHighest)
            .horizontalScroll(rememberScrollState()),
    ) {
        file.hunks.forEach { hunk ->
            Text(hunk.header, fontFamily = Mono, fontSize = 11.sp, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(horizontal = Space.s, vertical = Space.xxs))
            hunk.lines.forEach { line ->
                val bg = when (line.kind) {
                    "add" -> tints.success.copy(alpha = 0.16f)
                    "del" -> MaterialTheme.colorScheme.error.copy(alpha = 0.14f)
                    else -> Color.Transparent
                }
                val mark = when (line.kind) { "add" -> "+"; "del" -> "−"; else -> " " }
                val number = (line.newLine ?: line.oldLine)?.toString()?.padStart(4) ?: "    "
                Text(
                    "$number $mark ${line.text}",
                    fontFamily = Mono, fontSize = 12.sp, softWrap = false,
                    modifier = Modifier.background(bg).padding(horizontal = Space.s).then(
                        if (onLine != null && line.kind != "note") Modifier.combinedClickable(onClick = {}, onLongClick = {
                            // Quoted into the thread's message.
                            val place = (line.newLine ?: line.oldLine)?.let { "${file.path}:$it" } ?: file.path
                            val marker = when (line.kind) { "add" -> "+"; "del" -> "-"; else -> " " }
                            onLine("`$place`\n```\n$marker${line.text}\n```\n")
                        }) else Modifier,
                    ),
                )
            }
        }
    }
}
