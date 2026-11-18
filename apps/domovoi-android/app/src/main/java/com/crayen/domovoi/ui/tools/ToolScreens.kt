package com.crayen.domovoi.ui.tools

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
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
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.CallSplit
import androidx.compose.material.icons.automirrored.outlined.InsertDriveFile
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.ChevronRight
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.Folder
import androidx.compose.material.icons.outlined.FolderOpen
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.UnfoldMore
import androidx.compose.material.icons.outlined.Warning
import androidx.compose.material.icons.filled.Check
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.app.FileEntryView
import com.crayen.domovoi.core.Core
import com.crayen.domovoi.ui.common.EmptyState
import com.crayen.domovoi.ui.common.MarkdownBlocks
import com.crayen.domovoi.ui.common.SectionHeader
import com.crayen.domovoi.ui.common.highlighted
import com.crayen.domovoi.ui.common.watch
import com.crayen.domovoi.ui.nav.Navigator
import com.crayen.domovoi.ui.nav.Screen
import com.crayen.domovoi.ui.thread.DiffFileBody
import com.crayen.domovoi.ui.theme.LocalTints
import com.crayen.domovoi.ui.theme.Mono
import com.crayen.domovoi.ui.theme.Space

// ---- diff ----

/** The thread's changes, by source (all, a turn, uncommitted), file by file. */
@Composable
fun DiffScreen(core: Core, nav: Navigator, onBack: (() -> Unit)?) {
    val panel by core.view.panel.watch()
    val threadId by core.view.threadId.watch()
    val diff = panel.diff
    LaunchedEffect(Unit) { core.update(Event.ShowPanel("diff")) }
    ToolScaffold("Changes", onBack, actions = { IconButton(onClick = { core.update(Event.ShowPanel("diff")) }) { Icon(Icons.Outlined.Refresh, "Refresh") } }) { padding ->
        LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(top = padding.calculateTopPadding(), bottom = padding.calculateBottomPadding() + Space.xl)) {
            if (diff.sources.size > 1) item("sources") {
                Row(Modifier.horizontalScroll(rememberScrollState()).padding(horizontal = Space.l, vertical = Space.s), horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                    diff.sources.forEach { s -> FilterChip(selected = s.value == diff.selectedSource, onClick = { core.update(Event.SelectDiffSource(s.value)) }, label = { Text(s.label) }) }
                }
            }
            when {
                diff.loading && diff.files.isEmpty() -> item("loading") { Box(Modifier.fillMaxWidth().padding(Space.xxl), contentAlignment = Alignment.Center) { CircularProgressIndicator() } }
                diff.error != null -> item("error") { EmptyState(Icons.Outlined.Warning, "Couldn't load the changes", diff.error) }
                diff.files.isEmpty() -> item("none") { EmptyState(Icons.Outlined.CheckCircle, "No changes") }
            }
            items(diff.files, key = { it.path }) { file ->
                var open by rememberSaveable(file.path) { mutableStateOf(diff.files.size <= 3) }
                Column(Modifier.padding(horizontal = Space.m, vertical = Space.xs)) {
                    Surface(color = MaterialTheme.colorScheme.surfaceContainerLow, shape = MaterialTheme.shapes.medium) {
                        Column {
                            Row(
                                Modifier.fillMaxWidth().clickable { open = !open }.padding(horizontal = Space.m, vertical = Space.m),
                                verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.s),
                            ) {
                                Icon(Icons.Outlined.ChevronRight, null, modifier = Modifier.size(18.dp).rotate(if (open) 90f else 0f))
                                Text(file.path, fontFamily = Mono, fontSize = 13.sp, maxLines = 1, overflow = TextOverflow.StartEllipsis, modifier = Modifier.weight(1f))
                                Text("+${file.additions}", color = LocalTints.current.success, style = MaterialTheme.typography.labelMedium)
                                Text("−${file.deletions}", color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.labelMedium)
                            }
                            AnimatedVisibility(open) {
                                Box(Modifier.padding(start = Space.s, end = Space.s, bottom = Space.s)) {
                                    DiffFileBody(file, onLine = if (threadId == null) null else { text ->
                                        // Quoted into the thread's message, which opens again.
                                        core.update(Event.AppendToDraft(text))
                                        nav.back()
                                    })
                                }
                            }
                        }
                    }
                }
            }
            if (diff.truncated) item("truncated") { Text("Some changes are too large to show.", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(Space.l)) }
        }
    }
}

// ---- files ----

/** The project's files as a tree, with search and a preview. */
@Composable
fun FilesScreen(core: Core, onBack: (() -> Unit)?) {
    val panel by core.view.panel.watch()
    val files = panel.files
    var query by rememberSaveable { mutableStateOf(files.query) }
    var previewing by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(Unit) { core.update(Event.ShowPanel("files")) }
    ToolScaffold("Files", onBack) { padding ->
        Column(Modifier.fillMaxSize().padding(top = padding.calculateTopPadding())) {
            OutlinedTextField(
                value = query, onValueChange = { query = it; core.update(Event.SetFileQuery(it)) },
                placeholder = { Text("Find a file") }, leadingIcon = { Icon(Icons.Outlined.Search, null) }, singleLine = true,
                modifier = Modifier.fillMaxWidth().padding(horizontal = Space.l, vertical = Space.s),
            )
            if (files.loading && files.entries.isEmpty()) LinearProgressIndicator(Modifier.fillMaxWidth())
            val list = if (query.isNotEmpty()) files.results else files.entries
            LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(bottom = padding.calculateBottomPadding() + Space.xl)) {
                items(list, key = { it.path }) { entry -> FileRow(entry, flat = query.isNotEmpty()) {
                    if (entry.directory) core.update(Event.ToggleDirectory(entry.path)) else { previewing = entry.path; core.update(Event.OpenFile(entry.path)) }
                } }
            }
        }
    }
    files.preview?.takeIf { it.path == previewing }?.let { preview ->
        ModalBottomSheet(onDismissRequest = { previewing = null }) {
            Column(Modifier.padding(horizontal = Space.l)) {
                Text(preview.path, fontFamily = Mono, style = MaterialTheme.typography.titleSmall, maxLines = 2)
                Spacer(Modifier.height(Space.s))
                when {
                    preview.loading -> CircularProgressIndicator()
                    preview.error != null -> Text(preview.error!!, color = MaterialTheme.colorScheme.error)
                    else -> SelectionContainer {
                        Text(
                            highlighted(preview.spans, preview.text), fontFamily = Mono, fontSize = 12.sp, lineHeight = 16.sp, softWrap = false,
                            modifier = Modifier.verticalScroll(rememberScrollState()).horizontalScroll(rememberScrollState()),
                        )
                    }
                }
                if (preview.truncated) Text("Showing the start of the file.", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                Spacer(Modifier.height(Space.xl))
            }
        }
    }
}

@Composable
private fun FileRow(entry: FileEntryView, flat: Boolean, onClick: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().clickable(onClick = onClick)
            .padding(start = Space.l + if (flat) 0.dp else (entry.depth.toInt() * 16).dp, end = Space.l, top = Space.s, bottom = Space.s),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        Icon(
            when { entry.directory && entry.expanded -> Icons.Outlined.FolderOpen; entry.directory -> Icons.Outlined.Folder; else -> Icons.AutoMirrored.Outlined.InsertDriveFile },
            null, tint = if (entry.directory) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.size(20.dp),
        )
        Text(
            if (flat) entry.path else entry.name, style = MaterialTheme.typography.bodyMedium, maxLines = 1, overflow = TextOverflow.StartEllipsis,
            color = if (entry.ignored) MaterialTheme.colorScheme.outline else MaterialTheme.colorScheme.onSurface,
        )
    }
}

// ---- git ----

/** Branch, changes, commit, push, pull and pull requests (`GitScreen`). */
@Composable
fun GitScreen(core: Core, onBack: (() -> Unit)?) {
    val panel by core.view.panel.watch()
    val git = panel.git
    val uri = LocalUriHandler.current
    var message by rememberSaveable { mutableStateOf("") }
    var branchMenu by remember { mutableStateOf(false) }
    var newBranch by remember { mutableStateOf(false) }
    var confirming by remember { mutableStateOf<String?>(null) }
    val hasChanges = git.changedFiles.isNotEmpty()
    val hasMessage = message.isNotBlank()
    val canPush = git.hasRemote && (git.ahead > 0u || !git.hasUpstream)
    val tints = LocalTints.current
    fun run(action: String) {
        core.update(Event.GitAction(action, message))
        if (action.startsWith("commit")) message = ""
    }
    LaunchedEffect(Unit) { core.update(Event.ShowPanel("git")); core.update(Event.RefreshGit) }
    ToolScaffold("Git", onBack) { padding ->
        PullToRefreshBox(isRefreshing = false, onRefresh = { core.update(Event.RefreshGit) }, modifier = Modifier.padding(top = padding.calculateTopPadding())) {
            Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(bottom = Space.xxl)) {
                when {
                    git.loading -> Row(Modifier.padding(Space.l), horizontalArrangement = Arrangement.spacedBy(Space.m), verticalAlignment = Alignment.CenterVertically) {
                        CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp); Text("Reading the repository…", color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                    !git.isRepo -> EmptyState(Icons.AutoMirrored.Outlined.CallSplit, "Not a git repository")
                    else -> {
                        Box {
                            ListItem(
                                headlineContent = { Text("Branch") },
                                supportingContent = { Text(git.branch ?: "detached", fontFamily = Mono) },
                                leadingContent = { Icon(Icons.AutoMirrored.Outlined.CallSplit, null) },
                                trailingContent = { Icon(Icons.Outlined.UnfoldMore, null) },
                                modifier = Modifier.clickable(enabled = !git.busy) { branchMenu = true },
                            )
                            DropdownMenu(expanded = branchMenu, onDismissRequest = { branchMenu = false }) {
                                git.branches.forEach { b ->
                                    DropdownMenuItem(
                                        text = { Text(b, fontFamily = Mono) },
                                        trailingIcon = { if (b == git.branch) Icon(Icons.Filled.Check, null) },
                                        onClick = { branchMenu = false; core.update(Event.SwitchBranch(b)) },
                                    )
                                }
                                HorizontalDivider()
                                DropdownMenuItem(text = { Text("New branch…") }, leadingIcon = { Icon(Icons.Outlined.Add, null) }, onClick = { branchMenu = false; newBranch = true })
                            }
                        }
                        if (git.hasUpstream) ListItem(headlineContent = { Text("Upstream") }, trailingContent = { Text("↑${git.ahead}  ↓${git.behind}", style = MaterialTheme.typography.labelLarge) })
                        git.pullRequest?.let { pr ->
                            ListItem(
                                headlineContent = { Text("Pull request") },
                                supportingContent = { Text("#${pr.number} ${pr.title}", maxLines = 1, overflow = TextOverflow.Ellipsis) },
                                trailingContent = { Text(pr.state, style = MaterialTheme.typography.labelLarge) },
                                modifier = Modifier.clickable(enabled = pr.url.isNotEmpty()) { uri.openUri(pr.url) },
                            )
                        }
                        SectionHeader(if (hasChanges) "${git.changedFiles.size} changed · +${git.insertions} −${git.deletions}" else "Changes")
                        if (!hasChanges) Text("Working tree clean", color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(horizontal = Space.l, vertical = Space.s))
                        git.changedFiles.forEach { file ->
                            Row(Modifier.fillMaxWidth().padding(horizontal = Space.l, vertical = Space.xs), horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                                Text(file.path, fontFamily = Mono, fontSize = 13.sp, maxLines = 1, overflow = TextOverflow.StartEllipsis, modifier = Modifier.weight(1f))
                                Text("+${file.insertions}", color = tints.success, style = MaterialTheme.typography.labelMedium)
                                Text("−${file.deletions}", color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.labelMedium)
                            }
                        }
                        if (hasChanges) {
                            SectionHeader("Commit")
                            Column(Modifier.padding(horizontal = Space.l), verticalArrangement = Arrangement.spacedBy(Space.s)) {
                                OutlinedTextField(message, { message = it }, label = { Text("Commit message") }, minLines = 1, maxLines = 5, modifier = Modifier.fillMaxWidth(), supportingText = { if (!hasMessage) Text("Commits need a message.") })
                                Row(horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                                    Button(onClick = { run("commit") }, enabled = !git.busy && hasMessage) { Text("Commit") }
                                    if (git.hasRemote) FilledTonalButton(onClick = { confirming = "commit_push" }, enabled = !git.busy && hasMessage) { Text("Commit & push") }
                                }
                                if (git.hasRemote && git.pullRequest == null) OutlinedButton(onClick = { confirming = "commit_push_pr" }, enabled = !git.busy && hasMessage) { Text("Commit, push & open PR") }
                            }
                        }
                        if (git.hasRemote) {
                            SectionHeader("Remote")
                            Row(Modifier.padding(horizontal = Space.l), horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                                if (canPush) FilledTonalButton(onClick = { confirming = "push" }, enabled = !git.busy) {
                                    Text(if (git.hasUpstream) "Push ${git.ahead} commit${if (git.ahead == 1u) "" else "s"}" else "Publish branch")
                                }
                                OutlinedButton(onClick = { core.update(Event.GitPull) }, enabled = !git.busy && git.hasUpstream && git.behind > 0u) { Text("Pull") }
                                if (git.pullRequest == null) OutlinedButton(onClick = { run("create_pr") }, enabled = !git.busy && git.branch != null) { Text("Create PR") }
                            }
                        }
                        if (git.busy || git.progress.isNotEmpty() || git.error != null) {
                            Column(Modifier.padding(Space.l), verticalArrangement = Arrangement.spacedBy(Space.xs)) {
                                if (git.busy) LinearProgressIndicator(Modifier.fillMaxWidth())
                                git.progress.takeLast(6).forEach { Text(it, fontFamily = Mono, fontSize = 12.sp, color = MaterialTheme.colorScheme.onSurfaceVariant) }
                                git.error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
                            }
                        }
                    }
                }
            }
        }
    }
    confirming?.let { action ->
        AlertDialog(
            onDismissRequest = { confirming = null },
            title = { Text(if (action == "push") "Push ${git.branch ?: "the branch"}?" else "Commit and push to ${git.branch ?: "the branch"}?") },
            text = {
                Text(if (action == "push") "Pushes ${if (git.hasUpstream) "${git.ahead} commit(s)" else "the branch"} to the remote."
                else "Commits ${git.changedFiles.size} changed file(s) and pushes to the remote.")
            },
            confirmButton = {
                TextButton(onClick = { confirming = null; run(action) }) {
                    Text(when (action) { "commit_push_pr" -> "Commit, push & open PR"; "push" -> if (git.hasUpstream) "Push" else "Publish"; else -> "Commit & push" })
                }
            },
            dismissButton = { TextButton(onClick = { confirming = null }) { Text("Cancel") } },
        )
    }
    if (newBranch) {
        com.crayen.domovoi.ui.threads.NameDialog(title = "New branch", label = "Name", confirm = "Create", onDismiss = { newBranch = false }) {
            newBranch = false
            core.update(Event.CreateBranch(it))
        }
    }
}

// ---- pull requests ----

/** Open pull requests in the server's repositories; a tap shows one. */
@Composable
fun PullRequestsScreen(core: Core, nav: Navigator) {
    val prs by core.view.pullRequests.watch()
    val composer by core.view.composer.watch()
    var showing by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(Unit) { core.update(Event.LoadPullRequests) }
    // A thread started from a PR: its new-thread screen takes over.
    LaunchedEffect(composer.prWorktree) { if (composer.prWorktree.isNotEmpty()) nav.open(Screen.NewThread) }
    ToolScaffold("Pull requests", onBack = null, actions = { IconButton(onClick = { core.update(Event.LoadPullRequests) }) { Icon(Icons.Outlined.Refresh, "Refresh") } }) { padding ->
        PullToRefreshBox(isRefreshing = prs.loading && prs.items.isNotEmpty(), onRefresh = { core.update(Event.LoadPullRequests) }, modifier = Modifier.padding(top = padding.calculateTopPadding())) {
            LazyColumn(Modifier.fillMaxSize()) {
                prs.notice?.let { n -> item("notice") { Text(n, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(Space.l)) } }
                when {
                    prs.loading && prs.items.isEmpty() -> item("loading") { Box(Modifier.fillMaxWidth().padding(Space.xxl), contentAlignment = Alignment.Center) { CircularProgressIndicator() } }
                    prs.error != null -> item("error") { EmptyState(Icons.Outlined.Warning, "Couldn't load pull requests", prs.error) }
                    prs.items.isEmpty() -> item("none") { EmptyState(Icons.AutoMirrored.Outlined.CallSplit, "No open pull requests") }
                }
                items(prs.items, key = { it.key }) { pr ->
                    ListItem(
                        overlineContent = { Text("${pr.repository} #${pr.number}") },
                        headlineContent = { Text(pr.title, maxLines = 2, overflow = TextOverflow.Ellipsis) },
                        supportingContent = { Text(listOf(pr.author, "${pr.head} → ${pr.base}", pr.age).filter { it.isNotEmpty() }.joinToString(" · "), maxLines = 1, overflow = TextOverflow.Ellipsis) },
                        trailingContent = {
                            Column(horizontalAlignment = Alignment.End) {
                                if (pr.draft) Text("Draft", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                                if (pr.checks.isNotEmpty()) Text(pr.checks, style = MaterialTheme.typography.labelSmall)
                            }
                        },
                        modifier = Modifier.clickable { showing = pr.key; core.update(Event.SelectPullRequest(pr.key)) },
                    )
                }
            }
        }
    }
    prs.selected?.takeIf { it.key == showing }?.let { PullRequestDetailSheet(core, it, prs.busy) { showing = null } }
}

@Composable
private fun PullRequestDetailSheet(core: Core, pr: com.crayen.domovoi.app.PullRequestDetailView, busy: Boolean, onDismiss: () -> Unit) {
    val uri = LocalUriHandler.current
    var merge by remember { mutableStateOf<String?>(null) }
    var mergeMenu by remember { mutableStateOf(false) }
    var reviewing by remember { mutableStateOf(false) }
    var commenting by remember { mutableStateOf(false) }
    val tints = LocalTints.current
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(Modifier.verticalScroll(rememberScrollState()).padding(horizontal = Space.xl), verticalArrangement = Arrangement.spacedBy(Space.m)) {
            Text("#${pr.number} · ${pr.state}${if (pr.draft) " · draft" else ""}", style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.onSurfaceVariant)
            Text(pr.title, style = MaterialTheme.typography.titleLarge)
            Text("+${pr.additions} −${pr.deletions} · ${pr.changedFiles} files", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            if (busy || pr.loading) LinearProgressIndicator(Modifier.fillMaxWidth())
            Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                Button(onClick = { core.update(Event.StartPullRequestThread(pr.key)); onDismiss() }, enabled = !busy) { Text("Start thread") }
                if (pr.state.equals("open", true)) {
                    Box {
                        FilledTonalButton(onClick = { mergeMenu = true }, enabled = !busy && !pr.draft) { Text("Merge") }
                        DropdownMenu(expanded = mergeMenu, onDismissRequest = { mergeMenu = false }) {
                            listOf("merge" to "Create a merge commit", "squash" to "Squash and merge", "rebase" to "Rebase and merge").forEach { (m, label) ->
                                DropdownMenuItem(text = { Text(label) }, onClick = { mergeMenu = false; merge = m })
                            }
                        }
                    }
                    OutlinedButton(onClick = { reviewing = true }, enabled = !busy) { Text("Review") }
                }
                OutlinedButton(onClick = { commenting = true }, enabled = !busy) { Text("Comment") }
                if (pr.draft) OutlinedButton(onClick = { core.update(Event.PullRequestAction(pr.key, "ready", "")) }, enabled = !busy) { Text("Ready for review") }
                if (pr.url.isNotEmpty()) TextButton(onClick = { uri.openUri(pr.url) }) { Text("Open in browser") }
            }
            if (pr.checks.isNotEmpty()) {
                Text("Checks", style = MaterialTheme.typography.titleSmall)
                pr.checks.forEach { c ->
                    Row(horizontalArrangement = Arrangement.spacedBy(Space.s), verticalAlignment = Alignment.CenterVertically) {
                        val color = when (c.status) { "success", "pass" -> tints.success; "failure", "fail", "error" -> MaterialTheme.colorScheme.error; else -> tints.caution }
                        Box(Modifier.size(8.dp).padding(0.dp)) { Surface(color = color, shape = androidx.compose.foundation.shape.CircleShape, modifier = Modifier.fillMaxSize()) {} }
                        Text(c.name, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f))
                        Text(c.status, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                }
            }
            if (pr.body.isNotEmpty()) MarkdownBlocks(pr.body)
            Spacer(Modifier.height(Space.xl))
        }
    }
    merge?.let { method ->
        AlertDialog(
            onDismissRequest = { merge = null },
            title = { Text("Merge #${pr.number}?") },
            text = { Text(when (method) { "squash" -> "Squashes its commits into one on the base branch."; "rebase" -> "Rebases its commits onto the base branch."; else -> "Adds a merge commit to the base branch." }) },
            confirmButton = { TextButton(onClick = { merge = null; core.update(Event.PullRequestAction(pr.key, "merge", method)) }) { Text("Merge") } },
            dismissButton = { TextButton(onClick = { merge = null }) { Text("Cancel") } },
        )
    }
    if (reviewing) ReviewDialog(onDismiss = { reviewing = false }) { verdict, body -> reviewing = false; core.update(Event.ReviewPullRequest(pr.key, verdict, body)) }
    if (commenting) {
        var body by remember { mutableStateOf("") }
        AlertDialog(
            onDismissRequest = { commenting = false },
            title = { Text("Comment") },
            text = { OutlinedTextField(body, { body = it }, minLines = 3, modifier = Modifier.fillMaxWidth()) },
            confirmButton = { TextButton(onClick = { commenting = false; core.update(Event.CommentOnPullRequest(pr.key, body)) }, enabled = body.isNotBlank()) { Text("Comment") } },
            dismissButton = { TextButton(onClick = { commenting = false }) { Text("Cancel") } },
        )
    }
}

@Composable
private fun ReviewDialog(onDismiss: () -> Unit, submit: (String, String) -> Unit) {
    var verdict by remember { mutableStateOf("approve") }
    var body by remember { mutableStateOf("") }
    val verdicts = listOf("approve" to "Approve", "request-changes" to "Changes", "comment" to "Comment")
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Review") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(Space.m)) {
                SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
                    verdicts.forEachIndexed { i, (v, label) ->
                        SegmentedButton(selected = verdict == v, onClick = { verdict = v }, shape = SegmentedButtonDefaults.itemShape(i, verdicts.size)) { Text(label, maxLines = 1) }
                    }
                }
                OutlinedTextField(body, { body = it }, placeholder = { Text("Optional note") }, minLines = 3, modifier = Modifier.fillMaxWidth())
            }
        },
        confirmButton = { TextButton(onClick = { submit(verdict, body) }, enabled = verdict == "approve" || body.isNotBlank()) { Text("Submit") } },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

// ---- usage ----

/** Tokens used, by day and model. */
@Composable
fun UsageScreen(core: Core) {
    val usage by core.view.usage.watch()
    var days by rememberSaveable { mutableIntStateOf(7) }
    LaunchedEffect(days) { core.update(Event.LoadUsage(days.toUInt())) }
    ToolScaffold("Usage", onBack = null) { padding ->
        LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(top = padding.calculateTopPadding(), bottom = Space.xl)) {
            item("range") {
                val ranges = listOf(1 to "Today", 7 to "7 days", 30 to "30 days")
                SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth().padding(horizontal = Space.l, vertical = Space.s)) {
                    ranges.forEachIndexed { i, (d, label) ->
                        SegmentedButton(selected = days == d, onClick = { days = d }, shape = SegmentedButtonDefaults.itemShape(i, ranges.size)) { Text(label) }
                    }
                }
            }
            item("total") {
                Surface(color = MaterialTheme.colorScheme.primaryContainer, shape = MaterialTheme.shapes.large, modifier = Modifier.fillMaxWidth().padding(Space.l)) {
                    Column(Modifier.padding(Space.l)) {
                        Text(usage.rangeLabel.ifEmpty { "Total" }, style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.onPrimaryContainer)
                        Text(usage.totalTokens.ifEmpty { "—" }, style = MaterialTheme.typography.displaySmall, color = MaterialTheme.colorScheme.onPrimaryContainer)
                        Text("tokens", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onPrimaryContainer)
                    }
                }
            }
            if (usage.loading) item("loading") { LinearProgressIndicator(Modifier.fillMaxWidth().padding(horizontal = Space.l)) }
            if (usage.notice.isNotEmpty()) item("notice") { Text(usage.notice, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(horizontal = Space.l)) }
            var lastDay = ""
            usage.rows.forEachIndexed { i, row ->
                if (row.day != lastDay) {
                    lastDay = row.day
                    item("day/$i") { SectionHeader(row.day) }
                }
                item("row/$i") {
                    ListItem(
                        headlineContent = { Text(row.model) },
                        supportingContent = { Text("${row.provider} · in ${row.input} · cached ${row.cached} · out ${row.output}", maxLines = 1, overflow = TextOverflow.Ellipsis) },
                        trailingContent = { Text(row.total, style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.SemiBold) },
                        colors = ListItemDefaults.colors(containerColor = Color.Transparent),
                    )
                }
            }
        }
    }
}
