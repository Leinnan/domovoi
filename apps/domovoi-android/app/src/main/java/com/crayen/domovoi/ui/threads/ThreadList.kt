package com.crayen.domovoi.ui.threads

import androidx.compose.animation.animateColorAsState
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.CallSplit
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowRight
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.Archive
import androidx.compose.material.icons.outlined.Bedtime
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.ContentCopy
import androidx.compose.material.icons.outlined.Delete
import androidx.compose.material.icons.outlined.Download
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material.icons.outlined.Folder
import androidx.compose.material.icons.outlined.CreateNewFolder
import androidx.compose.material.icons.outlined.Inventory2
import androidx.compose.material.icons.outlined.Layers
import androidx.compose.material.icons.outlined.MarkChatUnread
import androidx.compose.material.icons.outlined.PanTool
import androidx.compose.material.icons.outlined.PushPin
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.MergeType
import androidx.compose.material3.Surface
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.material.icons.outlined.WbSunny
import androidx.compose.material.icons.filled.PushPin
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DatePicker
import androidx.compose.material3.DatePickerDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.SwipeToDismissBox
import androidx.compose.material3.SwipeToDismissBoxValue
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TextField
import androidx.compose.material3.TextFieldDefaults
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.material3.rememberDatePickerState
import androidx.compose.material3.rememberSwipeToDismissBoxState
import androidx.compose.material3.rememberTopAppBarState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.app.SidebarProjectView
import com.crayen.domovoi.app.SidebarSectionView
import com.crayen.domovoi.app.ThreadRowView
import com.crayen.domovoi.core.Core
import com.crayen.domovoi.ui.common.ConnectionBanner
import com.crayen.domovoi.ui.common.EmptyState
import com.crayen.domovoi.ui.common.ProjectIconTile
import com.crayen.domovoi.ui.common.SectionHeader
import com.crayen.domovoi.ui.common.SnoozeTimes
import com.crayen.domovoi.ui.common.SourceBadge
import com.crayen.domovoi.ui.common.StatusIcon
import com.crayen.domovoi.ui.common.copyText
import com.crayen.domovoi.ui.common.watch
import com.crayen.domovoi.ui.nav.Navigator
import com.crayen.domovoi.ui.nav.Screen
import com.crayen.domovoi.ui.theme.LocalTints
import com.crayen.domovoi.ui.theme.Space

/** The thread list, grouped by status (Inbox, Running, Inactive,
 * Archived) or by project, with search (`ThreadListView`). */
@Composable
fun ThreadListPane(core: Core, nav: Navigator, snackbars: SnackbarHostState, selectedId: String?) {
    val sidebar by core.view.sidebar.watch()
    val connection by core.view.connection.watch()
    val sessions by core.view.agentSessions.watch()
    var search by rememberSaveable { mutableStateOf(sidebar.searchQuery) }
    var acting by remember { mutableStateOf<ThreadRowView?>(null) }
    var snoozing by remember { mutableStateOf<ThreadRowView?>(null) }
    var pickingDate by remember { mutableStateOf<ThreadRowView?>(null) }
    var deleting by remember { mutableStateOf<ThreadRowView?>(null) }
    var askingName by remember { mutableStateOf(false) }
    val earlierShown = remember { mutableStateOf(setOf<String>()) }
    val scroll = TopAppBarDefaults.enterAlwaysScrollBehavior(rememberTopAppBarState())
    // Grouped by what each thread needs, not by project.
    val byStatus = sidebar.grouping == "status"
    val nothingListed = sidebar.sections.all { it.count == 0u } && sidebar.projects.all { it.threads.isEmpty() && it.earlier.isEmpty() }

    LaunchedEffect(connection.state) { if (connection.state == "connected") core.update(Event.ScanAgentSessions) }

    val newThread: (String?) -> Unit = { project ->
        core.update(Event.NewThread(project))
        nav.show(Screen.NewThread)
    }
    val addActions = AddProjectActions(
        importSessions = { nav.open(Screen.ImportSessions) },
        browse = { nav.open(Screen.BrowseFolders) },
        newProject = { askingName = true },
        withoutProject = {
            val scratch = sidebar.projects.firstOrNull { it.isScratch }
            if (scratch != null) newThread(scratch.id) else { core.update(Event.OpenScratch); nav.show(Screen.NewThread) }
        },
    )

    Scaffold(
        modifier = Modifier.nestedScroll(scroll.nestedScrollConnection),
        topBar = {
            TopAppBar(
                title = { ServerMenu(core, nav) },
                actions = {
                    IconButton(onClick = { core.update(Event.OpenPalette) }) { Icon(Icons.Outlined.Search, "Command palette") }
                    AddProjectMenu(addActions)
                },
                scrollBehavior = scroll,
            )
        },
        floatingActionButton = {
            ExtendedFloatingActionButton(
                onClick = { newThread(sidebar.selectedProjectId ?: sidebar.projects.firstOrNull()?.id) },
                icon = { Icon(Icons.Outlined.Edit, null) },
                text = { Text("New thread") },
            )
        },
        snackbarHost = { SnackbarHost(snackbars) },
    ) { padding ->
        PullToRefreshBox(
            isRefreshing = false,
            onRefresh = { core.update(Event.Resync) },
            modifier = Modifier.fillMaxSize().padding(padding),
        ) {
            LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(bottom = 96.dp)) {
                item("banner") { ConnectionBanner(core) }
                item("search") {
                    SearchField(search) { search = it; core.update(Event.SetSearch(it)) }
                }
                item("filters") {
                    Row(Modifier.padding(horizontal = Space.l), horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                        FilterChip(selected = !sidebar.unreadOnly, onClick = { core.update(Event.SetUnreadOnly(false)) }, label = { Text("All") })
                        FilterChip(selected = sidebar.unreadOnly, onClick = { core.update(Event.SetUnreadOnly(true)) }, label = { Text("Unread") })
                        Spacer(Modifier.weight(1f))
                        FilterChip(
                            selected = !byStatus,
                            onClick = { core.update(Event.SetSidebarGrouping(if (byStatus) "project" else "status")) },
                            label = { Text("By project") },
                            leadingIcon = { Icon(Icons.Outlined.Folder, null, modifier = Modifier.size(16.dp)) },
                        )
                    }
                }
                val row: LazyListScope.(ThreadRowView, String) -> Unit = { thread, section ->
                    item(key = "$section/${thread.id}") {
                        ThreadRow(
                            thread = thread,
                            core = core,
                            showsProject = byStatus,
                            selected = thread.id == selectedId,
                            onOpen = { nav.show(Screen.Thread(thread.id)) },
                            onLongPress = { acting = thread },
                            onArchive = { core.update(Event.ArchiveThread(thread.id, !thread.archived)) },
                            onSnooze = { if (thread.snoozedUntilMs > 0) core.update(Event.SnoozeThread(thread.id, 0)) else snoozing = thread },
                        )
                    }
                }
                if (search.isNotEmpty() && (sidebar.searching || sidebar.searchResults.isNotEmpty())) {
                    item("results-h") { SectionHeader(if (sidebar.searching) "Searching…" else "Results") }
                    items(sidebar.searchResults, key = { "hit/${it.threadId}" }) { hit ->
                        ListItem(
                            headlineContent = { Text(hit.title, maxLines = 1, overflow = TextOverflow.Ellipsis) },
                            supportingContent = { Text("${hit.projectTitle} · ${hit.snippet}", maxLines = 2, overflow = TextOverflow.Ellipsis) },
                            modifier = Modifier.combinedClickable(onClick = { nav.show(Screen.Thread(hit.threadId)) }),
                        )
                    }
                }
                if (search.isNotEmpty() && !sidebar.searching && sidebar.searchResults.isEmpty() && nothingListed) {
                    item("no-results") { EmptyState(Icons.Outlined.Search, "No results for “$search”") }
                }
                for (section in if (byStatus) sidebar.sections else emptyList()) {
                    item("s/${section.kind}") { StatusSectionHeader(section) { core.update(Event.ToggleSection(section.kind)) } }
                    section.rows.forEach { row(it, section.kind) }
                    if (section.hiddenCount > 0u) {
                        item("more/${section.kind}") {
                            TextButton(onClick = { core.update(Event.ShowAllInSection(section.kind)) }, modifier = Modifier.padding(start = Space.s)) { Text("Show ${section.hiddenCount} more") }
                        }
                    }
                }
                // Grouped by status, the Inbox lists what waits on you first.
                val waiting = if (byStatus) emptyList() else sidebar.projects.flatMap { it.threads }.filter { it.status == "approval" }
                if (search.isEmpty() && waiting.isNotEmpty()) {
                    item("needs-h") { SectionHeader("Needs you", icon = Icons.Outlined.PanTool, tint = LocalTints.current.caution) }
                    waiting.forEach { row(it, "needs") }
                }
                for (project in if (byStatus) emptyList() else sidebar.projects) {
                    item("p/${project.id}") {
                        SectionHeader(project.title, leading = { ProjectIconTile(project.icon, core, size = 20.dp) }, trailing = {
                            IconButton(onClick = { newThread(project.id) }) { Icon(Icons.Outlined.Edit, "New thread in ${project.title}", modifier = Modifier.size(20.dp)) }
                        })
                    }
                    project.threads.forEach { row(it, project.id) }
                    if (project.hiddenCount > 0u) {
                        item("more/${project.id}") {
                            TextButton(onClick = { core.update(Event.ShowAllThreads(project.id)) }, modifier = Modifier.padding(start = Space.s)) { Text("Show ${project.hiddenCount} more") }
                        }
                    }
                    if (project.earlierCount > 0u) earlier(project, earlierShown.value.contains(project.id), { shown ->
                        earlierShown.value = if (shown) earlierShown.value + project.id else earlierShown.value - project.id
                    }, row, onShowAll = { core.update(Event.ShowAllThreads(project.id)) })
                }
                if (!byStatus && sidebar.snoozed.isNotEmpty() && search.isEmpty()) {
                    item("snoozed-h") { SectionHeader("Snoozed", icon = Icons.Outlined.Bedtime, tint = MaterialTheme.colorScheme.onSurfaceVariant) }
                    sidebar.snoozed.forEach { row(it, "snoozed") }
                }
                if (sidebar.unreadOnly && search.isEmpty() && nothingListed) {
                    item("caught-up") {
                        EmptyState(Icons.Outlined.CheckCircle, "All caught up", "No thread has news you haven't seen.") {
                            OutlinedButton(onClick = { core.update(Event.SetUnreadOnly(false)) }) { Text("Show all threads") }
                        }
                    }
                } else if (sidebar.projects.isEmpty() && connection.state == "connected" && search.isEmpty()) {
                    item("empty") {
                        val available = sessions.available.toInt()
                        EmptyState(
                            Icons.Outlined.Folder, "No projects yet",
                            if (available > 0) "The server has Claude Code or Codex sessions in $available folders."
                            else "Pick a folder on the server, start a new project, or import your Claude Code sessions.",
                        ) {
                            Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
                                Button(onClick = addActions.importSessions, modifier = Modifier.fillMaxWidth()) { Text("Import from Claude Code") }
                                OutlinedButton(onClick = addActions.browse, modifier = Modifier.fillMaxWidth()) { Text("Add folder on server") }
                                Row(horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                                    OutlinedButton(onClick = addActions.newProject, modifier = Modifier.weight(1f)) { Text("New project") }
                                    OutlinedButton(onClick = addActions.withoutProject, modifier = Modifier.weight(1f)) { Text("No project") }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    acting?.let { thread ->
        ThreadActionsSheet(
            thread = thread,
            byStatus = byStatus,
            onDismiss = { acting = null },
            onAction = { action ->
                acting = null
                when (action) {
                    RowAction.Pin -> core.update(Event.PinThread(thread.id, !thread.pinned))
                    RowAction.Unread -> core.update(Event.MarkThreadUnread(thread.id))
                    RowAction.Snooze -> snoozing = thread
                    RowAction.Wake -> core.update(Event.SnoozeThread(thread.id, 0))
                    RowAction.Settle -> core.update(Event.SettleThread(thread.id, !thread.settled))
                    RowAction.Archive -> core.update(Event.ArchiveThread(thread.id, !thread.archived))
                    RowAction.Delete -> deleting = thread
                    RowAction.CopyId -> Unit
                }
            },
        )
    }
    snoozing?.let { thread ->
        SnoozeSheet(
            title = thread.title,
            onDismiss = { snoozing = null },
            onPick = { ms -> snoozing = null; core.update(Event.SnoozeThread(thread.id, ms)) },
            onPickDate = { snoozing = null; pickingDate = thread },
        )
    }
    pickingDate?.let { thread ->
        val state = rememberDatePickerState(initialSelectedDateMillis = System.currentTimeMillis() + 86_400_000L)
        DatePickerDialog(
            onDismissRequest = { pickingDate = null },
            confirmButton = {
                TextButton(onClick = {
                    // The chosen day at 09:00 local time.
                    state.selectedDateMillis?.let { utc ->
                        val day = java.time.Instant.ofEpochMilli(utc).atZone(java.time.ZoneOffset.UTC).toLocalDate()
                        val ms = day.atTime(9, 0).atZone(java.time.ZoneId.systemDefault()).toInstant().toEpochMilli()
                        core.update(Event.SnoozeThread(thread.id, ms))
                    }
                    pickingDate = null
                }) { Text("Snooze") }
            },
            dismissButton = { TextButton(onClick = { pickingDate = null }) { Text("Cancel") } },
        ) { DatePicker(state) }
    }
    deleting?.let { thread ->
        AlertDialog(
            onDismissRequest = { deleting = null },
            icon = { Icon(Icons.Outlined.Delete, null) },
            title = { Text("Delete “${thread.title}”?") },
            text = { Text("Its conversation is gone for good. Files it changed stay as they are.") },
            confirmButton = {
                TextButton(onClick = { deleting = null; core.update(Event.DeleteThread(thread.id)) }) {
                    Text("Delete", color = MaterialTheme.colorScheme.error)
                }
            },
            dismissButton = { TextButton(onClick = { deleting = null }) { Text("Cancel") } },
        )
    }
    if (askingName) {
        NameDialog(title = "New project", label = "Name", confirm = "Create", onDismiss = { askingName = false }) { name ->
            askingName = false
            core.update(Event.CreateProject(name))
        }
    }
}

private fun LazyListScope.earlier(
    project: SidebarProjectView,
    shown: Boolean,
    toggle: (Boolean) -> Unit,
    row: LazyListScope.(ThreadRowView, String) -> Unit,
    onShowAll: () -> Unit,
) {
    // A project with no Domovoi threads shows its newest sessions right away.
    val inline = if (project.threads.isEmpty()) project.earlier.take(3) else emptyList()
    inline.forEach { row(it, "earlier") }
    val more = project.earlierCount.toInt() - inline.size
    if (more > 0) {
        item("earlier-toggle/${project.id}") {
            ListItem(
                headlineContent = {
                    Text(
                        if (project.threads.isEmpty()) "$more more session${if (more == 1) "" else "s"}" else "Earlier sessions ($more)",
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                },
                trailingContent = { Icon(Icons.Outlined.ExpandMore, null, modifier = Modifier.rotateIf(shown)) },
                modifier = Modifier.combinedClickable(onClick = { toggle(!shown) }),
            )
        }
    }
    if (shown) {
        project.earlier.drop(inline.size).forEach { row(it, "earlier") }
        if (project.earlier.size < project.earlierCount.toInt()) {
            item("earlier-all/${project.id}") {
                TextButton(onClick = onShowAll, modifier = Modifier.padding(start = Space.s)) { Text("Show all ${project.earlierCount}") }
            }
        }
    }
}

private fun Modifier.rotateIf(on: Boolean) = this.rotate(if (on) 180f else 0f)

@Composable
private fun SearchField(value: String, onChange: (String) -> Unit) {
    val focus = remember { androidx.compose.ui.focus.FocusRequester() }
    val requests = com.crayen.domovoi.ui.nav.LocalSearchRequests.current
    LaunchedEffect(requests) { requests.collect { focus.requestFocus() } }
    TextField(
        value = value,
        onValueChange = onChange,
        placeholder = { Text("Search threads") },
        leadingIcon = { Icon(Icons.Outlined.Search, null) },
        trailingIcon = { if (value.isNotEmpty()) IconButton(onClick = { onChange("") }) { Icon(Icons.Outlined.Close, "Clear") } },
        singleLine = true,
        shape = CircleShape,
        keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
        colors = TextFieldDefaults.colors(
            focusedIndicatorColor = Color.Transparent,
            unfocusedIndicatorColor = Color.Transparent,
            unfocusedContainerColor = MaterialTheme.colorScheme.surfaceContainerHigh,
            focusedContainerColor = MaterialTheme.colorScheme.surfaceContainerHigh,
        ),
        modifier = Modifier.fillMaxWidth().padding(horizontal = Space.l, vertical = Space.s).focusRequester(focus),
    )
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun ThreadRow(
    thread: ThreadRowView,
    core: Core,
    showsProject: Boolean,
    selected: Boolean,
    onOpen: () -> Unit,
    onLongPress: () -> Unit,
    onArchive: () -> Unit,
    onSnooze: () -> Unit,
) {
    val haptics = LocalHapticFeedback.current
    val state = rememberSwipeToDismissBoxState()
    LaunchedEffect(state.currentValue) {
        when (state.currentValue) {
            SwipeToDismissBoxValue.EndToStart -> { onArchive(); state.reset() }
            SwipeToDismissBoxValue.StartToEnd -> { onSnooze(); state.reset() }
            SwipeToDismissBoxValue.Settled -> Unit
        }
    }
    SwipeToDismissBox(
        state = state,
        backgroundContent = {
            val direction = state.dismissDirection
            val tints = LocalTints.current
            val color by animateColorAsState(
                when (direction) {
                    SwipeToDismissBoxValue.EndToStart -> MaterialTheme.colorScheme.secondaryContainer
                    SwipeToDismissBoxValue.StartToEnd -> tints.planContainer
                    else -> Color.Transparent
                },
                label = "swipe",
            )
            Box(Modifier.fillMaxSize().background(color).padding(horizontal = Space.xl), contentAlignment = if (direction == SwipeToDismissBoxValue.StartToEnd) Alignment.CenterStart else Alignment.CenterEnd) {
                when (direction) {
                    SwipeToDismissBoxValue.EndToStart -> Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                        Text(if (thread.archived) "Unarchive" else "Archive"); Icon(Icons.Outlined.Archive, null)
                    }
                    SwipeToDismissBoxValue.StartToEnd -> Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                        Icon(if (thread.snoozedUntilMs > 0) Icons.Outlined.WbSunny else Icons.Outlined.Bedtime, null); Text(if (thread.snoozedUntilMs > 0) "Wake" else "Snooze")
                    }
                    else -> Unit
                }
            }
        },
    ) {
        ListItem(
            colors = ListItemDefaults.colors(containerColor = if (selected) MaterialTheme.colorScheme.secondaryContainer else MaterialTheme.colorScheme.surface),
            leadingContent = {
                if (showsProject) {
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.xs)) {
                        ProjectIconTile(thread.projectIcon, core, size = 24.dp)
                        StatusIcon(thread.status, thread.unread)
                    }
                } else StatusIcon(thread.status, thread.unread)
            },
            headlineContent = {
                Text(
                    thread.title, maxLines = 2, overflow = TextOverflow.Ellipsis,
                    fontWeight = if (thread.unread) FontWeight.SemiBold else FontWeight.Normal,
                )
            },
            // "project · model" (the project when no header names it), and the branch.
            supportingContent = listOfNotNull(thread.projectTitle.takeIf { showsProject }, thread.modelLabel).filter { it.isNotEmpty() }.joinToString(" · ").let { subtitle ->
                if (subtitle.isEmpty() && thread.branch == null) null else {
                    {
                        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.xs)) {
                            if (subtitle.isNotEmpty()) Text(subtitle, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodySmall, modifier = Modifier.weight(1f, fill = false))
                            thread.branch?.let { branch ->
                                Icon(Icons.AutoMirrored.Outlined.CallSplit, null, modifier = Modifier.size(12.dp))
                                Text(branch, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodySmall)
                            }
                        }
                    }
                }
            },
            trailingContent = {
                Column(horizontalAlignment = Alignment.End, verticalArrangement = Arrangement.spacedBy(Space.xs)) {
                    // A plan waits for you, or a run in plan mode ("Planning 6m"): its status as a plan-coloured badge.
                    if (thread.planReady) Pill(thread.statusLabel.ifEmpty { "Plan ready" }, LocalTints.current.plan)
                    else if (thread.planMode && thread.status == "working") Pill(thread.statusLabel.ifEmpty { "Planning" }, LocalTints.current.plan)
                    else Text(
                        when {
                            thread.statusLabel.isNotEmpty() -> thread.statusLabel
                            thread.snoozedUntilMs > 0 -> SnoozeTimes.label(thread.snoozedUntilMs)
                            else -> thread.age
                        },
                        style = MaterialTheme.typography.labelSmall,
                        color = if (thread.status == "failed") MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant,
                        fontWeight = if (thread.statusLabel.isNotEmpty()) FontWeight.Medium else FontWeight.Normal,
                    )
                    Row(horizontalArrangement = Arrangement.spacedBy(Space.xs), verticalAlignment = Alignment.CenterVertically) {
                        if (thread.relation == "fork") Icon(Icons.AutoMirrored.Outlined.CallSplit, "Fork", modifier = Modifier.size(12.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
                        if (thread.inWorktree) Icon(Icons.Outlined.Layers, "In its own worktree", modifier = Modifier.size(12.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
                        SourceBadge(thread.source)
                        if (thread.pinned) Icon(Icons.Filled.PushPin, "Pinned", modifier = Modifier.size(12.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
                        if (thread.pullRequest.isNotEmpty()) PullRequestBadge(thread.pullRequest, thread.pullRequestState)
                    }
                }
            },
            modifier = Modifier.combinedClickable(
                onClick = onOpen,
                onLongClick = { haptics.performHapticFeedback(HapticFeedbackType.LongPress); onLongPress() },
                onLongClickLabel = "More actions",
            ),
        )
    }
}

/** A small tinted label on a row. */
@Composable
private fun Pill(text: String, tint: Color) {
    Surface(color = tint.copy(alpha = 0.14f), shape = CircleShape) {
        Text(text, style = MaterialTheme.typography.labelSmall, color = tint, fontWeight = FontWeight.Medium, maxLines = 1, modifier = Modifier.padding(horizontal = Space.s, vertical = 1.dp))
    }
}

/** The thread's pull request ("#12"), coloured by its state. */
@Composable
private fun PullRequestBadge(number: String, state: String) {
    val tints = LocalTints.current
    val tint = when (state) {
        "open" -> tints.success
        "merged" -> tints.plan
        "closed" -> MaterialTheme.colorScheme.error
        else -> MaterialTheme.colorScheme.onSurfaceVariant
    }
    val label = when (state) { "draft" -> "Draft pull request"; "" -> "Pull request"; else -> "${state.replaceFirstChar { it.uppercase() }} pull request" }
    Surface(color = tint.copy(alpha = 0.14f), shape = CircleShape, modifier = Modifier.semantics { contentDescription = "$label $number" }) {
        Row(Modifier.padding(horizontal = Space.xs + 1.dp, vertical = 1.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(2.dp)) {
            Icon(Icons.Outlined.MergeType, null, tint = tint, modifier = Modifier.size(11.dp))
            Text(number, style = MaterialTheme.typography.labelSmall, color = tint)
        }
    }
}

/** A status section's header: a chevron, its title and how many threads it holds. */
@Composable
private fun StatusSectionHeader(section: SidebarSectionView, onToggle: () -> Unit) {
    SectionHeader(
        section.title,
        modifier = Modifier.clickable(onClickLabel = if (section.expanded) "Collapse" else "Expand", onClick = onToggle),
        icon = if (section.expanded) Icons.Outlined.ExpandMore else Icons.AutoMirrored.Outlined.KeyboardArrowRight,
        trailing = {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.s), modifier = Modifier.padding(end = Space.m)) {
                if (section.kind == "running" && section.count > 0u) CircularProgressIndicator(modifier = Modifier.size(12.dp), strokeWidth = 1.5.dp)
                if (section.count > 0u) Text("${section.count}", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        },
    )
}

private enum class RowAction { Pin, Unread, Snooze, Wake, Settle, CopyId, Archive, Delete }

/** Everything a row can do, on a long press. */
@Composable
private fun ThreadActionsSheet(thread: ThreadRowView, byStatus: Boolean, onDismiss: () -> Unit, onAction: (RowAction) -> Unit) {
    val context = LocalContext.current
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Text(thread.title, style = MaterialTheme.typography.titleMedium, maxLines = 2, overflow = TextOverflow.Ellipsis, modifier = Modifier.padding(horizontal = Space.xl, vertical = Space.s))
        @Composable
        fun action(label: String, icon: androidx.compose.ui.graphics.vector.ImageVector, a: RowAction, danger: Boolean = false) {
            val color = if (danger) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface
            ListItem(
                headlineContent = { Text(label, color = color) },
                leadingContent = { Icon(icon, null, tint = color) },
                colors = ListItemDefaults.colors(containerColor = Color.Transparent),
                modifier = Modifier.combinedClickable(onClick = { onAction(a) }),
            )
        }
        action(if (thread.pinned) "Unpin" else "Pin", Icons.Outlined.PushPin, RowAction.Pin)
        if (!thread.unread) action("Mark as unread", Icons.Outlined.MarkChatUnread, RowAction.Unread)
        if (thread.snoozedUntilMs > 0) action("Wake now", Icons.Outlined.WbSunny, RowAction.Wake) else action("Snooze…", Icons.Outlined.Bedtime, RowAction.Snooze)
        val settle = when {
            byStatus -> if (thread.settled) "Move to Inbox" else "Set Aside"
            else -> if (thread.settled) "Unsettle" else "Settle"
        }
        action(settle, Icons.Outlined.Inventory2, RowAction.Settle)
        HorizontalDivider(Modifier.padding(vertical = Space.xs))
        ListItem(
            headlineContent = { Text("Copy thread ID") },
            leadingContent = { Icon(Icons.Outlined.ContentCopy, null) },
            colors = ListItemDefaults.colors(containerColor = Color.Transparent),
            modifier = Modifier.combinedClickable(onClick = { copyText(context, thread.id); onAction(RowAction.CopyId) }),
        )
        action(if (thread.archived) "Unarchive" else "Archive", Icons.Outlined.Archive, RowAction.Archive)
        action("Delete…", Icons.Outlined.Delete, RowAction.Delete, danger = true)
        Spacer(Modifier.height(Space.l))
    }
}

/** When a snooze ends: the presets, or a day from the calendar. */
@Composable
fun SnoozeSheet(title: String, onDismiss: () -> Unit, onPick: (Long) -> Unit, onPickDate: () -> Unit) {
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Text("Snooze until…", style = MaterialTheme.typography.titleMedium, modifier = Modifier.padding(horizontal = Space.xl))
        Text(title, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.padding(horizontal = Space.xl, vertical = Space.xs))
        SnoozeTimes.presets().forEach { preset ->
            ListItem(
                headlineContent = { Text(preset.label) },
                trailingContent = { Text(SnoozeTimes.label(preset.ms), color = MaterialTheme.colorScheme.onSurfaceVariant) },
                colors = ListItemDefaults.colors(containerColor = Color.Transparent),
                modifier = Modifier.combinedClickable(onClick = { onPick(preset.ms) }),
            )
        }
        ListItem(
            headlineContent = { Text("Pick a date…") },
            colors = ListItemDefaults.colors(containerColor = Color.Transparent),
            modifier = Modifier.combinedClickable(onClick = onPickDate),
        )
        Spacer(Modifier.height(Space.l))
    }
}

data class AddProjectActions(val importSessions: () -> Unit, val browse: () -> Unit, val newProject: () -> Unit, val withoutProject: () -> Unit)

@Composable
private fun AddProjectMenu(actions: AddProjectActions) {
    var open by remember { mutableStateOf(false) }
    Box {
        IconButton(onClick = { open = true }) { Icon(Icons.Outlined.Add, "Add project") }
        DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
            DropdownMenuItem(text = { Text("Import from Claude Code") }, leadingIcon = { Icon(Icons.Outlined.Download, null) }, onClick = { open = false; actions.importSessions() })
            DropdownMenuItem(text = { Text("Add folder on server") }, leadingIcon = { Icon(Icons.Outlined.CreateNewFolder, null) }, onClick = { open = false; actions.browse() })
            DropdownMenuItem(text = { Text("New project") }, leadingIcon = { Icon(Icons.Outlined.Folder, null) }, onClick = { open = false; actions.newProject() })
            DropdownMenuItem(text = { Text("Thread without project") }, leadingIcon = { Icon(Icons.Outlined.Edit, null) }, onClick = { open = false; actions.withoutProject() })
        }
    }
}

/** A one-field dialog (new project, rename). */
@Composable
fun NameDialog(title: String, label: String, confirm: String, initial: String = "", onDismiss: () -> Unit, onConfirm: (String) -> Unit) {
    var text by rememberSaveable { mutableStateOf(initial) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(title) },
        text = {
            OutlinedTextField(value = text, onValueChange = { text = it }, label = { Text(label) }, singleLine = true, modifier = Modifier.fillMaxWidth())
        },
        confirmButton = { TextButton(onClick = { onConfirm(text.trim()) }, enabled = text.isNotBlank()) { Text(confirm) } },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}
