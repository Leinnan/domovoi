package com.crayen.domovoi.ui.nav

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.Chat
import androidx.compose.material.icons.automirrored.filled.Chat
import androidx.compose.material.icons.filled.BarChart
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.outlined.BarChart
import androidx.compose.material.icons.outlined.ChatBubbleOutline
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material.icons.outlined.MergeType
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material.icons.filled.MergeType
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.adaptive.currentWindowAdaptiveInfo
import androidx.compose.material3.adaptive.navigation3.ListDetailSceneStrategy
import androidx.compose.material3.adaptive.navigation3.rememberListDetailSceneStrategy
import androidx.compose.material3.adaptive.navigationsuite.NavigationSuiteScaffold
import androidx.compose.material3.adaptive.navigationsuite.NavigationSuiteScaffoldDefaults
import androidx.compose.material3.adaptive.navigationsuite.NavigationSuiteType
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import com.crayen.domovoi.system.AppRequest
import com.crayen.domovoi.system.AppRequests
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.isCtrlPressed
import androidx.compose.ui.input.key.isShiftPressed
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.SharedFlow
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshots.SnapshotStateList
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.navigation3.runtime.entryProvider
import androidx.navigation3.ui.NavDisplay
import androidx.window.core.layout.WindowSizeClass
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.core.Core
import com.crayen.domovoi.ui.common.ArtifactsScreen
import com.crayen.domovoi.ui.common.EmptyState
import com.crayen.domovoi.ui.common.ToastEffect
import com.crayen.domovoi.ui.common.watch
import com.crayen.domovoi.ui.pairing.PairingScreen
import com.crayen.domovoi.ui.thread.NewThreadScreen
import com.crayen.domovoi.ui.thread.ThreadScreen
import com.crayen.domovoi.ui.projects.FolderBrowserScreen
import com.crayen.domovoi.ui.projects.ImportSessionsScreen
import com.crayen.domovoi.ui.settings.AccessScreen
import com.crayen.domovoi.ui.settings.ArchivedThreadsScreen
import com.crayen.domovoi.ui.settings.MaintenanceScreen
import com.crayen.domovoi.ui.settings.ProvidersScreen
import com.crayen.domovoi.ui.settings.ScheduledTasksScreen
import com.crayen.domovoi.ui.settings.ServerHubScreen
import com.crayen.domovoi.ui.settings.ServerProjectsScreen
import com.crayen.domovoi.ui.settings.ServerSettingsScreen
import com.crayen.domovoi.ui.settings.SettingsScreen
import com.crayen.domovoi.ui.terminal.TerminalScreen
import com.crayen.domovoi.ui.threads.ThreadListPane
import com.crayen.domovoi.ui.tools.DiffScreen
import com.crayen.domovoi.ui.tools.FilesScreen
import com.crayen.domovoi.ui.tools.GitScreen
import com.crayen.domovoi.ui.tools.IssuesScreen
import com.crayen.domovoi.ui.tools.PullRequestsScreen
import com.crayen.domovoi.ui.tools.UsageScreen

/** The app's top-level places, in the navigation bar (phone) or rail (tablet). */
enum class Tab(val label: String, val icon: ImageVector, val selectedIcon: ImageVector, val root: Screen) {
    Threads("Threads", Icons.Outlined.ChatBubbleOutline, Icons.AutoMirrored.Filled.Chat, Screen.Threads),
    PullRequests("Pull requests", Icons.Outlined.MergeType, Icons.Filled.MergeType, Screen.PullRequests),
    Usage("Usage", Icons.Outlined.BarChart, Icons.Filled.BarChart, Screen.Usage),
    Settings("Settings", Icons.Outlined.Settings, Icons.Filled.Settings, Screen.Settings),
}

/** Screens that belong in a list-detail layout's extra pane, beside the thread. */
private val toolScreens = setOf(Screen.Diff, Screen.Artifacts, Screen.Terminal, Screen.Files, Screen.Git)

/**
 * The connected app: a navigation bar or rail of tabs; in Threads, the list
 * and the open thread side by side when there is room (and a third pane for
 * the thread's changes, terminal, files and git on wide screens), stacked on
 * a phone.
 */
@Composable
fun MainScaffold(core: Core) {
    var tab by rememberSaveable { mutableStateOf(Tab.Threads) }
    val stacks = remember { Tab.entries.associateWith { mutableStateListOf(it.root) } }
    val stack = stacks.getValue(tab)
    val snackbars = remember { SnackbarHostState() }
    val sidebar by core.view.sidebar.watch()
    val connection by core.view.connection.watch()
    val palette by core.view.palette.watch()
    val windowInfo = currentWindowAdaptiveInfo()
    val expanded = windowInfo.windowSizeClass.isWidthAtLeastBreakpoint(WindowSizeClass.WIDTH_DP_MEDIUM_LOWER_BOUND)

    ToastEffect(core, snackbars)
    com.crayen.domovoi.ui.thread.PullRequestReviewDialog(core)

    val nav = remember(stack) {
        object : Navigator {
            override fun open(screen: Screen) {
                if (stack.lastOrNull() != screen) stack.add(screen)
            }
            override fun back() {
                if (stack.size > 1) stack.removeAt(stack.lastIndex) else if (tab != Tab.Threads) tab = Tab.Threads
            }
            override fun show(screen: Screen) {
                // The detail replaces whatever detail (and tools) were open.
                while (stack.size > 1) stack.removeAt(stack.lastIndex)
                stack.add(screen)
            }
        }
    }
    val threadsNav = stacks.getValue(Tab.Threads).let { threads ->
        remember(threads) {
            object : Navigator {
                override fun open(screen: Screen) {
                    // Usage is a tab of its own (opened from a thread's usage meter).
                    if (screen == Screen.Usage) { tab = Tab.Usage; return }
                    tab = Tab.Threads
                    if (threads.lastOrNull() != screen) threads.add(screen)
                }
                override fun back() { if (threads.size > 1) threads.removeAt(threads.lastIndex) }
                override fun show(screen: Screen) {
                    tab = Tab.Threads
                    while (threads.size > 1) threads.removeAt(threads.lastIndex)
                    threads.add(screen)
                }
            }
        }
    }

    // Shared from another app, the New thread shortcut, a notification or a
    // link: acted on once the server's projects are there.
    val request by AppRequests.pending.collectAsState()
    LaunchedEffect(request, sidebar.projects.isEmpty()) {
        when (val r = request) {
            null -> Unit
            is AppRequest.OpenThread -> { AppRequests.take(); threadsNav.show(Screen.Thread(r.id)) }
            is AppRequest.NewThread -> if (sidebar.projects.isNotEmpty()) {
                AppRequests.take()
                core.update(Event.NewThread(sidebar.selectedProjectId ?: sidebar.projects.firstOrNull()?.id))
                if (r.text.isNotEmpty()) core.update(Event.SetDraft(r.text))
                r.images.forEach { core.update(Event.AddAttachment(it.name, "image/jpeg", it.sizeBytes.toULong(), it.dataUrl)) }
                threadsNav.show(Screen.NewThread)
            }
        }
    }
    // Another server: reset thread/PR/usage stacks, but stay inside the
    // server hub when the user switched servers from Settings.
    var lastEnvironment by remember { mutableStateOf(connection.activeEnvironmentId) }
    LaunchedEffect(connection.activeEnvironmentId) {
        if (connection.activeEnvironmentId == lastEnvironment) return@LaunchedEffect
        val newId = connection.activeEnvironmentId
        lastEnvironment = newId
        stacks.forEach { (stackTab, s) ->
            if (stackTab == Tab.Settings) {
                if (s.size > 1) {
                    while (s.size > 1) s.removeAt(s.lastIndex)
                    if (newId.isNotEmpty()) s.add(Screen.ServerHub(newId))
                }
            } else {
                while (s.size > 1) s.removeAt(s.lastIndex)
            }
        }
    }
    val composer by core.view.composer.watch()
    var seenIssueHandoff by remember { mutableStateOf(composer.issueHandoff) }
    LaunchedEffect(composer.issueHandoff) {
        val changed = composer.issueHandoff != seenIssueHandoff
        seenIssueHandoff = composer.issueHandoff
        if (changed && composer.issueHandoff != 0u && core.view.route.value == "new-thread") threadsNav.show(Screen.NewThread)
    }

    // The core picked a thread elsewhere (a fork, a PR thread).
    LaunchedEffect(sidebar.selectedThreadId) {
        val id = sidebar.selectedThreadId ?: return@LaunchedEffect
        val threads = stacks.getValue(Tab.Threads)
        val showing = threads.any { it == Screen.Thread(id) }
        if (!showing && threads.lastOrNull() !is Screen.NewThread && threads.size > 1) threadsNav.show(Screen.Thread(id))
    }

    val inDetail = tab == Tab.Threads && stack.size > 1
    val layout = when {
        expanded -> NavigationSuiteScaffoldDefaults.navigationSuiteType(windowInfo)
        inDetail || stack.size > 1 -> NavigationSuiteType.None
        else -> NavigationSuiteType.NavigationBar
    }
    val searchRequests = remember { MutableSharedFlow<Unit>(extraBufferCapacity = 1) }
    // A palette item ran: the core changed its own state; where the app goes is ours.
    fun runPalette(id: String) {
        core.update(Event.RunPaletteItem(id))
        val threads = stacks.getValue(Tab.Threads)
        when {
            id.startsWith("thread:") -> threadsNav.show(Screen.Thread(id.removePrefix("thread:")))
            id.startsWith("project:") -> threadsNav.show(Screen.NewThread)
            // A file or a search match: the core opens it in the files panel.
            id.startsWith("file:") || id.startsWith("match:") -> threadsNav.open(Screen.Files)
            else -> when (id) {
                "action:new-thread", "action:scratch" -> threadsNav.show(Screen.NewThread)
                "action:diff" -> threadsNav.open(Screen.Diff)
                "action:artifacts" -> threadsNav.open(Screen.Artifacts)
                "action:files" -> threadsNav.open(Screen.Files)
                "action:git" -> threadsNav.open(Screen.Git)
                "action:terminal" -> threadsNav.open(Screen.Terminal)
                "action:toggle-panel" -> if (tab == Tab.Threads && threads.lastOrNull() in toolScreens) threadsNav.back() else threadsNav.open(Screen.Diff)
                "action:find" -> sidebar.selectedThreadId?.let { open ->
                    if (tab != Tab.Threads || threads.lastOrNull() != Screen.Thread(open)) threadsNav.show(Screen.Thread(open))
                }
                "action:pull-requests" -> tab = Tab.PullRequests
                "action:usage" -> tab = Tab.Usage
                else -> Unit // pin, settle; go-to-file and search-project keep the palette open
            }
        }
    }
    // Hardware keyboard: Ctrl+N new thread, Ctrl+F find in the thread, Ctrl+K the
    // command palette, Ctrl+Shift+K search threads.
    val shortcuts = Modifier.onPreviewKeyEvent { e ->
        if (e.type != KeyEventType.KeyDown || !e.isCtrlPressed) return@onPreviewKeyEvent false
        when (e.key) {
            Key.N -> {
                core.update(Event.NewThread(sidebar.selectedProjectId ?: sidebar.projects.firstOrNull()?.id))
                threadsNav.show(Screen.NewThread)
                true
            }
            Key.F -> {
                val open = tab == Tab.Threads && stacks.getValue(Tab.Threads).lastOrNull() is Screen.Thread
                if (open) core.update(Event.FindInThread(""))
                open
            }
            Key.K -> {
                if (!e.isShiftPressed) {
                    core.update(Event.OpenPalette)
                    return@onPreviewKeyEvent true
                }
                tab = Tab.Threads
                if (!expanded) stacks.getValue(Tab.Threads).let { s -> while (s.size > 1) s.removeAt(s.lastIndex) }
                searchRequests.tryEmit(Unit)
                true
            }
            else -> false
        }
    }
    CompositionLocalProvider(LocalSearchRequests provides searchRequests) {
    NavigationSuiteScaffold(
        modifier = shortcuts,
        layoutType = layout,
        navigationSuiteItems = {
            Tab.entries.forEach { t ->
                item(
                    selected = t == tab,
                    onClick = {
                        if (t == tab) { val s = stacks.getValue(t); while (s.size > 1) s.removeAt(s.lastIndex) } else tab = t
                    },
                    icon = { Icon(if (t == tab) t.selectedIcon else t.icon, null) },
                    label = { Text(t.label) },
                    badge = if (t == Tab.Threads && sidebar.threadsNeedingAttention > 0u) {
                        { androidx.compose.material3.Badge { Text("${sidebar.threadsNeedingAttention}") } }
                    } else null,
                )
            }
        },
    ) {
        Content(core, tab, stack, nav, threadsNav, snackbars, expanded)
    }
    palette?.let { PaletteDialog(it, expanded, { e -> core.update(e) }, ::runPalette) }
    }
}

/** Ctrl+Shift+K: the thread list's search field takes focus. */
val LocalSearchRequests = staticCompositionLocalOf<SharedFlow<Unit>> { MutableSharedFlow() }

@Composable
private fun Content(core: Core, tab: Tab, stack: SnapshotStateList<Screen>, nav: Navigator, threadsNav: Navigator, snackbars: SnackbarHostState, expanded: Boolean) {
    val strategy = rememberListDetailSceneStrategy<Screen>()
    val selected = (stack.lastOrNull { it is Screen.Thread } as? Screen.Thread)?.id
    NavDisplay(
        backStack = stack,
        onBack = { nav.back() },
        sceneStrategies = listOf(strategy),
        entryProvider = entryProvider {
            entry<Screen.Threads>(metadata = ListDetailSceneStrategy.listPane(detailPlaceholder = { NoThreadOpen(core, threadsNav) })) {
                ThreadListPane(core, threadsNav, snackbars, selected)
            }
            entry<Screen.Thread>(metadata = ListDetailSceneStrategy.detailPane()) { key ->
                ThreadScreen(core, key.id, threadsNav, snackbars, showBack = !expanded)
            }
            entry<Screen.NewThread>(metadata = ListDetailSceneStrategy.detailPane()) {
                NewThreadScreen(core, threadsNav, snackbars, showBack = !expanded)
            }
            entry<Screen.AddServer> { PairingScreen(core, onboarding = false, onBack = nav::back) }
            val tool = ListDetailSceneStrategy.extraPane()
            entry<Screen.Diff>(metadata = tool) { DiffScreen(core, threadsNav, onBack = threadsNav::back) }
            entry<Screen.Artifacts>(metadata = tool) { ArtifactsScreen(core, onBack = threadsNav::back) }
            entry<Screen.Files>(metadata = tool) { FilesScreen(core, onBack = threadsNav::back) }
            entry<Screen.Git>(metadata = tool) { GitScreen(core, onBack = threadsNav::back, openTerminal = { threadsNav.open(Screen.Terminal) }) }
            entry<Screen.Terminal>(metadata = tool) { TerminalScreen(core, onBack = threadsNav::back) }
            entry<Screen.ImportSessions> { ImportSessionsScreen(core, onBack = nav::back) }
            entry<Screen.BrowseFolders> { FolderBrowserScreen(core, threadsNav, onBack = nav::back) }
            entry<Screen.Issues> { IssuesScreen(core, threadsNav) }
            entry<Screen.PullRequests> { PullRequestsScreen(core, threadsNav) }
            entry<Screen.Usage> { UsageScreen(core) }
            entry<Screen.Settings> { SettingsScreen(core, nav) }
            entry<Screen.ServerHub> { key -> ServerHubScreen(core, key.environmentId, nav) }
            entry<Screen.ServerSettings> { ServerSettingsScreen(core, onBack = nav::back) }
            entry<Screen.ServerProjects> { ServerProjectsScreen(core, nav, onBack = nav::back) }
            entry<Screen.ArchivedThreads> { ArchivedThreadsScreen(core, onBack = nav::back) }
            entry<Screen.Providers> { ProvidersScreen(core, onBack = nav::back) }
            entry<Screen.ScheduledTasks> { ScheduledTasksScreen(core, onBack = nav::back, openThread = { threadsNav.show(Screen.Thread(it)) }) }
            entry<Screen.Maintenance> { MaintenanceScreen(core, onBack = nav::back) }
            entry<Screen.Access> { AccessScreen(core, onBack = nav::back) }
        },
    )
}

@Composable
private fun NoThreadOpen(core: Core, nav: Navigator) {
    val sidebar by core.view.sidebar.watch()
    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        EmptyState(Icons.Outlined.ChatBubbleOutline, "No thread open", "Pick one from the list, or start a new one.") {
            Button(onClick = {
                core.update(Event.NewThread(sidebar.selectedProjectId ?: sidebar.projects.firstOrNull()?.id))
                nav.show(Screen.NewThread)
            }) {
                Icon(Icons.Outlined.Edit, null); Text("  New thread")
            }
        }
    }
}
