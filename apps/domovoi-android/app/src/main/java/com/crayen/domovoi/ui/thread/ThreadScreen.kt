package com.crayen.domovoi.ui.thread

import android.Manifest
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.Crossfade
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutVertically
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.height
import androidx.compose.material.icons.outlined.Checklist
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.font.FontWeight
import com.crayen.domovoi.app.PlanProgressView
import com.crayen.domovoi.app.TaskProgressView
import com.crayen.domovoi.ui.common.MarkdownBlocks
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import com.crayen.domovoi.app.ThreadLinkView
import androidx.compose.material.icons.filled.PanTool
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.automirrored.outlined.CallSplit
import androidx.compose.material.icons.automirrored.outlined.Undo
import androidx.compose.material.icons.outlined.AutoAwesome
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.CreateNewFolder
import androidx.compose.material.icons.outlined.Difference
import androidx.compose.material.icons.outlined.Folder
import androidx.compose.material.icons.outlined.KeyboardArrowDown
import androidx.compose.material.icons.outlined.KeyboardArrowUp
import androidx.compose.material.icons.outlined.Laptop
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.Terminal
import androidx.compose.material.icons.outlined.UnfoldMore
import androidx.compose.material.icons.outlined.CommentsDisabled
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Badge
import androidx.compose.material3.BadgedBox
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.TextField
import androidx.compose.material3.TextFieldDefaults
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.AssistChip
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.crayen.domovoi.app.ContextUsageView
import com.crayen.domovoi.app.ThreadPageView
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.app.FindView
import com.crayen.domovoi.core.Core
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import com.crayen.domovoi.app.SessionUsageView
import com.crayen.domovoi.ui.common.LimitWindowRow
import com.crayen.domovoi.ui.common.PercentBar
import com.crayen.domovoi.ui.common.limitColor
import com.crayen.domovoi.ui.common.ArtifactGallery
import com.crayen.domovoi.ui.common.ConnectionBanner
import com.crayen.domovoi.ui.common.EmptyState
import com.crayen.domovoi.ui.common.ProjectIconTile
import com.crayen.domovoi.ui.common.copyText
import com.crayen.domovoi.ui.common.watch
import com.crayen.domovoi.ui.nav.Navigator
import com.crayen.domovoi.ui.nav.Screen
import com.crayen.domovoi.ui.threads.NameDialog
import com.crayen.domovoi.ui.theme.LocalTints
import com.crayen.domovoi.ui.theme.Mono
import com.crayen.domovoi.ui.theme.Space
import kotlinx.coroutines.delay

/** A thread: transcript, then what it waits for, then the composer. */
@Composable
fun ThreadScreen(core: Core, threadId: String, nav: Navigator, snackbars: SnackbarHostState, showBack: Boolean) {
    val page by core.view.thread.watch()
    val connection by core.view.connection.watch()
    val sidebar by core.view.sidebar.watch()
    val retryBlocked by core.view.retryBlocked.watch()
    val thread = page?.takeIf { it.id == threadId }
    var slow by remember(threadId) { mutableStateOf(false) }
    var renaming by remember { mutableStateOf(false) }
    var deleting by remember { mutableStateOf(false) }
    var forking by remember { mutableStateOf(false) }
    var menu by remember { mutableStateOf(false) }
    // A scheduled task for this thread, being set up.
    var scheduling by remember { mutableStateOf<com.crayen.domovoi.app.ScheduledTaskEdit?>(null) }
    var artifacts by remember(threadId) { mutableStateOf(false) }
    var artifactJump by remember(threadId) { mutableStateOf<String?>(null) }
    var progress by remember(threadId) { mutableStateOf(false) }
    var contextDetails by remember { mutableStateOf(false) }
    var info by remember { mutableStateOf(false) }
    val context = LocalContext.current

    LaunchedEffect(threadId) {
        if (sidebar.selectedThreadId != threadId) core.update(Event.SelectThread(threadId))
        delay(15_000)
        val t = core.view.thread.value
        if (t == null || t.id != threadId || (t.loading && t.items.isEmpty())) slow = true
    }
    LaunchedEffect(connection.state) {
        if (connection.state != "connected" && (thread == null || (thread.items.isEmpty() && thread.loading))) slow = true
    }
    // A fork opens once the core selects it.
    LaunchedEffect(sidebar.selectedThreadId) {
        val id = sidebar.selectedThreadId
        if (forking && id != null && id != threadId) { forking = false; nav.show(Screen.Thread(id)) }
    }
    val dark = isSystemInDarkTheme()
    val actions = remember(nav, threadId, dark) {
        TranscriptActions(
            send = core::update,
            openThread = { nav.show(Screen.Thread(it)) },
            openDiff = { nav.open(Screen.Diff) },
            openArtifact = { artifactId, version, codeMode ->
                core.update(Event.OpenArtifact(threadId, artifactId, version, dark))
                if (codeMode) core.update(Event.SetArtifactViewMode("code"))
                nav.open(Screen.Artifacts)
            },
            openWorkspaceArtifact = { path ->
                core.update(Event.OpenArtifactWorkspacePath(threadId, path, dark))
                nav.open(Screen.Artifacts)
            },
            previewFile = { path ->
                core.update(Event.ShowPanel("artifacts"))
                core.update(Event.OpenArtifactFile(threadId, path))
                nav.open(Screen.Artifacts)
            },
        )
    }
    val isScratch = thread != null && sidebar.projects.firstOrNull { it.id == thread.projectId }?.isScratch == true

    Scaffold(
        topBar = {
            TopAppBar(
                navigationIcon = { if (showBack) IconButton(onClick = nav::back) { Icon(Icons.AutoMirrored.Outlined.ArrowBack, "Back") } },
                title = {
                    // A tap shows where the thread works: project, branch, worktree, folder.
                    Column(Modifier.clickable(enabled = thread != null, onClickLabel = "Thread info") { info = true }) {
                        Text(thread?.title ?: "Thread", maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.titleMedium)
                        thread?.let { t ->
                            val sub = listOfNotNull(t.projectTitle.ifEmpty { null }, t.branch).joinToString(" · ")
                            if (sub.isNotEmpty()) Text(sub, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                    }
                },
                actions = {
                    thread?.usage?.let { UsageMeter(it) { contextDetails = true } }
                    if (thread != null && thread.artifactCount > 0u) {
                        IconButton(onClick = {
                            core.update(Event.ShowPanel("artifacts"))
                            nav.open(Screen.Artifacts)
                        }) {
                            BadgedBox(badge = { Badge { Text("${thread.artifactCount}") } }) {
                                Icon(Icons.Outlined.AutoAwesome, "Artifacts (${thread.artifactCount})")
                            }
                        }
                    }
                    IconButton(onClick = { nav.open(Screen.Diff) }) { Icon(Icons.Outlined.Difference, "Changes") }
                    IconButton(onClick = { nav.open(Screen.Terminal) }) { Icon(Icons.Outlined.Terminal, "Terminal") }
                    Box {
                        IconButton(onClick = { menu = true }) { Icon(Icons.Outlined.MoreVert, "More") }
                        DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                            DropdownMenuItem(text = { Text("Files") }, leadingIcon = { Icon(Icons.Outlined.Folder, null) }, onClick = { menu = false; nav.open(Screen.Files) })
                            DropdownMenuItem(text = { Text("Git") }, leadingIcon = { Icon(Icons.AutoMirrored.Outlined.CallSplit, null) }, onClick = { menu = false; nav.open(Screen.Git) })
                            if (thread != null) {
                                HorizontalDivider()
                                DropdownMenuItem(
                                    text = { Text(if (thread.artifactCount > 0u) "Artifacts (${thread.artifactCount})" else "Artifacts") },
                                    leadingIcon = { Icon(Icons.Outlined.AutoAwesome, null) },
                                    onClick = {
                                        menu = false
                                        core.update(Event.ShowPanel("artifacts"))
                                        nav.open(Screen.Artifacts)
                                    },
                                )
                                DropdownMenuItem(text = { Text("Find in thread") }, leadingIcon = { Icon(Icons.Outlined.Search, null) }, onClick = { menu = false; core.update(Event.FindInThread("")) })
                                DropdownMenuItem(text = { Text("Rename") }, onClick = { menu = false; renaming = true })
                                DropdownMenuItem(text = { Text(if (thread.pinned) "Unpin" else "Pin") }, onClick = { menu = false; core.update(Event.PinThread(thread.id, !thread.pinned)) })
                                DropdownMenuItem(text = { Text(if (thread.settled) "Unsettle" else "Settle") }, onClick = { menu = false; core.update(Event.SettleThread(thread.id, !thread.settled)) })
                                DropdownMenuItem(text = { Text(if (thread.archived) "Unarchive" else "Archive") }, onClick = {
                                    menu = false
                                    core.update(Event.ArchiveThread(thread.id, !thread.archived))
                                    if (!thread.archived) nav.back()
                                })
                                DropdownMenuItem(text = { Text("Thread info") }, onClick = { menu = false; info = true })
                                DropdownMenuItem(text = { Text("Copy thread ID") }, onClick = { menu = false; copyText(context, thread.id) })
                                DropdownMenuItem(text = { Text("Schedule this thread…") }, enabled = !thread.archived, onClick = {
                                    menu = false
                                    core.update(Event.LoadSettings("scheduled"))
                                    scheduling = com.crayen.domovoi.ui.settings.scheduledTaskFromThread(thread)
                                })
                                DropdownMenuItem(text = { Text("Forget allowed tools") }, onClick = { menu = false; core.update(Event.ForgetThreadPermissions(thread.id)) })
                                HorizontalDivider()
                                DropdownMenuItem(
                                    text = { Text("Fork thread") }, leadingIcon = { Icon(Icons.AutoMirrored.Outlined.CallSplit, null) }, enabled = thread.canFork,
                                    onClick = { menu = false; forking = true; core.update(Event.ForkThread(thread.id)) },
                                )
                                val parent = thread.parent
                                if (thread.canMergeBack && parent != null) {
                                    DropdownMenuItem(text = { Text("Merge back into “${parent.title}”") }, leadingIcon = { Icon(Icons.AutoMirrored.Outlined.Undo, null) }, onClick = { menu = false; core.update(Event.MergeBack) })
                                }
                                DropdownMenuItem(
                                    text = { Text("Regenerate title") }, leadingIcon = { Icon(Icons.Outlined.AutoAwesome, null) }, enabled = thread.items.isNotEmpty(),
                                    onClick = { menu = false; core.update(Event.RegenerateTitle(thread.id)) },
                                )
                                if (thread.worktreePath != null && isScratch) {
                                    DropdownMenuItem(
                                        text = { Text("Make this folder a project") }, leadingIcon = { Icon(Icons.Outlined.CreateNewFolder, null) }, enabled = !thread.working,
                                        onClick = { menu = false; core.update(Event.PromoteScratchThread(thread.id)) },
                                    )
                                }
                                HorizontalDivider()
                                DropdownMenuItem(text = { Text("Delete thread", color = MaterialTheme.colorScheme.error) }, onClick = { menu = false; deleting = true })
                            }
                        }
                    }
                },
            )
        },
        snackbarHost = { SnackbarHost(snackbars) },
        bottomBar = {
            Column(Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.surface).navigationBarsPadding().imePadding(), horizontalAlignment = Alignment.CenterHorizontally) {
                // Subagents and side threads that wait on you: one banner each.
                thread?.waitingChildren?.forEach { child ->
                    WaitingChildBanner(child, Modifier.widthIn(max = Space.column)) {
                        core.update(Event.SelectThread(child.threadId))
                        nav.show(Screen.Thread(child.threadId))
                    }
                }
                val requests = thread?.pendingRequests.orEmpty()
                AnimatedVisibility(requests.isNotEmpty(), enter = slideInVertically { it } + fadeIn(), exit = slideOutVertically { it } + fadeOut()) {
                    ApprovalBar(requests, core::update)
                }
                val secrets = thread?.secretRequests.orEmpty()
                AnimatedVisibility(secrets.isNotEmpty(), enter = slideInVertically { it } + fadeIn(), exit = slideOutVertically { it } + fadeOut()) {
                    SecretRequestBar(secrets, core::update, Modifier.widthIn(max = Space.column))
                }
                if (thread != null && (thread.plan != null || thread.tasks != null)) {
                    WorkProgressStrip(thread.plan, thread.tasks, Modifier.widthIn(max = Space.column)) { progress = true }
                }
                Composer(core, Modifier.widthIn(max = Space.column).fillMaxWidth())
            }
        },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(top = padding.calculateTopPadding())) {
            ConnectionBanner(core)
            thread?.find?.let { FindBar(it, core::update) }
            thread?.let { ThreadPullRequestControls(core, it) }
            thread?.links?.let { WorkLinksStrip(it) }
            when {
                thread != null -> Transcript(
                    thread, retryBlocked, actions,
                    modifier = Modifier.weight(1f).fillMaxWidth(),
                    contentPadding = PaddingValues(bottom = padding.calculateBottomPadding()),
                    artifactJump = artifactJump,
                )
                slow -> Box(Modifier.weight(1f), contentAlignment = Alignment.Center) {
                    val connected = connection.state == "connected"
                    EmptyState(Icons.Outlined.CommentsDisabled, "Couldn't load this thread", if (connected) "The server hasn't sent it yet." else "Not connected to the server.") {
                        Button(onClick = { slow = false; core.update(if (connected) Event.ReloadThread else Event.Reconnect) }) { Text(if (connected) "Try again" else "Reconnect") }
                    }
                }
                else -> Box(Modifier.weight(1f).fillMaxWidth(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
            }
        }
    }

    if (artifacts && thread != null) {
        ArtifactGallery(core, thread, close = { artifacts = false }) { item ->
            core.update(Event.CloseFind)
            artifactJump = item
            artifacts = false
        }
    }
    if (progress && thread != null) {
        WorkProgressSheet(thread.plan, thread.tasks, onDismiss = { progress = false }) { planId ->
            progress = false
            core.update(Event.ImplementPlan(planId))
        }
    }
    val usage = thread?.usage
    LaunchedEffect(usage == null) { if (usage == null) contextDetails = false }
    if (contextDetails && usage != null) {
        UsageDetailsSheet(
            usage,
            onCompact = { contextDetails = false; core.update(Event.Compact) },
            onShowAll = { contextDetails = false; nav.open(Screen.Usage) },
        ) { contextDetails = false }
    }
    if (info && thread != null) ThreadInfoDialog(thread) { info = false }
    scheduling?.let { draft -> com.crayen.domovoi.ui.settings.ScheduledTaskEditorFor(core, draft) { scheduling = null } }
    if (renaming && thread != null) {
        NameDialog(title = "Rename thread", label = "Title", confirm = "Rename", initial = thread.title, onDismiss = { renaming = false }) {
            renaming = false
            core.update(Event.RenameThread(threadId, it))
        }
    }
    if (deleting) {
        AlertDialog(
            onDismissRequest = { deleting = false },
            title = { Text("Delete this thread?") },
            text = { Text("Its conversation is gone for good. Files it changed stay as they are.") },
            confirmButton = { TextButton(onClick = { deleting = false; core.update(Event.DeleteThread(threadId)); nav.back() }) { Text("Delete", color = MaterialTheme.colorScheme.error) } },
            dismissButton = { TextButton(onClick = { deleting = false }) { Text("Cancel") } },
        )
    }
}

/** Where the thread works: its project, branch, worktree and folder (each copied with a tap). */
@Composable
private fun ThreadInfoDialog(thread: ThreadPageView, onDismiss: () -> Unit) {
    val context = LocalContext.current
    val rows = listOfNotNull(
        thread.projectTitle.ifEmpty { null }?.let { "Project" to it },
        thread.branch?.let { "Branch" to it },
        thread.worktreePath?.let { "Worktree" to it },
        thread.cwd.ifEmpty { null }?.let { "Folder" to it },
        thread.modelLabel.ifEmpty { null }?.let { "Model" to it },
    )
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(thread.title, maxLines = 2, overflow = TextOverflow.Ellipsis) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(Space.m)) {
                rows.forEach { (label, value) ->
                    Column(
                        Modifier.fillMaxWidth().clip(MaterialTheme.shapes.small).clickable(onClickLabel = "Copy $label") { copyText(context, value, label) },
                    ) {
                        Text(label, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        Text(value, style = MaterialTheme.typography.bodyMedium, fontFamily = if (label == "Folder" || label == "Worktree") Mono else null)
                    }
                }
            }
        },
        confirmButton = { TextButton(onClick = onDismiss) { Text("Done") } },
    )
}

/** The meter's colour: red all but full (90 %), orange nearly full (80 %). */
@Composable
private fun contextColor(context: ContextUsageView, normal: Color): Color = when {
    context.critical -> MaterialTheme.colorScheme.error
    context.high -> LocalTints.current.caution
    else -> normal
}

/**
 * The thread's usage at a glance: a ring for how full the agent's context window
 * is (or the fullest provider limit when there is no context yet) and its
 * percentage, with a dot when a limit runs low; a tap shows both in detail.
 */
@Composable
private fun UsageMeter(usage: SessionUsageView, onClick: () -> Unit) {
    val context = usage.context
    val fullest = usage.fullest
    // What the ring fills to: the context, else the fullest limit (null: no ring).
    val ring: Pair<UInt, Color>? = when {
        context != null -> if (context.maxTokens > 0uL) context.percent to contextColor(context, MaterialTheme.colorScheme.primary) else null
        fullest != null -> fullest.percent to limitColor(fullest)
        else -> null
    }
    val textColor = when {
        context != null -> contextColor(context, MaterialTheme.colorScheme.onSurfaceVariant)
        fullest != null && fullest.high -> limitColor(fullest)
        else -> MaterialTheme.colorScheme.onSurfaceVariant
    }
    val alert = when (usage.limitAlert) {
        "danger" -> MaterialTheme.colorScheme.error
        "caution" -> LocalTints.current.caution
        else -> null
    }
    Row(
        Modifier.clip(MaterialTheme.shapes.small).clickable(onClickLabel = "Usage details", onClick = onClick).padding(horizontal = Space.xs, vertical = Space.s)
            .semantics(mergeDescendants = true) { contentDescription = usageSummary(usage).joinToString(". ") },
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.xs),
    ) {
        if (ring != null) {
            Box {
                androidx.compose.material3.CircularProgressIndicator(
                    progress = { ring.first.toFloat() / 100f },
                    modifier = Modifier.size(16.dp),
                    strokeWidth = 2.5.dp,
                    color = ring.second,
                    trackColor = MaterialTheme.colorScheme.surfaceContainerHighest,
                    strokeCap = StrokeCap.Round,
                )
                if (alert != null) {
                    // A provider limit nearly used up.
                    Box(
                        Modifier.align(Alignment.TopEnd).offset(x = 3.dp, y = (-3).dp).size(7.dp)
                            .background(MaterialTheme.colorScheme.surface, CircleShape).padding(1.dp).background(alert, CircleShape),
                    )
                }
            }
        }
        Text(
            if (ring != null) "${ring.first}%" else context?.label.orEmpty(),
            style = MaterialTheme.typography.labelMedium,
            color = textColor,
        )
    }
}

/** `Context: 48k of 200k tokens (24%)`, `Claude Weekly: 92% used, resets in 2d`. */
private fun usageSummary(usage: SessionUsageView): List<String> = buildList {
    usage.context?.let { add("Context: ${it.label}" + (if (it.maxTokens > 0uL) " (${it.percent}%)" else "") + if (it.remainingLabel.isNotEmpty()) ", ${it.remainingLabel}" else "") }
    val provider = usage.limits?.provider
    val fullest = usage.fullest
    if (provider != null && fullest != null) {
        add("$provider ${fullest.label}: ${fullest.percent}% used" + if (fullest.resets.isNotEmpty()) ", resets ${fullest.resets}" else "")
    } else if (provider != null && usage.limitsNotice.isNotEmpty()) {
        add("$provider limits: ${usage.limitsNotice}")
    }
}

/**
 * The thread's usage in full: the context window (how much is used, where it
 * compacts, what it is made of, the cost), then the limits of the provider it
 * runs on, and the way to the Usage tab.
 */
@Composable
private fun UsageDetailsSheet(usage: SessionUsageView, onCompact: () -> Unit, onShowAll: () -> Unit, onDismiss: () -> Unit) {
    val uriHandler = LocalUriHandler.current
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(
            Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(horizontal = Space.xl),
            verticalArrangement = Arrangement.spacedBy(Space.m),
        ) {
            usage.context?.let { context ->
                Text("Context", style = MaterialTheme.typography.titleMedium)
                Text(context.label, style = MaterialTheme.typography.bodyLarge, color = contextColor(context, MaterialTheme.colorScheme.onSurface))
                if (context.maxTokens > 0uL) {
                    PercentBar(context.percent, contextColor(context, MaterialTheme.colorScheme.primary), tick = context.compactAtPercent)
                    Row {
                        Text(
                            listOf("${context.percent}% used", context.remainingLabel).filter { it.isNotEmpty() }.joinToString(" · "),
                            style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.weight(1f),
                        )
                        if (context.compactAtPercent > 0u) Text("Compacts at ${context.compactAtPercent}%", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                }
                if (context.cacheHitLabel.isNotEmpty()) {
                    // How much of the input came from the prompt cache (cheaper, faster).
                    Column(verticalArrangement = Arrangement.spacedBy(Space.xxs)) {
                        StatRow("Cache hit", context.cacheHitLabel)
                        PercentBar(context.cacheHitPercent, LocalTints.current.success)
                    }
                }
                if (context.breakdown.isNotEmpty() || context.cost.isNotEmpty()) {
                    context.breakdown.forEach { line -> StatRow(line.label, line.value) }
                    if (context.cost.isNotEmpty()) StatRow("Cost", context.cost, strong = true)
                }
                if (context.canCompact) {
                    OutlinedButton(onClick = onCompact, modifier = Modifier.fillMaxWidth()) { Text("Compact now") }
                }
            }
            usage.limits?.let { limits ->
                if (usage.context != null) HorizontalDivider()
                Row(verticalAlignment = Alignment.Bottom) {
                    Text("${limits.provider} limits", style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f))
                    if (usage.limitsChecked.isNotEmpty()) {
                        Text(
                            if (usage.limitsChecked == "now") "Updated now" else "Updated ${usage.limitsChecked} ago",
                            style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
                if (usage.limitsNotice.isNotEmpty()) {
                    Text(usage.limitsNotice, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
                limits.windows.forEach { LimitWindowRow(it) }
                if (limits.externalUrl.isNotEmpty()) {
                    TextButton(onClick = { runCatching { uriHandler.openUri(limits.externalUrl) } }) {
                        Text(limits.externalLabel.ifEmpty { "Usage on the web" })
                    }
                }
            }
            HorizontalDivider()
            TextButton(onClick = onShowAll, modifier = Modifier.fillMaxWidth()) { Text("All usage") }
            Spacer(Modifier.height(Space.xl))
        }
    }
}

@Composable
private fun StatRow(label: String, value: String, strong: Boolean = false) {
    Row(Modifier.fillMaxWidth()) {
        Text(label, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.weight(1f))
        Text(value, style = MaterialTheme.typography.bodyMedium, fontWeight = if (strong) FontWeight.SemiBold else FontWeight.Normal)
    }
}

/** A child thread waiting for an approval or an answer, with the way there. */
@Composable
private fun WaitingChildBanner(child: ThreadLinkView, modifier: Modifier = Modifier, open: () -> Unit) {
    val tint = LocalTints.current.caution
    val relation = when (child.relation) { "subagent" -> "Subagent"; "fork" -> "Fork"; "side" -> "Side thread"; else -> "Thread" }
    Surface(
        color = tint.copy(alpha = 0.12f),
        shape = MaterialTheme.shapes.medium,
        modifier = modifier.fillMaxWidth().padding(horizontal = Space.m, vertical = Space.xxs),
    ) {
        Row(Modifier.padding(start = Space.m), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.s)) {
            Icon(Icons.Filled.PanTool, null, tint = tint, modifier = Modifier.size(16.dp))
            Text(
                "$relation waiting: ${child.title.ifEmpty { "Untitled" }}",
                style = MaterialTheme.typography.bodyMedium, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f),
            )
            TextButton(onClick = open) { Text("Open") }
        }
    }
}

/** Opens the latest plan and checklist without moving the transcript. */
@Composable
private fun WorkProgressStrip(plan: PlanProgressView?, tasks: TaskProgressView?, modifier: Modifier = Modifier, onClick: () -> Unit) {
    Surface(
        color = MaterialTheme.colorScheme.surfaceContainer,
        shape = MaterialTheme.shapes.medium,
        modifier = modifier.fillMaxWidth().padding(horizontal = Space.m, vertical = Space.xxs).clip(MaterialTheme.shapes.medium).clickable(onClickLabel = "Show plan and checklist", onClick = onClick),
    ) {
        Column(Modifier.padding(horizontal = Space.m, vertical = Space.xs), verticalArrangement = Arrangement.spacedBy(Space.xxs)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                Icon(Icons.Outlined.Checklist, null, tint = MaterialTheme.colorScheme.primary, modifier = Modifier.size(14.dp))
                Text(listOfNotNull(plan?.label, tasks?.label).joinToString(" · "), style = MaterialTheme.typography.labelMedium, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f))
            }
            if (tasks != null && tasks.total > 0u) {
                LinearProgressIndicator(
                    progress = { tasks.done.toFloat() / tasks.total.toFloat() },
                    trackColor = MaterialTheme.colorScheme.surfaceContainerHighest,
                    strokeCap = StrokeCap.Round,
                    drawStopIndicator = {},
                    modifier = Modifier.fillMaxWidth().height(3.dp),
                )
            }
        }
    }
}

@Composable
private fun FindBar(find: FindView, send: (Event) -> Unit) {
    var query by rememberSaveable { mutableStateOf(find.query) }
    val focus = remember { FocusRequester() }
    LaunchedEffect(Unit) { focus.requestFocus() }
    Surface(color = MaterialTheme.colorScheme.surfaceContainer) {
        Row(Modifier.fillMaxWidth().padding(horizontal = Space.s), verticalAlignment = Alignment.CenterVertically) {
            TextField(
                value = query,
                onValueChange = { query = it; send(Event.FindInThread(it)) },
                placeholder = { Text("Find in thread") },
                leadingIcon = { Icon(Icons.Outlined.Search, null) },
                singleLine = true,
                colors = TextFieldDefaults.colors(focusedContainerColor = Color.Transparent, unfocusedContainerColor = Color.Transparent, focusedIndicatorColor = Color.Transparent, unfocusedIndicatorColor = Color.Transparent),
                modifier = Modifier.weight(1f).focusRequester(focus),
            )
            if (find.query.isNotEmpty()) Text(if (find.total == 0u) "No matches" else "${find.active} of ${find.total}", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            IconButton(onClick = { send(Event.FindNext(false)) }, enabled = find.total > 0u) { Icon(Icons.Outlined.KeyboardArrowUp, "Previous match") }
            IconButton(onClick = { send(Event.FindNext(true)) }, enabled = find.total > 0u) { Icon(Icons.Outlined.KeyboardArrowDown, "Next match") }
            IconButton(onClick = { send(Event.CloseFind) }) { Icon(Icons.Outlined.Close, "Close find") }
        }
    }
}

/** A thread about to start: where it runs, then the composer (`NewThreadScreen`). */
@Composable
fun NewThreadScreen(core: Core, nav: Navigator, snackbars: SnackbarHostState, showBack: Boolean) {
    val composer by core.view.composer.watch()
    val sidebar by core.view.sidebar.watch()
    val route by core.view.route.watch()
    val threadId by core.view.threadId.watch()
    val project = sidebar.projects.firstOrNull { it.id == sidebar.selectedProjectId }
    val askNotifications = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {}
    val startId = remember { threadId }
    // Sent: the thread's own screen takes over, and now hearing it finished makes sense.
    LaunchedEffect(threadId, route) {
        val id = threadId
        if (id != null && id != startId && route == "thread") {
            if (Build.VERSION.SDK_INT >= 33) askNotifications.launch(Manifest.permission.POST_NOTIFICATIONS)
            nav.show(Screen.Thread(id))
        }
    }
    Scaffold(
        topBar = {
            TopAppBar(
                navigationIcon = { if (showBack) IconButton(onClick = nav::back) { Icon(Icons.AutoMirrored.Outlined.ArrowBack, "Back") } },
                title = { Text(project?.let { "New in ${it.title}" } ?: "New thread", maxLines = 1, overflow = TextOverflow.Ellipsis) },
            )
        },
        snackbarHost = { SnackbarHost(snackbars) },
        bottomBar = { Column(Modifier.background(MaterialTheme.colorScheme.surface).navigationBarsPadding().imePadding()) { Composer(core) } },
    ) { padding ->
        Column(
            Modifier.fillMaxSize().padding(padding).padding(horizontal = Space.xl),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(Space.l, Alignment.CenterVertically),
        ) {
            // The project's own icon: which project this starts in, at a glance.
            Crossfade(project?.id, label = "projectIcon") { id ->
                val shown = sidebar.projects.firstOrNull { it.id == id }
                if (shown != null) ProjectIconTile(shown.icon, core, size = 64.dp)
                else Icon(Icons.Outlined.AutoAwesome, null, tint = MaterialTheme.colorScheme.primary, modifier = Modifier.size(44.dp))
            }
            Text(project?.let { "What should we do in ${it.title}?" } ?: "Pick a project to start", style = MaterialTheme.typography.titleLarge, textAlign = TextAlign.Center)
            FlowRow(horizontalArrangement = Arrangement.spacedBy(Space.s, Alignment.CenterHorizontally), verticalArrangement = Arrangement.spacedBy(Space.s)) {
                ChoiceChip(project?.title ?: "Project", Icons.Outlined.Folder, sidebar.projects.map { it.id to it.title }) { core.update(Event.NewThread(it)) }
                IssueComposerControls(core, composer)
                if (composer.prWorktree.isEmpty()) {
                    val worktree = composer.workspaceMode == "worktree"
                    ChoiceChip(
                        if (composer.workspaceRequired) "Choose workspace" else if (worktree) "New worktree" else "Current checkout",
                        if (worktree) Icons.AutoMirrored.Outlined.CallSplit else Icons.Outlined.Laptop,
                        listOf("local" to "Current checkout", "worktree" to "New worktree"),
                    ) { core.update(Event.SetWorkspaceMode(it)) }
                    if (worktree && composer.branches.isNotEmpty()) {
                        ChoiceChip("from ${composer.baseBranch ?: "HEAD"}", Icons.AutoMirrored.Outlined.CallSplit, composer.branches.map { it to it }) { core.update(Event.SetBaseBranch(it)) }
                    }
                } else {
                    AssistChip(onClick = {}, label = { Text(composer.prWorktree) }, leadingIcon = { Icon(Icons.AutoMirrored.Outlined.CallSplit, null, modifier = Modifier.size(18.dp)) })
                }
            }
            Text(
                if (composer.workspaceMode == "worktree") "A new worktree keeps the agent's changes apart from your checkout." else "The agent works in your current checkout.",
                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, textAlign = TextAlign.Center,
            )
        }
    }
}

@Composable
private fun ChoiceChip(label: String, icon: androidx.compose.ui.graphics.vector.ImageVector, choices: List<Pair<String, String>>, pick: (String) -> Unit) {
    var open by remember { mutableStateOf(false) }
    Box {
        AssistChip(
            onClick = { open = true },
            label = { Text(label, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.widthIn(max = 220.dp)) },
            leadingIcon = { Icon(icon, null, modifier = Modifier.size(18.dp)) },
            trailingIcon = { Icon(Icons.Outlined.UnfoldMore, null, modifier = Modifier.size(16.dp)) },
        )
        DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
            choices.forEach { (value, text) -> DropdownMenuItem(text = { Text(text) }, onClick = { open = false; pick(value) }) }
        }
    }
}


@Composable
private fun WorkProgressSheet(
    plan: PlanProgressView?, tasks: TaskProgressView?, onDismiss: () -> Unit, implement: (String) -> Unit,
) {
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Row(
            Modifier.fillMaxWidth().padding(horizontal = Space.xl),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("Plan and checklist", style = MaterialTheme.typography.titleLarge, modifier = Modifier.weight(1f).semantics { heading() })
            TextButton(onClick = onDismiss) { Text("Done") }
        }
        HorizontalDivider()
        Column(
            Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(Space.xl),
            verticalArrangement = Arrangement.spacedBy(Space.m),
        ) {
            if (plan != null) {
                Text(plan.label, style = MaterialTheme.typography.titleMedium, modifier = Modifier.semantics { heading() })
                MarkdownBlocks(plan.blocks)
                if (plan.canImplement) {
                    Button(onClick = { implement(plan.planId) }) { Text("Implement plan") }
                }
            }
            if (tasks != null) {
                if (plan != null) HorizontalDivider()
                Text("Checklist · ${tasks.done}/${tasks.total}", style = MaterialTheme.typography.titleMedium, modifier = Modifier.semantics { heading() })
                if (tasks.previousRun) {
                    Text("Previous run", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
                tasks.explanation?.let { Text(it, style = MaterialTheme.typography.bodyMedium) }
                TodoSteps(tasks.todos)
            }
            if (plan == null && tasks == null) {
                Text("No plan or checklist is available.")
            }
        }
    }
}

@Composable
private fun IssueComposerControls(core: Core, composer: com.crayen.domovoi.app.ComposerView) {
    val issue = composer.issueChip
    val query = composer.issuePicker
    if (issue != null) {
        Text(issue, style = MaterialTheme.typography.labelMedium)
        TextButton(onClick = { core.update(Event.DetachIssue) }) { Text("Remove issue") }
        TextButton(onClick = { core.update(Event.SetAssignIssueToMe(!composer.issueAssignToMe)) }) {
            Text(if (composer.issueAssignToMe) "✓ Assign to me" else "Assign to me")
        }
    } else if (query != null) {
        AlertDialog(
            onDismissRequest = { core.update(Event.DetachIssue) },
            title = { Text("Attach issue") },
            text = { OutlinedTextField(query, { core.update(Event.SetIssuePickerQuery(it)) }, label = { Text("Issue URL or #123") }, singleLine = true) },
            confirmButton = { TextButton(onClick = { core.update(Event.AttachIssue) }) { Text("Attach") } },
            dismissButton = { TextButton(onClick = { core.update(Event.DetachIssue) }) { Text("Cancel") } },
        )
    } else if (composer.issueAvailable) {
        TextButton(onClick = { core.update(Event.OpenIssuePicker) }) { Text("Attach issue") }
    }
}

@Composable
fun ThreadPullRequestControls(core: Core, thread: com.crayen.domovoi.app.ThreadPageView, showStatus: Boolean = true) {
    val uri = androidx.compose.ui.platform.LocalUriHandler.current
    val pr = thread.pullRequestStatus
    if (showStatus) Column(Modifier.padding(horizontal = Space.s)) {
        if (pr != null) {
            TextButton(onClick = { uri.openUri(pr.prStatusUrl) }) { Text(pr.prStatusLabel) }
            val state = listOfNotNull(pr.prStatusChecks, pr.prStatusReview).joinToString(" · ")
            if (state.isNotEmpty()) Text(state, style = MaterialTheme.typography.labelSmall)
            if (pr.prStatusHasComments) {
                TextButton(onClick = { core.update(Event.HandReviewCommentsToAgent) }) { Text("Send review to agent") }
            }
            if (pr.prStatusFollowing) {
                TextButton(onClick = { core.update(Event.StopFollowingPullRequest) }) { Text("Stop following") }
            }
        }
        if (thread.canCreatePullRequest && pr == null) {
            TextButton(onClick = { core.update(Event.PreparePullRequest) }) { Text("Create pull request") }
        }
    }
    val sheet = thread.pullRequestSheet ?: return
    fun field(field: String, value: String) = core.update(Event.SetPullRequestSheetField(field, value))
    AlertDialog(
        onDismissRequest = { core.update(Event.ClosePullRequestSheet) },
        title = { Text("Create pull request") },
        text = {
            Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(Space.s)) {
                sheet.prSheetBranch?.let { Text("$it → ${sheet.prSheetBase}") }
                sheet.prSheetNotice?.let { Text(it) }
                val existing = sheet.prSheetExistingUrl
                if (existing != null) {
                    TextButton(onClick = { uri.openUri(existing) }) { Text("Open existing pull request") }
                } else {
                    OutlinedTextField(sheet.prSheetTitle, { field("title", it) }, label = { Text("Title") }, singleLine = true)
                    OutlinedTextField(sheet.prSheetBody, { field("body", it) }, label = { Text("Description") }, minLines = 3)
                    if (sheet.prSheetBases.isNotEmpty()) {
                        ChoiceChip("Base: ${sheet.prSheetBase}", Icons.AutoMirrored.Outlined.CallSplit, sheet.prSheetBases.map { it to it }) { field("base", it) }
                    }
                    if (sheet.prSheetSupportsDraft) {
                        TextButton(onClick = { field("draft", (!sheet.prSheetDraft).toString()) }) { Text(if (sheet.prSheetDraft) "✓ Draft" else "Draft") }
                    }
                    sheet.prSheetFiles.forEach { file ->
                        TextButton(onClick = { core.update(Event.TogglePullRequestFile(file.prFilePath)) }) {
                            Text(if (file.prFileSelected) "✓ ${file.prFilePath}" else file.prFilePath)
                        }
                    }
                }
            }
        },
        confirmButton = {
            TextButton(onClick = { core.update(Event.CreatePullRequest) }, enabled = sheet.prSheetCanCreate) {
                Text(if (sheet.prSheetLoading) "Working…" else "Create")
            }
        },
        dismissButton = { TextButton(onClick = { core.update(Event.ClosePullRequestSheet) }) { Text("Close") } },
    )
}
