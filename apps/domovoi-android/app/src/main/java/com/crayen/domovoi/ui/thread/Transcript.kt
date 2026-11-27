package com.crayen.domovoi.ui.thread

import android.content.Intent
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
import androidx.compose.foundation.relocation.BringIntoViewRequester
import androidx.compose.foundation.relocation.bringIntoViewRequester
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
import androidx.compose.material.icons.outlined.Extension
import androidx.compose.material.icons.outlined.FolderOpen
import androidx.compose.material.icons.outlined.Flag
import androidx.compose.material.icons.outlined.GraphicEq
import androidx.compose.material.icons.outlined.Groups
import androidx.compose.material.icons.outlined.HelpOutline
import androidx.compose.material.icons.outlined.Image
import androidx.compose.material.icons.outlined.Info
import androidx.compose.material.icons.outlined.KeyboardArrowDown
import androidx.compose.material.icons.outlined.KeyboardArrowUp
import androidx.compose.material.icons.outlined.Language
import androidx.compose.material.icons.outlined.Link
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material.icons.outlined.Pause
import androidx.compose.material.icons.outlined.Psychology
import androidx.compose.material.icons.outlined.RadioButtonUnchecked
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.Save
import androidx.compose.material.icons.outlined.Schedule
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material.icons.outlined.Share
import androidx.compose.material.icons.outlined.Download
import androidx.compose.material.icons.outlined.SmartToy
import androidx.compose.material.icons.outlined.StopCircle
import androidx.compose.material.icons.outlined.Terminal
import androidx.compose.material.icons.outlined.Timelapse
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Badge
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SmallFloatingActionButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.withFrameNanos
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
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.sp
import coil3.compose.AsyncImage
import com.crayen.domovoi.app.BackgroundTaskView
import com.crayen.domovoi.app.DiffFileView
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.app.FieldView
import com.crayen.domovoi.app.ImageView
import com.crayen.domovoi.app.ItemView
import com.crayen.domovoi.app.LinkView
import com.crayen.domovoi.app.TodoView
import com.crayen.domovoi.app.PendingSendView
import com.crayen.domovoi.app.QueuedMessageView
import com.crayen.domovoi.app.ThreadLinkView
import com.crayen.domovoi.app.ThreadPageView
import com.crayen.domovoi.app.WorkingActivityView
import com.crayen.domovoi.ui.common.ArtifactCard
import com.crayen.domovoi.ui.common.isHtmlPath
import com.crayen.domovoi.ui.common.LocalCore
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

/** What transcript rows can ask: the core's update, opening a thread, the diff, or an artifact. */
@Immutable
class TranscriptActions(
    val send: (Event) -> Unit,
    val openThread: (String) -> Unit,
    val openDiff: () -> Unit,
    val openArtifact: (artifactId: String, version: UInt?, codeMode: Boolean) -> Unit = { _, _, _ -> },
    val openWorkspaceArtifact: (path: String) -> Unit = {},
    /** An HTML page an edit wrote, read from the workspace (`OpenArtifactFile`). */
    val previewFile: (path: String) -> Unit = {},
)

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
fun Transcript(thread: ThreadPageView, retryBlocked: Boolean, actions: TranscriptActions, modifier: Modifier = Modifier, contentPadding: PaddingValues = PaddingValues(), artifactJump: String? = null) {
    val state = rememberLazyListState()
    val scope = rememberCoroutineScope()
    val lines = remember(thread.items) { lines(thread.items) }
    val atEnd by remember { derivedStateOf { state.firstVisibleItemIndex == 0 && state.firstVisibleItemScrollOffset < 80 } }
    var unseen by remember(thread.id) { mutableStateOf(false) }
    val last = thread.items.lastOrNull()
    val signature = Triple(thread.items.size, thread.pendingSends.size, thread.working) to
        Triple(last?.text, last?.output, last?.status)
    LaunchedEffect(signature) {
        if (atEnd) state.scrollToItem(0) else unseen = true
    }
    LaunchedEffect(atEnd) { if (atEnd) unseen = false }
    // The step the working row points at, once tapped (again: a new count).
    var step by remember(thread.id) { mutableStateOf<Pair<String, Int>?>(null) }
    LaunchedEffect(atEnd) { if (atEnd) step = null }
    // Find: bring the active match into view.
    val active = thread.find?.activeItemId ?: artifactJump ?: step?.first
    suspend fun scrollTo(id: String?) {
        if (id == null) return
        val index = state.layoutInfo.totalItemsCount
        val target = lines.indexOfFirst { l -> l is Line.Item && l.item.id == id || l is Line.Group && l.items.any { it.id == id } }
        if (target >= 0 && index > 0) state.animateScrollToItem(tailCount(thread) + (lines.size - 1 - target))
    }
    LaunchedEffect(active, step) { scrollTo(active) }

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
            if (thread.limitResets.isNotEmpty()) item("limit") { UsageLimitBanner(thread.limitResets, thread.limitPlan, actions) }
            if (thread.backgroundTasks.isNotEmpty()) {
                item("background-activity") {
                    Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.TopCenter) {
                        Column(Modifier.widthIn(max = Space.column).fillMaxWidth()) {
                            BackgroundActivity(thread.backgroundTasks, actions)
                        }
                    }
                }
            }
            if (thread.working) {
                item("working") {
                    WorkingRow(thread.status, thread.workingSinceMs, thread.workingActivity, thread.background.ifEmpty { liveSummary(lines) }) { id ->
                        step = id to (step?.second ?: 0) + 1
                    }
                }
            } else if (thread.status == "interrupted" && thread.pendingSends.isEmpty() && thread.items.isNotEmpty()) {
                item("stopped") { StoppedRow(retryBlocked) { actions.send(Event.Retry) } }
            }
            if (thread.queued.isNotEmpty()) item("queued") { QueuedSection(thread.queued, thread.queueHeld, actions) }
            for (pending in thread.pendingSends.asReversed()) item("pending-${pending.id}") { PendingSendRow(pending) { actions.send(Event.CancelSend(pending.id)) } }
            for (index in lines.indices.reversed()) {
                val line = lines[index]
                item(line.key, contentType = (line as? Line.Item)?.item?.kind ?: "group") {
                    Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.TopCenter) {
                        Column(Modifier.widthIn(max = Space.column).fillMaxWidth()) {
                            when (line) {
                                is Line.Item -> ItemRow(line.item, retryBlocked, actions)
                                is Line.Group -> StepGroup(line.summary, line.items, live = thread.working && index == lines.lastIndex, active, actions)
                            }
                        }
                    }
                }
            }
            if (thread.hasEarlier) {
                item("earlier") {
                    Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
                        TextButton(onClick = { actions.send(Event.LoadEarlier) }, enabled = !thread.loadingEarlier) {
                            Text(if (thread.loadingEarlier) "Loading earlier messages…" else "Show earlier messages")
                        }
                    }
                }
            }
            if (thread.loading && thread.items.isEmpty()) {
                item("loading") { Box(Modifier.fillMaxWidth().padding(Space.xxl), contentAlignment = Alignment.Center) { CircularProgressIndicator() } }
            } else if (thread.items.isEmpty() && thread.pendingSends.isEmpty() && thread.queued.isEmpty() && !thread.working && thread.backgroundTasks.isEmpty()) {
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
    1 + (if (t.limitResets.isNotEmpty()) 1 else 0) + (if (t.backgroundTasks.isNotEmpty()) 1 else 0) +
        (if (t.working || (t.status == "interrupted" && t.pendingSends.isEmpty() && t.items.isNotEmpty())) 1 else 0) +
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
    val hasMenu = item.kind in setOf("user", "assistant", "plan", "error", "command", "tool")
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
                    DropdownMenuItem(text = { Text(if (item.kind == "user") "Copy" else "Copy Markdown") }, leadingIcon = { Icon(Icons.Outlined.ContentCopy, null) }, onClick = { menu = false; copyText(context, item.text) })
                    DropdownMenuItem(text = { Text("Select text") }, leadingIcon = { Icon(Icons.Outlined.Description, null) }, onClick = { menu = false; selecting = true })
                    if (item.kind == "user") DropdownMenuItem(text = { Text("Edit and send again") }, leadingIcon = { Icon(Icons.Outlined.Edit, null) }, onClick = { menu = false; actions.send(Event.EditMessage(item.text)) })
                    // The last reply of a finished turn: a new thread that goes on from here (it opens once created).
                    if (item.canForkHere) DropdownMenuItem(text = { Text("Fork from here") }, leadingIcon = { Icon(Icons.AutoMirrored.Outlined.CallSplit, null) }, onClick = { menu = false; actions.send(Event.ForkFromItem(item.id)) })
                }
                "error" -> DropdownMenuItem(text = { Text("Copy error") }, leadingIcon = { Icon(Icons.Outlined.ContentCopy, null) }, onClick = { menu = false; copyText(context, item.title) })
                "command", "tool" -> {
                    // The command as run (or the tool's input) when the line shows something shorter.
                    val copied = item.text.ifEmpty { item.title }
                    if (copied.isNotEmpty()) DropdownMenuItem(text = { Text(if (item.kind == "command") "Copy command" else "Copy") }, leadingIcon = { Icon(Icons.Outlined.ContentCopy, null) }, onClick = { menu = false; copyText(context, copied) })
                    item.output?.takeIf { it.isNotEmpty() }?.let { out -> DropdownMenuItem(text = { Text("Copy output") }, leadingIcon = { Icon(Icons.Outlined.ContentCopy, null) }, onClick = { menu = false; copyText(context, out) }) }
                }
            }
        }
    }
    if (selecting) SelectTextSheet(if (item.blocks.isEmpty()) item.text else item.blocks.joinToString("\n\n") { block ->
            when {
                block.cells.isNotEmpty() -> block.cells.mapIndexed { index, source ->
                    val runs = block.cellRuns.getOrNull(index).orEmpty()
                    if (runs.isEmpty()) source else runs.joinToString("") { it.text }
                }.joinToString(" | ")
                block.runs.isNotEmpty() -> block.runs.joinToString("") { it.text }
                else -> block.text
            }
        }) { selecting = false }
}

@Composable
internal fun TodoSteps(todos: List<TodoView>) {
    val tints = LocalTints.current
    Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
        todos.forEach { todo ->
            Row(
                horizontalArrangement = Arrangement.spacedBy(Space.s),
                modifier = Modifier.semantics(mergeDescendants = true) {
                    stateDescription = todo.status.replace('_', ' ')
                },
            ) {
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
}

@Composable
private fun ItemContent(item: ItemView, retryBlocked: Boolean, actions: TranscriptActions) {
    val tints = LocalTints.current
    when (item.kind) {
        "todo" -> Collapsible(
            item.title.ifEmpty { "To-dos" }, Icons.Outlined.Checklist, startCollapsed = item.collapsed,
            trailing = item.progress.ifEmpty { null },
        ) {
            TodoSteps(item.todos)
        }
        "user" -> Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
            Spacer(Modifier.width(48.dp))
            Surface(color = MaterialTheme.colorScheme.primaryContainer, shape = RoundedCornerShape(20.dp, 20.dp, 6.dp, 20.dp)) {
                Column(Modifier.padding(horizontal = Space.l, vertical = Space.m), horizontalAlignment = Alignment.End, verticalArrangement = Arrangement.spacedBy(Space.s)) {
                    item.issueCard?.let { issue ->
                        val uri = androidx.compose.ui.platform.LocalUriHandler.current
                        androidx.compose.material3.TextButton(onClick = { uri.openUri(issue.issueUrl) }) { Text(issue.issueLabel) }
                    }
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
            MarkdownBlocks(
                item.blocks,
                streaming = item.streaming,
                onOpenArtifact = actions.openArtifact,
                onOpenWorkspaceArtifact = actions.openWorkspaceArtifact,
            )
            if (item.streaming && item.blocks.lastOrNull()?.kind.let { it == null || it in setOf("code", "table_row", "rule") }) StreamingCaret()
        }
        "artifact" -> {
            val aid = item.artifactId
            val version = item.artifactVersion.takeIf { it > 0u } ?: 1u
            ArtifactCard(
                title = item.artifactTitle.ifEmpty { item.title },
                kind = item.artifactKind.ifEmpty { "html" },
                version = version,
                streaming = item.streaming || item.status == "running",
                hostedUrl = item.artifactHostedUrl,
                onOpen = {
                    if (aid != null) actions.openArtifact(aid, item.artifactVersion.takeIf { it > 0u }, false)
                },
                onViewCode = if (aid != null) {
                    { actions.openArtifact(aid, item.artifactVersion.takeIf { it > 0u }, true) }
                } else null,
            )
        }
        "reasoning" -> Collapsible("Thinking", Icons.Outlined.Psychology, startCollapsed = item.collapsed) {
            Text(item.text, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        "command", "file_change", "tool" -> ToolRow(item, actions)
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
        "plan" -> PlanCard(item, actions)
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
        // A compaction or handoff: its summary under the title.
        "notice" -> if (item.text.isNotEmpty()) {
            Collapsible(item.title, Icons.Outlined.Info, startCollapsed = item.collapsed) {
                SelectionContainer { Text(item.text, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant) }
            }
        } else NoticeLine(item.title)
        else -> NoticeLine(item.title)
    }
}

@Composable
private fun NoticeLine(title: String) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.s)) {
        Icon(Icons.Outlined.Info, null, tint = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.size(16.dp))
        Text(title, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

/**
 * A plan the agent proposed: folded to its first lines when long, with
 * copy, save to the workspace and share; dimmed once a later plan replaced it.
 */
@Composable
private fun PlanCard(item: ItemView, actions: TranscriptActions) {
    val tints = LocalTints.current
    val context = LocalContext.current
    var expanded by rememberSaveable(item.id) { mutableStateOf(false) }
    var menu by remember { mutableStateOf(false) }
    var saving by remember { mutableStateOf(false) }
    val long = item.text.lines().size > 8 || item.text.length > 640
    Surface(
        color = tints.planContainer, shape = MaterialTheme.shapes.medium,
        modifier = Modifier.fillMaxWidth().alpha(if (item.superseded) 0.55f else 1f),
    ) {
        Column(Modifier.padding(start = Space.l, end = Space.xs, top = Space.xs, bottom = Space.l), verticalArrangement = Arrangement.spacedBy(Space.s)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                Icon(Icons.Outlined.Checklist, null, tint = tints.plan)
                Text("Plan", style = MaterialTheme.typography.titleMedium, color = tints.plan)
                if (item.superseded) {
                    Surface(color = MaterialTheme.colorScheme.surfaceContainerHighest, shape = MaterialTheme.shapes.small) {
                        Text("Superseded", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(horizontal = Space.s, vertical = Space.xxs))
                    }
                }
                Spacer(Modifier.weight(1f))
                Box {
                    IconButton(onClick = { menu = true }) { Icon(Icons.Outlined.MoreVert, "Plan actions", tint = tints.plan) }
                    DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                        DropdownMenuItem(text = { Text("Copy") }, leadingIcon = { Icon(Icons.Outlined.ContentCopy, null) }, onClick = { menu = false; copyText(context, item.text) })
                        DropdownMenuItem(text = { Text("Save to workspace…") }, leadingIcon = { Icon(Icons.Outlined.Save, null) }, onClick = { menu = false; saving = true })
                        DropdownMenuItem(text = { Text("Share markdown") }, leadingIcon = { Icon(Icons.Outlined.Share, null) }, onClick = {
                            menu = false
                            val share = Intent(Intent.ACTION_SEND).apply {
                                type = "text/markdown"
                                putExtra(Intent.EXTRA_SUBJECT, "Plan")
                                putExtra(Intent.EXTRA_TEXT, item.text)
                            }
                            context.startActivity(Intent.createChooser(share, "Share plan"))
                        })
                    }
                }
            }
            Column(
                Modifier.padding(end = Space.m - Space.xs).then(if (long && !expanded) Modifier.heightIn(max = 200.dp).clipToBounds() else Modifier),
                verticalArrangement = Arrangement.spacedBy(Space.s),
            ) { MarkdownBlocks(item.blocks) }
            if (long) {
                TextButton(onClick = { expanded = !expanded }) { Text(if (expanded) "Collapse" else "Show full plan", color = tints.plan) }
            }
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
    if (saving) SavePlanDialog(onDismiss = { saving = false }) { path ->
        saving = false
        actions.send(Event.SavePlan(item.id, path))
    }
}

/** Where in the workspace a plan goes; empty leaves it to the server. */
@Composable
private fun SavePlanDialog(onDismiss: () -> Unit, save: (String) -> Unit) {
    var path by rememberSaveable { mutableStateOf("") }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Save plan to workspace") },
        text = {
            OutlinedTextField(
                value = path, onValueChange = { path = it },
                label = { Text("Path in the workspace") },
                placeholder = { Text("plans/<name>.md") },
                supportingText = { Text("Leave empty for the default place.") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
        },
        confirmButton = { TextButton(onClick = { save(path.trim()) }) { Text("Save") } },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
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

/** A step's icon by what it did (`ItemView.icon`), or by its kind when the core gave none. */
private fun stepIcon(icon: String, kind: String): ImageVector = when (icon) {
    "read" -> Icons.Outlined.Description
    "image" -> Icons.Outlined.Image
    "search" -> Icons.Outlined.Search
    "list" -> Icons.Outlined.FolderOpen
    "command" -> Icons.Outlined.Terminal
    "edit" -> Icons.Outlined.Edit
    "web" -> Icons.Outlined.Language
    "fetch" -> Icons.Outlined.Link
    "mcp" -> Icons.Outlined.Extension
    "skill" -> Icons.Outlined.AutoAwesome
    "agent" -> Icons.Outlined.SmartToy
    "question" -> Icons.Outlined.HelpOutline
    "wait" -> Icons.Outlined.Schedule
    "tool" -> Icons.Outlined.Build
    else -> when (kind) {
        "command" -> Icons.Outlined.Terminal
        "file_change" -> Icons.Outlined.Edit
        else -> Icons.Outlined.Build
    }
}

/** Where a step's body, images and fields start: past the icon and its gap. */
private val StepIndent = 18.dp + Space.s

/**
 * A command, tool call or file change: one line of icon, verb, what it worked
 * on, detail and a trailing mark; when the core says there is more, a tap
 * opens it (the command as run, arguments, links, a file, output, a diff).
 * Images the step viewed show under the line even while it is closed.
 */
@Composable
private fun ToolRow(item: ItemView, actions: TranscriptActions) {
    var expanded by rememberSaveable(item.id) { mutableStateOf<Boolean?>(null) }
    val open = item.expandable && (expanded ?: !item.collapsed)
    val tints = LocalTints.current
    val failed = item.status == "failed" || item.badgeTone == "failed"
    val iconColor = when {
        failed -> MaterialTheme.colorScheme.error
        item.status == "running" -> MaterialTheme.colorScheme.primary
        item.status == "waiting" -> tints.caution
        else -> MaterialTheme.colorScheme.onSurfaceVariant
    }
    val dim = item.status == "interrupted" || item.status == "cancelled"
    val line = buildAnnotatedString {
        if (item.verb.isNotEmpty()) withStyle(SpanStyle(fontWeight = FontWeight.Medium)) { append(item.verb) }
        if (item.title.isNotEmpty()) {
            if (length > 0) append(' ')
            withStyle(if (item.mono) SpanStyle(fontFamily = Mono, fontSize = 13.sp) else SpanStyle()) { append(item.title) }
        }
        if (length == 0) append("…")
        if (item.subtitle.isNotEmpty()) {
            append("  ")
            withStyle(SpanStyle(fontSize = 12.sp, color = MaterialTheme.colorScheme.onSurfaceVariant)) { append(item.subtitle) }
        }
    }
    Surface(color = MaterialTheme.colorScheme.surfaceContainerLow, shape = MaterialTheme.shapes.medium, modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(horizontal = Space.m, vertical = Space.s + Space.xxs), verticalArrangement = Arrangement.spacedBy(Space.s)) {
            Row(
                Modifier.fillMaxWidth().clip(MaterialTheme.shapes.small).then(
                    if (item.expandable) Modifier.clickable(onClickLabel = if (open) "Close" else "Open") { expanded = !open } else Modifier,
                ),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Space.s),
            ) {
                Icon(stepIcon(item.icon, item.kind), null, tint = iconColor, modifier = Modifier.size(18.dp))
                Text(
                    line,
                    style = MaterialTheme.typography.bodyMedium,
                    color = if (dim) MaterialTheme.colorScheme.onSurfaceVariant else MaterialTheme.colorScheme.onSurface,
                    maxLines = if (open) 4 else 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f),
                )
                val fileArtifactId = item.artifactId
                if (item.kind == "file_change" && fileArtifactId != null && !item.streaming) {
                    TextButton(
                        onClick = { actions.openArtifact(fileArtifactId, item.artifactVersion.takeIf { it > 0u }, false) },
                        contentPadding = PaddingValues(horizontal = Space.s, vertical = 0.dp),
                        modifier = Modifier.height(28.dp),
                    ) {
                        Text("Preview", style = MaterialTheme.typography.labelSmall)
                    }
                } else if (item.kind == "file_change" && !item.streaming) {
                    // Web pages the edit wrote, drawn from the workspace.
                    val pages = item.artifactPaths.filter(::isHtmlPath)
                    if (pages.isNotEmpty()) {
                        var choosing by remember { mutableStateOf(false) }
                        Box {
                            TextButton(
                                onClick = { if (pages.size == 1) actions.previewFile(pages.first()) else choosing = true },
                                contentPadding = PaddingValues(horizontal = Space.s, vertical = 0.dp),
                                modifier = Modifier.height(28.dp),
                            ) {
                                Text("Open preview", style = MaterialTheme.typography.labelSmall)
                            }
                            DropdownMenu(expanded = choosing, onDismissRequest = { choosing = false }) {
                                pages.forEach { path ->
                                    DropdownMenuItem(text = { Text(path.substringAfterLast('/'), fontFamily = Mono) }, onClick = { choosing = false; actions.previewFile(path) })
                                }
                            }
                        }
                    }
                }
                if (item.badge.isNotEmpty()) {
                    Text(
                        item.badge, style = MaterialTheme.typography.labelSmall, maxLines = 1,
                        color = if (item.badgeTone == "failed") MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                } else if (item.status == "interrupted") StatusDot(item.status)
                // A failed exit is in the badge ("exit 101"); a clean one gets a quiet check.
                if (item.kind == "command" && item.status != "running" && item.exitCode == 0L) {
                    Icon(Icons.Outlined.Check, "Exited with code 0", tint = tints.success, modifier = Modifier.size(14.dp))
                }
                if (item.status == "running" || item.status == "waiting") StatusDot(item.status)
                if (item.expandable) Icon(Icons.Outlined.ChevronRight, null, tint = MaterialTheme.colorScheme.outline, modifier = Modifier.size(16.dp).rotate(if (open) 90f else 0f))
            }
            if (item.images.isNotEmpty()) Box(Modifier.padding(start = StepIndent)) { MessageImages(item.images, size = 72.dp) }
            AnimatedVisibility(open, enter = expandVertically() + fadeIn(), exit = shrinkVertically() + fadeOut()) {
                Column(Modifier.padding(start = StepIndent), verticalArrangement = Arrangement.spacedBy(Space.s)) {
                    ToolBody(item, failed, actions)
                }
            }
        }
    }
}

/** An opened step: the command as run (or an edit's error), arguments, links, a file, output, a diff. */
@Composable
private fun ToolBody(item: ItemView, failed: Boolean, actions: TranscriptActions) {
    if (item.text.isNotEmpty()) {
        SelectionContainer {
            Text(
                item.text, fontFamily = Mono, fontSize = 12.sp, lineHeight = 16.sp,
                color = if (item.kind == "file_change" && failed) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface,
            )
        }
    }
    if (item.fields.isNotEmpty()) ToolFields(item.fields)
    if (item.links.isNotEmpty()) ToolLinks(item.links)
    if (item.blocks.isNotEmpty()) MarkdownBlocks(item.blocks)
    val output = item.output?.takeIf { it.isNotEmpty() }
    when {
        output != null && item.kind == "command" -> {
            val v = rememberScrollState(Int.MAX_VALUE)
            Box(
                Modifier.fillMaxWidth().heightIn(max = 280.dp)
                    .background(MaterialTheme.colorScheme.surfaceContainerHighest, MaterialTheme.shapes.small)
                    .verticalScroll(v).horizontalScroll(rememberScrollState()).padding(Space.s),
            ) { Text(output, fontFamily = Mono, fontSize = 12.sp, lineHeight = 16.sp, softWrap = false) }
        }
        output != null -> Text(
            output, fontFamily = Mono, fontSize = 12.sp, lineHeight = 16.sp,
            maxLines = if (item.truncated) 40 else Int.MAX_VALUE, overflow = TextOverflow.Ellipsis,
        )
        // A command whose line opens but printed nothing (yet).
        item.kind == "command" && item.blocks.isEmpty() && item.links.isEmpty() && item.images.isEmpty() -> Text(
            if (item.status == "running") "Running…" else "No output",
            style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
    item.diff?.takeIf { it.hunks.isNotEmpty() }?.let { DiffFileBody(it) }
    if (item.truncated) ShowAll { actions.send(Event.ExpandItem(item.id)) }
}

/** A tool's arguments: names in one column, values in the other. */
@Composable
private fun ToolFields(fields: List<FieldView>) {
    Column(verticalArrangement = Arrangement.spacedBy(Space.xs)) {
        fields.forEach { field ->
            Row(horizontalArrangement = Arrangement.spacedBy(Space.m)) {
                Text(
                    field.label, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.width(96.dp).padding(top = 1.dp),
                )
                SelectionContainer(Modifier.weight(1f)) {
                    if (field.mono) Text(field.value, fontFamily = Mono, fontSize = 12.sp, lineHeight = 16.sp)
                    else Text(field.value, style = MaterialTheme.typography.bodySmall)
                }
            }
        }
    }
}

/** A web search's results; a tap opens one. */
@Composable
private fun ToolLinks(links: List<LinkView>) {
    val uri = LocalUriHandler.current
    Column {
        links.forEach { link ->
            Column(
                Modifier.fillMaxWidth().clip(MaterialTheme.shapes.small)
                    .clickable(enabled = link.url.isNotEmpty(), onClickLabel = "Open") { runCatching { uri.openUri(link.url) } }
                    .padding(vertical = Space.xs),
                verticalArrangement = Arrangement.spacedBy(Space.xxs),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                    Text(
                        link.title.ifEmpty { link.url }, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.primary,
                        maxLines = 2, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f, fill = false),
                    )
                    if (link.host.isNotEmpty()) Text(link.host, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1)
                }
                if (link.snippet.isNotEmpty()) Text(link.snippet, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 2, overflow = TextOverflow.Ellipsis)
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
private fun StepGroup(summary: String, items: List<ItemView>, live: Boolean, active: String?, actions: TranscriptActions) {
    // Keyed by the first step only: the summary changes as the steps run.
    var expanded by rememberSaveable(items.first().id) { mutableStateOf<Boolean?>(null) }
    val open = items.any { it.id == active } || (expanded ?: live)
    val running = items.any { it.status == "running" || it.status == "waiting" }
    val stopped = items.any { it.status == "interrupted" }
    val failed = items.any { it.status == "failed" || it.badgeTone == "failed" }
    val icons = items.map { it.icon.ifEmpty { if (it.kind == "file_change") "edit" else if (it.kind == "command") "command" else "" } }
    val icon = when {
        "edit" in icons -> Icons.Outlined.Edit
        "command" in icons -> Icons.Outlined.Terminal
        icons.any { it == "read" || it == "search" || it == "list" } -> Icons.Outlined.Search
        else -> Icons.Outlined.Build
    }
    val images = items.first().groupImages
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
        // The images the steps viewed, while the steps (which show them too) are folded.
        AnimatedVisibility(!open && images.isNotEmpty(), enter = expandVertically() + fadeIn(), exit = shrinkVertically() + fadeOut()) {
            Box(Modifier.padding(start = 16.dp + Space.s)) { MessageImages(images, size = 56.dp) }
        }
        AnimatedVisibility(open, enter = expandVertically() + fadeIn(), exit = shrinkVertically() + fadeOut()) {
            Row(Modifier.height(IntrinsicSize.Min)) {
                Box(Modifier.padding(start = 7.dp).width(1.5.dp).fillMaxHeight().background(MaterialTheme.colorScheme.outlineVariant))
                Column(Modifier.padding(start = Space.m), verticalArrangement = Arrangement.spacedBy(Space.s)) {
                    items.forEach { item ->
                        val target = remember(item.id) { BringIntoViewRequester() }
                        if (item.id == active) LaunchedEffect(item.id) {
                            withFrameNanos { }
                            target.bringIntoView()
                        }
                        Box(Modifier.bringIntoViewRequester(target)) {
                            ItemRow(item, retryBlocked = false, actions = actions)
                        }
                    }
                }
            }
        }
        items.lastOrNull()?.takeIf { it.runDurationMs > 0 }?.let { TurnFooter(it.runStartedMs, it.runDurationMs) }
    }
}

@Composable
private fun CheckpointRow(item: ItemView, actions: TranscriptActions) {
    var confirm by remember { mutableStateOf(false) }
    var expanded by rememberSaveable(item.id) { mutableStateOf(false) }
    val cp = item.checkpoint
    val files = cp?.files.orEmpty()
    val tints = LocalTints.current
    fun viewDiff() {
        val c = cp ?: return
        actions.send(Event.SelectDiffSource(c.diffSource.ifEmpty { "turn:${c.turnCount}" }))
        actions.send(Event.ShowPanel("diff"))
        actions.openDiff()
    }
    Surface(color = MaterialTheme.colorScheme.surfaceContainerLow, shape = MaterialTheme.shapes.small, modifier = Modifier.fillMaxWidth()) {
        Column {
            FlowRow(Modifier.padding(horizontal = Space.m, vertical = Space.xs), verticalArrangement = Arrangement.Center, horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                Row(
                    Modifier.align(Alignment.CenterVertically).clip(MaterialTheme.shapes.small)
                        .then(if (files.isNotEmpty()) Modifier.clickable(onClickLabel = if (expanded) "Hide the changed files" else "Show the changed files") { expanded = !expanded } else Modifier)
                        .padding(vertical = Space.xs),
                    verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.s),
                ) {
                    if (files.isNotEmpty()) Icon(Icons.Outlined.ChevronRight, null, tint = MaterialTheme.colorScheme.outline, modifier = Modifier.size(16.dp).rotate(if (expanded) 90f else 0f))
                    else Icon(Icons.Outlined.Flag, null, tint = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.size(16.dp))
                    Text(item.title, style = MaterialTheme.typography.bodyMedium, maxLines = 1)
                    // A turn that changed nothing has no counts or diff to show.
                    cp?.takeIf { it.filesChanged > 0u }?.let {
                        Text("${it.filesChanged} file${if (it.filesChanged == 1u) "" else "s"} · +${it.additions} −${it.deletions}", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                }
                if (cp != null) {
                    if (cp.filesChanged > 0u) TextButton(onClick = ::viewDiff) { Text("View diff") }
                    if (cp.canRevert) TextButton(onClick = { confirm = true }) { Text("Revert to here") }
                }
            }
            AnimatedVisibility(expanded && files.isNotEmpty(), enter = expandVertically() + fadeIn(), exit = shrinkVertically() + fadeOut()) {
                Column(Modifier.padding(start = Space.m, end = Space.m, bottom = Space.s)) {
                    files.forEach { file ->
                        Row(
                            Modifier.fillMaxWidth().clip(MaterialTheme.shapes.small).clickable(onClickLabel = "View diff") { viewDiff() }.padding(horizontal = Space.xs, vertical = Space.xs),
                            verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.s),
                        ) {
                            Text(file.path, fontFamily = Mono, fontSize = 12.sp, maxLines = 1, overflow = TextOverflow.StartEllipsis, modifier = Modifier.weight(1f))
                            Text("+${file.insertions}", color = tints.success, style = MaterialTheme.typography.labelSmall)
                            Text("−${file.deletions}", color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.labelSmall)
                        }
                    }
                }
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
private fun BackgroundActivity(tasks: List<BackgroundTaskView>, actions: TranscriptActions) {
    var expanded by remember { mutableStateOf(false) }
    val previewCount = 2
    val visible = if (expanded) tasks else tasks.take(previewCount)
    LaunchedEffect(tasks.size) {
        if (tasks.size <= previewCount) expanded = false
    }
    Card {
        Text(
            "Background activity · ${tasks.size}",
            style = MaterialTheme.typography.labelMedium,
            fontWeight = FontWeight.SemiBold,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        for (task in visible) {
            val icon = when (task.kind) {
                "subagent" -> Icons.Outlined.Groups
                "command" -> Icons.Outlined.Terminal
                "monitor" -> Icons.Outlined.GraphicEq
                else -> Icons.Outlined.Settings
            }
            val detail = when {
                task.status == "pending" -> "Starting"
                task.status == "waiting" && !task.progress.isNullOrBlank() -> "Waiting · ${task.progress}"
                task.status == "waiting" -> "Waiting"
                !task.progress.isNullOrBlank() -> task.progress!!
                else -> "Running in the background"
            }
            val child = task.childThreadId?.takeIf { it.isNotEmpty() }
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .clip(MaterialTheme.shapes.small)
                    .then(if (child != null) Modifier.clickable { actions.openThread(child) } else Modifier)
                    .padding(vertical = Space.xxs),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Space.s),
            ) {
                Icon(icon, null, tint = MaterialTheme.colorScheme.primary, modifier = Modifier.size(18.dp))
                Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                    Text(task.label, style = MaterialTheme.typography.bodyMedium, fontWeight = FontWeight.Medium, maxLines = 2, overflow = TextOverflow.Ellipsis)
                    Text(detail, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 2, overflow = TextOverflow.Ellipsis)
                }
                if (child != null) {
                    Icon(Icons.AutoMirrored.Outlined.OpenInNew, "Open background thread", tint = MaterialTheme.colorScheme.primary, modifier = Modifier.size(16.dp))
                }
            }
        }
        if (tasks.size > previewCount) {
            TextButton(onClick = { expanded = !expanded }) {
                Icon(if (expanded) Icons.Outlined.KeyboardArrowUp else Icons.Outlined.KeyboardArrowDown, null, modifier = Modifier.size(16.dp))
                Spacer(Modifier.width(Space.xs))
                Text(if (expanded) "Show less" else "Show all ${tasks.size}")
            }
        }
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

/** The last turn stopped on a usage limit: when it resets, and continuing (or snoozing) the thread then. */
@Composable
private fun UsageLimitBanner(resets: String, plan: String, actions: TranscriptActions) {
    Surface(color = MaterialTheme.colorScheme.surfaceContainerLow, shape = MaterialTheme.shapes.medium, modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(Space.m), verticalArrangement = Arrangement.spacedBy(Space.s)) {
            Text(if (resets == "now") "The usage limit has reset." else "Usage limit reached. It resets $resets.", style = MaterialTheme.typography.labelLarge)
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                when (plan) {
                    "resume", "snooze" -> {
                        Text(if (plan == "resume") "Continues when it resets." else "Snoozed until it resets.", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        TextButton(onClick = { actions.send(Event.CancelLimitPlan) }) { Text("Cancel") }
                    }
                    else -> {
                        androidx.compose.material3.FilledTonalButton(onClick = { actions.send(Event.PlanForLimitReset(false)) }) { Text("Resume at reset") }
                        TextButton(onClick = { actions.send(Event.PlanForLimitReset(true)) }) { Text("Snooze until reset") }
                    }
                }
            }
        }
    }
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

/** "Running a command · 42 s" while a turn runs, with what the agent is on;
 *  "Show step" brings the step under way into view. */
@Composable
private fun WorkingRow(status: String, since: Long, activity: WorkingActivityView?, doing: String?, showStep: (String) -> Unit) {
    Row(Modifier.fillMaxWidth().padding(vertical = Space.xs), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.s)) {
        if (status == "approval") {
            Icon(Icons.Filled.PanTool, null, tint = LocalTints.current.caution, modifier = Modifier.size(18.dp))
            Text("Waiting for you", color = MaterialTheme.colorScheme.onSurfaceVariant)
        } else {
            val pulse = rememberInfiniteTransition(label = "work").animateFloat(0.4f, 1f, infiniteRepeatable(tween(900), RepeatMode.Reverse), label = "a")
            Icon(Icons.Outlined.AutoAwesome, null, tint = MaterialTheme.colorScheme.primary, modifier = Modifier.size(18.dp).alpha(pulse.value))
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(Space.xxs)) {
                var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
                LaunchedEffect(since) { while (true) { now = System.currentTimeMillis(); delay(1000) } }
                val title = when (activity?.kind) {
                    "starting" -> "Starting"
                    "assistant" -> "Writing a reply"
                    "command" -> "Running a command"
                    "file_change" -> "Editing files"
                    "tool" -> "Using a tool"
                    "reasoning" -> "Thinking"
                    else -> "Working"
                }
                Text(
                    if (since > 0) "$title · ${duration(maxOf(0, now - since))}" else "$title…",
                    style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                doing?.let { Text(it, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.outline, maxLines = 1, overflow = TextOverflow.Ellipsis) }
            }
            activity?.stepItemId?.let { id ->
                TextButton(onClick = { showStep(id) }) { Text("Show step") }
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
private fun MessageImages(images: List<ImageView>, size: Dp = 96.dp) {
    var shown by remember { mutableStateOf<ImageView?>(null) }
    Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(Space.s)) {
        images.forEach { image ->
            AsyncImage(
                model = image.url, contentDescription = image.name, contentScale = ContentScale.Crop,
                modifier = Modifier.size(size).clip(MaterialTheme.shapes.small).clickable { shown = image },
            )
        }
    }
    shown?.let { image ->
        val context = LocalContext.current
        val core = LocalCore.current
        val scope = rememberCoroutineScope()
        // What saving or sharing came to, in the app's toast.
        fun tell(kind: String, message: String) { core?.update(Event.ShowNotice(kind, message)) }
        ModalBottomSheet(onDismissRequest = { shown = null }) {
            AsyncImage(model = image.url, contentDescription = image.name, contentScale = ContentScale.Fit, modifier = Modifier.fillMaxWidth().padding(Space.l))
            Row(Modifier.fillMaxWidth().padding(horizontal = Space.l), horizontalArrangement = Arrangement.spacedBy(Space.s, Alignment.End)) {
                OutlinedButton(onClick = {
                    scope.launch {
                        Images.share(context, image.url, image.name).onFailure { tell("error", "Couldn't share the image: ${it.message ?: "it didn't download."}") }
                    }
                }) { Icon(Icons.Outlined.Share, null, modifier = Modifier.size(18.dp)); Spacer(Modifier.width(Space.xs)); Text("Share") }
                FilledTonalButton(onClick = {
                    scope.launch {
                        Images.save(context, image.url, image.name).fold(
                            // Closed, so the toast that says where it went shows.
                            onSuccess = { shown = null; tell("success", it) },
                            onFailure = { tell("error", "Couldn't save the image: ${it.message ?: "it didn't download."}") },
                        )
                    }
                }) { Icon(Icons.Outlined.Download, null, modifier = Modifier.size(18.dp)); Spacer(Modifier.width(Space.xs)); Text("Save") }
            }
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

/** A diff file's name (`old → new` when renamed), how it changed when not
 *  just edited (Added, Deleted, Renamed, Binary), and its counts. */
@Composable
fun DiffFileHeader(file: DiffFileView, modifier: Modifier = Modifier) {
    val tints = LocalTints.current
    val change = when (file.change) {
        "added" -> "Added" to tints.success
        "deleted" -> "Deleted" to MaterialTheme.colorScheme.error
        "renamed" -> "Renamed" to MaterialTheme.colorScheme.tertiary
        "binary" -> "Binary" to MaterialTheme.colorScheme.onSurfaceVariant
        else -> null
    }
    val old = file.oldPath?.takeIf { it != file.path }
    Row(modifier, verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.s)) {
        Text(
            if (old != null) "$old → ${file.path}" else file.path,
            fontFamily = Mono, fontSize = 13.sp, maxLines = 1, overflow = TextOverflow.StartEllipsis, modifier = Modifier.weight(1f),
        )
        change?.let { (label, color) ->
            Surface(color = color.copy(alpha = 0.14f), shape = RoundedCornerShape(6.dp)) {
                Text(label, color = color, style = MaterialTheme.typography.labelSmall, modifier = Modifier.padding(horizontal = 6.dp, vertical = 1.dp))
            }
        }
        Text("+${file.additions}", color = tints.success, style = MaterialTheme.typography.labelMedium)
        Text("−${file.deletions}", color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.labelMedium)
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
