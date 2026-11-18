package com.crayen.domovoi.core

import com.crayen.domovoi.app.AgentSessionsView
import com.crayen.domovoi.app.ComposerView
import com.crayen.domovoi.app.ConnectionView
import com.crayen.domovoi.app.FolderBrowserView
import com.crayen.domovoi.app.PanelView
import com.crayen.domovoi.app.PullRequestsView
import com.crayen.domovoi.app.SettingsView
import com.crayen.domovoi.app.SidebarView
import com.crayen.domovoi.app.ItemView
import com.crayen.domovoi.app.ThreadChange
import com.crayen.domovoi.app.ThreadPageView
import com.crayen.domovoi.app.ToastView
import com.crayen.domovoi.app.UsageView
import com.crayen.domovoi.app.ViewDelta
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * The core's view model, split so Compose observes each part on its own: a
 * streaming reply recomposes the thread, not the thread list behind it.
 * Built from the core's first view (every part), then each [ViewDelta]
 * (what changed since the one before) is applied in order. Rows that did
 * not change stay the same objects, so their composables are skipped.
 */
class LiveView(first: ViewDelta) {
    private val rows = ThreadPageState()

    private val _connection = MutableStateFlow(requireNotNull(first.connection) { FIRST })
    private val _toast = MutableStateFlow(requireNotNull(first.toast) { FIRST }.toast)
    private val _sidebar = MutableStateFlow(requireNotNull(first.sidebar) { FIRST })
    private val _route = MutableStateFlow(requireNotNull(first.route) { FIRST })
    private val _thread = MutableStateFlow(first.thread?.let(rows::apply))
    private val _threadId = MutableStateFlow(_thread.value?.id)
    private val _composer = MutableStateFlow(requireNotNull(first.composer) { FIRST })
    private val _retryBlocked = MutableStateFlow(_composer.value.blockedReason.isNotEmpty())
    private val _panel = MutableStateFlow(requireNotNull(first.panel) { FIRST })
    private val _settings = MutableStateFlow(requireNotNull(first.settings) { FIRST })
    private val _pullRequests = MutableStateFlow(requireNotNull(first.pullRequests) { FIRST })
    private val _usage = MutableStateFlow(requireNotNull(first.usage) { FIRST })
    private val _agentSessions = MutableStateFlow(requireNotNull(first.agentSessions) { FIRST })
    private val _folderBrowser = MutableStateFlow(requireNotNull(first.folderBrowser) { FIRST })

    val connection: StateFlow<ConnectionView> = _connection.asStateFlow()
    val toast: StateFlow<ToastView?> = _toast.asStateFlow()
    val sidebar: StateFlow<SidebarView> = _sidebar.asStateFlow()
    val route: StateFlow<String> = _route.asStateFlow()
    val thread: StateFlow<ThreadPageView?> = _thread.asStateFlow()

    /** The open thread's id, apart from the thread: readers that only need
     *  which thread is open don't recompose on every streamed token. */
    val threadId: StateFlow<String?> = _threadId.asStateFlow()
    val composer: StateFlow<ComposerView> = _composer.asStateFlow()

    /** Retry is off (the provider can't run), apart from the composer: the
     *  transcript doesn't recompose on every keystroke. */
    val retryBlocked: StateFlow<Boolean> = _retryBlocked.asStateFlow()
    val panel: StateFlow<PanelView> = _panel.asStateFlow()
    val settings: StateFlow<SettingsView> = _settings.asStateFlow()
    val pullRequests: StateFlow<PullRequestsView> = _pullRequests.asStateFlow()
    val usage: StateFlow<UsageView> = _usage.asStateFlow()
    val agentSessions: StateFlow<AgentSessionsView> = _agentSessions.asStateFlow()
    val folderBrowser: StateFlow<FolderBrowserView> = _folderBrowser.asStateFlow()

    /** Applies one delta (on the renders dispatcher, one at a time, in order). */
    fun apply(delta: ViewDelta) {
        delta.connection?.let { _connection.value = it }
        delta.toast?.let { _toast.value = it.toast }
        delta.sidebar?.let { _sidebar.value = it }
        delta.route?.let { _route.value = it }
        delta.thread?.let {
            val page = rows.apply(it)
            _thread.value = page
            _threadId.value = page?.id
        }
        delta.composer?.let {
            _composer.value = it
            _retryBlocked.value = it.blockedReason.isNotEmpty()
        }
        delta.panel?.let { _panel.value = it }
        delta.settings?.let { _settings.value = it }
        delta.pullRequests?.let { _pullRequests.value = it }
        delta.usage?.let { _usage.value = it }
        delta.agentSessions?.let { _agentSessions.value = it }
        delta.folderBrowser?.let { _folderBrowser.value = it }
    }

    /** The core was replaced: its next delta is a whole view, not a patch
     *  of the thread page built so far. */
    fun reset() = rows.reset()

    private companion object {
        const val FIRST = "the core's first view carries every part"
    }
}

/**
 * The thread page as the core's deltas have built it: the header and rows
 * last sent, and where each row is.
 */
class ThreadPageState {
    private var page: ThreadPageView? = null
    private var index: Map<String, Int> = emptyMap()

    fun reset() {
        page = null
        index = emptyMap()
    }

    /** Applies a change and returns the page it leads to. */
    fun apply(change: ThreadChange): ThreadPageView? {
        when (change) {
            ThreadChange.Closed -> {
                page = null
                index = emptyMap()
            }
            is ThreadChange.Page -> {
                val patch = change.value
                // The core sends a new page with its header; without one there
                // is nothing to patch, and the page stays as it is.
                val next = patch.header ?: page ?: return page
                val old = page?.items.orEmpty()
                val order = patch.order
                val items: List<ItemView> = if (order != null) {
                    val byId = HashMap<String, ItemView>(old.size + patch.rows.size)
                    for (row in old) byId.putIfAbsent(row.id, row)
                    for (row in patch.rows) byId[row.id] = row
                    // Each id once, as the core keeps them: a lazy list with a
                    // key twice throws.
                    val seen = HashSet<String>(order.size)
                    order.mapNotNull { id -> if (seen.add(id)) byId[id] else null }.also { list ->
                        index = HashMap<String, Int>(list.size).apply { list.forEachIndexed { i, r -> putIfAbsent(r.id, i) } }
                    }
                } else if (patch.rows.isEmpty()) {
                    old
                } else {
                    old.toMutableList().apply { for (row in patch.rows) index[row.id]?.let { this[it] = row } }
                }
                page = next.copy(items = items)
            }
        }
        return page
    }
}
