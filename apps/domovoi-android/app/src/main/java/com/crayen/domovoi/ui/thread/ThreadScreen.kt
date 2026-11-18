package com.crayen.domovoi.ui.thread

import android.Manifest
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutVertically
import androidx.compose.foundation.background
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
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.app.FindView
import com.crayen.domovoi.core.Core
import com.crayen.domovoi.ui.common.ConnectionBanner
import com.crayen.domovoi.ui.common.EmptyState
import com.crayen.domovoi.ui.common.copyText
import com.crayen.domovoi.ui.common.watch
import com.crayen.domovoi.ui.nav.Navigator
import com.crayen.domovoi.ui.nav.Screen
import com.crayen.domovoi.ui.threads.NameDialog
import com.crayen.domovoi.ui.theme.LocalTints
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
    val actions = remember(nav) { TranscriptActions(send = core::update, openThread = { nav.show(Screen.Thread(it)) }, openDiff = { nav.open(Screen.Diff) }) }
    val isScratch = thread != null && sidebar.projects.firstOrNull { it.id == thread.projectId }?.isScratch == true

    Scaffold(
        topBar = {
            TopAppBar(
                navigationIcon = { if (showBack) IconButton(onClick = nav::back) { Icon(Icons.AutoMirrored.Outlined.ArrowBack, "Back") } },
                title = {
                    Column {
                        Text(thread?.title ?: "Thread", maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.titleMedium)
                        thread?.let { t ->
                            val sub = listOfNotNull(t.projectTitle.ifEmpty { null }, t.branch).joinToString(" · ")
                            if (sub.isNotEmpty()) Text(sub, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                    }
                },
                actions = {
                    thread?.context?.let { ContextMeter(it) }
                    IconButton(onClick = { nav.open(Screen.Diff) }) { Icon(Icons.Outlined.Difference, "Changes") }
                    IconButton(onClick = { nav.open(Screen.Terminal) }) { Icon(Icons.Outlined.Terminal, "Terminal") }
                    Box {
                        IconButton(onClick = { menu = true }) { Icon(Icons.Outlined.MoreVert, "More") }
                        DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                            DropdownMenuItem(text = { Text("Files") }, leadingIcon = { Icon(Icons.Outlined.Folder, null) }, onClick = { menu = false; nav.open(Screen.Files) })
                            DropdownMenuItem(text = { Text("Git") }, leadingIcon = { Icon(Icons.AutoMirrored.Outlined.CallSplit, null) }, onClick = { menu = false; nav.open(Screen.Git) })
                            if (thread != null) {
                                HorizontalDivider()
                                DropdownMenuItem(text = { Text("Find in thread") }, leadingIcon = { Icon(Icons.Outlined.Search, null) }, onClick = { menu = false; core.update(Event.FindInThread("")) })
                                DropdownMenuItem(text = { Text("Rename") }, onClick = { menu = false; renaming = true })
                                DropdownMenuItem(text = { Text(if (thread.pinned) "Unpin" else "Pin") }, onClick = { menu = false; core.update(Event.PinThread(thread.id, !thread.pinned)) })
                                DropdownMenuItem(text = { Text(if (thread.settled) "Unsettle" else "Settle") }, onClick = { menu = false; core.update(Event.SettleThread(thread.id, !thread.settled)) })
                                DropdownMenuItem(text = { Text(if (thread.archived) "Unarchive" else "Archive") }, onClick = {
                                    menu = false
                                    core.update(Event.ArchiveThread(thread.id, !thread.archived))
                                    if (!thread.archived) nav.back()
                                })
                                DropdownMenuItem(text = { Text("Copy thread ID") }, onClick = { menu = false; copyText(context, thread.id) })
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
            Column(Modifier.background(MaterialTheme.colorScheme.surface).navigationBarsPadding().imePadding()) {
                val requests = thread?.pendingRequests.orEmpty()
                AnimatedVisibility(requests.isNotEmpty(), enter = slideInVertically { it } + fadeIn(), exit = slideOutVertically { it } + fadeOut()) {
                    ApprovalBar(requests, core::update)
                }
                Composer(core)
            }
        },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(top = padding.calculateTopPadding())) {
            ConnectionBanner(core)
            thread?.find?.let { FindBar(it, core::update) }
            when {
                thread != null -> Transcript(
                    thread, retryBlocked, actions,
                    modifier = Modifier.weight(1f).fillMaxWidth(),
                    contentPadding = PaddingValues(bottom = padding.calculateBottomPadding()),
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

/** How full the agent's context window is, as a ring and a percentage. */
@Composable
private fun ContextMeter(context: ContextUsageView) {
    val tints = LocalTints.current
    Row(Modifier.padding(horizontal = Space.xs), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.xs)) {
        if (context.maxTokens > 0uL) {
            androidx.compose.material3.CircularProgressIndicator(
                progress = { context.percent.toFloat() / 100f },
                modifier = Modifier.size(16.dp),
                strokeWidth = 2.5.dp,
                color = if (context.high) tints.caution else MaterialTheme.colorScheme.primary,
                trackColor = MaterialTheme.colorScheme.surfaceContainerHighest,
                strokeCap = StrokeCap.Round,
            )
        }
        Text(
            if (context.maxTokens > 0uL) "${context.percent}%" else context.label,
            style = MaterialTheme.typography.labelMedium,
            color = if (context.high) tints.caution else MaterialTheme.colorScheme.onSurfaceVariant,
        )
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
            Icon(Icons.Outlined.AutoAwesome, null, tint = MaterialTheme.colorScheme.primary, modifier = Modifier.size(44.dp))
            Text(project?.let { "What should we do in ${it.title}?" } ?: "Pick a project to start", style = MaterialTheme.typography.titleLarge, textAlign = TextAlign.Center)
            FlowRow(horizontalArrangement = Arrangement.spacedBy(Space.s, Alignment.CenterHorizontally), verticalArrangement = Arrangement.spacedBy(Space.s)) {
                ChoiceChip(project?.title ?: "Project", Icons.Outlined.Folder, sidebar.projects.map { it.id to it.title }) { core.update(Event.NewThread(it)) }
                if (composer.prWorktree.isEmpty()) {
                    val worktree = composer.workspaceMode == "worktree"
                    ChoiceChip(
                        if (worktree) "New worktree" else "Current checkout",
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
