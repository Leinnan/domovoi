package com.crayen.domovoi.ui.projects

import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.ArrowUpward
import androidx.compose.material.icons.outlined.Download
import androidx.compose.material.icons.outlined.Folder
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.style.TextOverflow
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.core.Core
import com.crayen.domovoi.ui.common.EmptyState
import com.crayen.domovoi.ui.common.SectionHeader
import com.crayen.domovoi.ui.common.watch
import com.crayen.domovoi.ui.nav.Navigator
import com.crayen.domovoi.ui.nav.Screen
import com.crayen.domovoi.ui.theme.Space
import com.crayen.domovoi.ui.tools.ToolScaffold

/** Folders with Claude Code or Codex sessions, to import as projects. */
@Composable
fun ImportSessionsScreen(core: Core, onBack: () -> Unit) {
    val sessions by core.view.agentSessions.watch()
    var chosen by remember { mutableStateOf<Set<String>?>(null) }
    LaunchedEffect(Unit) { core.update(Event.ScanAgentSessions) }
    // Recent folders start ticked, once the scan is in.
    LaunchedEffect(sessions.scanned) {
        if (sessions.scanned && chosen == null) chosen = sessions.candidates.filter { it.recent && !it.alreadyImported }.map { it.path }.toSet()
    }
    val picked = chosen.orEmpty()
    ToolScaffold(
        "Import sessions", onBack,
        actions = { IconButton(onClick = { core.update(Event.ScanAgentSessions) }) { Icon(Icons.Outlined.Refresh, "Scan again") } },
        bottomBar = {
            Surface(tonalElevation = 3.dp()) {
                Column(Modifier.fillMaxWidth().navigationBarsPadding().padding(Space.l), verticalArrangement = Arrangement.spacedBy(Space.s)) {
                    if (sessions.importing) {
                        LinearProgressIndicator(progress = { if (sessions.total > 0u) sessions.done.toFloat() / sessions.total.toFloat() else 0f }, modifier = Modifier.fillMaxWidth())
                        Text("Importing ${sessions.done} of ${sessions.total}…", style = MaterialTheme.typography.bodySmall)
                    }
                    sessions.error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
                    Button(onClick = { core.update(Event.ImportAgentSessions(picked.toList())) }, enabled = picked.isNotEmpty() && !sessions.importing, modifier = Modifier.fillMaxWidth()) {
                        Icon(Icons.Outlined.Download, null); Text("  Import ${picked.size} folder${if (picked.size == 1) "" else "s"}")
                    }
                }
            }
        },
    ) { padding ->
        LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(top = padding.calculateTopPadding(), bottom = padding.calculateBottomPadding())) {
            if (sessions.scanning && sessions.candidates.isEmpty()) item("scan") { Box(Modifier.fillMaxWidth().padding(Space.xxl), contentAlignment = Alignment.Center) { CircularProgressIndicator() } }
            else if (sessions.scanned && sessions.candidates.isEmpty()) item("none") { EmptyState(Icons.Outlined.Folder, "No sessions found", "The server has no Claude Code or Codex sessions to import.") }
            val (fresh, done) = sessions.candidates.partition { !it.alreadyImported }
            if (fresh.isNotEmpty()) item("new-h") { SectionHeader("Not imported") }
            items(fresh, key = { it.path }) { c ->
                val on = c.path in picked
                ListItem(
                    headlineContent = { Text(c.title) },
                    supportingContent = { Text(listOf(c.detail, c.sources, "${c.threadCount} sessions").filter { it.isNotEmpty() }.joinToString(" · "), maxLines = 2, overflow = TextOverflow.Ellipsis) },
                    leadingContent = { Checkbox(checked = on, onCheckedChange = null) },
                    colors = ListItemDefaults.colors(containerColor = Color.Transparent),
                    modifier = Modifier.clickable { chosen = if (on) picked - c.path else picked + c.path },
                )
            }
            if (done.isNotEmpty()) item("done-h") { SectionHeader("Already projects") }
            items(done, key = { "done/${it.path}" }) { c ->
                ListItem(
                    headlineContent = { Text(c.title) },
                    supportingContent = { Text(c.detail, maxLines = 1, overflow = TextOverflow.Ellipsis) },
                    colors = ListItemDefaults.colors(containerColor = Color.Transparent),
                )
            }
        }
    }
}

private fun Int.dp() = androidx.compose.ui.unit.Dp(this.toFloat())

/** The server's folders: add one as a project, or start a thread in it. */
@Composable
fun FolderBrowserScreen(core: Core, nav: Navigator, onBack: () -> Unit) {
    val browser by core.view.folderBrowser.watch()
    LaunchedEffect(Unit) { core.update(Event.BrowseFolders(browser.path)) }
    val crumbs = remember(browser.path) {
        val parts = browser.path.split('/').filter { it.isNotEmpty() }
        parts.indices.map { i -> parts[i] to "/" + parts.take(i + 1).joinToString("/") }
    }
    ToolScaffold(
        "Add folder", onBack,
        bottomBar = {
            Surface(tonalElevation = 3.dp()) {
                Box(Modifier.fillMaxWidth().navigationBarsPadding().padding(Space.l)) {
                    if (browser.isProject) {
                        Button(onClick = {
                            core.update(Event.AddProject(browser.path))
                            nav.show(Screen.NewThread)
                        }, modifier = Modifier.fillMaxWidth()) { Text("Start a thread here") }
                    } else {
                        Button(onClick = { core.update(Event.AddProject(browser.path)); onBack() }, enabled = browser.path.isNotEmpty(), modifier = Modifier.fillMaxWidth()) { Text("Add this folder") }
                    }
                }
            }
        },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(top = padding.calculateTopPadding(), bottom = padding.calculateBottomPadding())) {
            Row(Modifier.horizontalScroll(rememberScrollState()).padding(horizontal = Space.l, vertical = Space.s), horizontalArrangement = Arrangement.spacedBy(Space.xs)) {
                AssistChip(onClick = { core.update(Event.BrowseFolders("/")) }, label = { Text("/") })
                crumbs.forEach { (name, path) -> AssistChip(onClick = { core.update(Event.BrowseFolders(path)) }, label = { Text(name) }) }
            }
            if (browser.loading) LinearProgressIndicator(Modifier.fillMaxWidth())
            browser.error?.let { Text(it, color = MaterialTheme.colorScheme.error, modifier = Modifier.padding(Space.l)) }
            LazyColumn(Modifier.fillMaxSize()) {
                if (browser.parentPath.isNotEmpty() && browser.parentPath != browser.path) item("up") {
                    ListItem(
                        headlineContent = { Text("Up") }, leadingContent = { Icon(Icons.Outlined.ArrowUpward, null) },
                        colors = ListItemDefaults.colors(containerColor = Color.Transparent),
                        modifier = Modifier.clickable { core.update(Event.BrowseFolders(browser.parentPath)) },
                    )
                }
                items(browser.entries, key = { it.fullPath }) { e ->
                    ListItem(
                        headlineContent = { Text(e.name) }, leadingContent = { Icon(Icons.Outlined.Folder, null, tint = MaterialTheme.colorScheme.primary) },
                        colors = ListItemDefaults.colors(containerColor = Color.Transparent),
                        modifier = Modifier.clickable { core.update(Event.BrowseFolders(e.fullPath)) },
                    )
                }
            }
        }
    }
}
