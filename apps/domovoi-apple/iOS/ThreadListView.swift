import App
import SwiftUI

/// The thread list, grouped by status (Inbox, Running, Inactive,
/// Archived) or by project, with search.
struct ThreadListView: View {
    @ObservedObject var core: Core
    let open: (Screen) -> Void
    let expectProject: () -> Void
    @State private var search = ""
    @State private var askingName = false
    /// Projects whose earlier sessions are shown.
    @State private var showingEarlier: Set<String> = []
    /// A thread whose snooze time is being picked (presets, then a date).
    @State private var snoozing: ThreadRowView?
    @Environment(\.dynamicTypeSize) private var typeSize
    @State private var pickingSnoozeDate: ThreadRowView?
    @State private var deleting: ThreadRowView?
    @State private var iconProject: SidebarProjectView?
    @ScaledMetric(relativeTo: .body) private var avatarSize = Size.avatar
    /// A section header's icon column and the project tile in it, growing
    /// with the header's text.
    @ScaledMetric(relativeTo: .subheadline) private var headerIconWidth = Size.rowIcon
    @ScaledMetric(relativeTo: .subheadline) private var headerTile: CGFloat = 18
    @Environment(\.sidebarOpen) private var sidebarOpen

    private var actions: AddProjectActions {
        AddProjectActions(core: core, open: open, expectProject: expectProject, askName: { askingName = true })
    }

    private var sidebar: SidebarView { core.view.sidebar }
    /// Grouped by what each thread needs, not by project.
    private var byStatus: Bool { sidebar.grouping == "status" }
    /// No thread listed (in any section or project).
    private var nothingListed: Bool {
        sidebar.sections.allSatisfy { $0.count == 0 } && sidebar.projects.allSatisfy { $0.threads.isEmpty && $0.earlier.isEmpty }
    }
    private var connection: ConnectionView { core.view.connection }
    /// How the server is reached, under the title: "Direct · 29 ms".
    private var routeLine: String? {
        switch connection.state {
        case "connected" where connection.remote && !connection.path.isEmpty:
            return "\(connection.path.capitalized) · \(connection.rttMs) ms"
        case "connected": return nil
        case "reconnecting": return "Reconnecting…"
        case "connecting", "pairing": return "Connecting…"
        default: return connection.state.capitalized
        }
    }

    /// Search hits are tagged apart from rows, so the open thread isn't
    /// selected twice.
    private static let hitTag = "hit:"

    /// iPad: the list's own selection (highlight, arrow keys, VoiceOver's
    /// "selected") opens threads in the detail column.
    private var selection: Binding<String?>? {
        guard let sidebarOpen else { return nil }
        return Binding(
            get: { core.view.route == "thread" ? sidebar.selectedThreadId : nil },
            set: { tag in
                guard let tag else { return }
                let id = tag.hasPrefix(Self.hitTag) ? String(tag.dropFirst(Self.hitTag.count)) : tag
                sidebarOpen(.thread(id))
            }
        )
    }

    var body: some View {
        List(selection: selection) {
            // Nothing in titles (the list below filters) nor in messages.
            if !search.isEmpty, !sidebar.searching, sidebar.searchResults.isEmpty, nothingListed {
                ContentUnavailableView.search(text: search)
            }
            if !search.isEmpty, sidebar.searching || !sidebar.searchResults.isEmpty {
                Section(sidebar.searching ? "Searching…" : "Results") {
                    let listed = Dictionary(sidebar.listedRows.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
                    ForEach(sidebar.searchResults, id: \.threadId) { hit in
                        // The same leading column as a thread row, so titles line up.
                        let label = HStack(alignment: .top, spacing: Space.s) {
                            leadingColumn(unread: false) { hitAvatar(listed[hit.threadId]) }
                            VStack(alignment: .leading, spacing: Space.xs) {
                                Text(hit.title).font(TypeRole.rowTitle).foregroundStyle(.primary)
                                Text("\(hit.projectTitle) · \(hit.snippet)").font(TypeRole.meta).foregroundStyle(.secondary).lineLimit(2)
                            }
                            .alignmentGuide(.listRowSeparatorLeading) { d in d[.leading] }
                        }
                        Group {
                            if sidebarOpen != nil {
                                label.tag(Self.hitTag + hit.threadId)
                            } else {
                                NavigationLink(value: Screen.thread(hit.threadId)) { label }
                            }
                        }
                        .modifier(ThreadListRowInsets())
                    }
                }
            }
            if !sidebar.terminalAttention.isEmpty {
                Section("Terminal activity") {
                    ForEach(sidebar.terminalAttention, id: \.instanceId) { terminal in
                        Button {
                            core.update(.openTerminalAttention(threadId: terminal.threadId, terminalId: terminal.terminalId))
                            let navigate = sidebarOpen ?? open
                            navigate(.thread(terminal.threadId))
                            navigate(.terminal)
                        } label: {
                            VStack(alignment: .leading) {
                                Text(terminal.title ?? terminal.label).lineLimit(1)
                                Text(terminal.agent?.activity == "needsInput" ? "Needs input" : terminal.agent?.activity == "finished" ? "Finished" : "Working")
                                    .font(.caption).foregroundStyle(.secondary)
                            }
                        }
                    }
                }
            }
            ForEach(byStatus ? sidebar.sections : [], id: \.kind) { section in
                // The core leaves a collapsed section's rows out; the header
                // (a button: an inset list has no disclosure of its own) toggles it.
                Section {
                    ForEach(section.rows, id: \.id) { row($0, showsProject: true) }
                    if section.hiddenCount > 0 {
                        showMore("Show \(section.hiddenCount) more") { core.update(.showAllInSection(section: section.kind)) }
                    }
                } header: {
                    Button { core.update(.toggleSection(section: section.kind)) } label: {
                        HStack(spacing: Space.tight) {
                            DisclosureChevron(open: section.expanded)
                            sectionTitle(section.title) { SectionGlyph(kind: section.kind, count: section.count) }
                            if section.count > 0 { CountCapsule(count: section.count) }
                        }
                        .tapTarget()
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("\(section.title), \(section.count) threads")
                    .accessibilityValue(section.expanded ? "Expanded" : "Collapsed")
                    .accessibilityHint(section.expanded ? "Collapses the section" : "Expands the section")
                }
            }
            // What waits on the user (approvals, questions), wherever it is
            // (grouped by status, the Inbox lists it first).
            let waiting = byStatus ? [] : sidebar.projects.flatMap(\.threads).filter { $0.status == "approval" }
            if search.isEmpty, !waiting.isEmpty {
                Section {
                    ForEach(waiting, id: \.id) { row($0) }
                } header: {
                    // Orange on the icon only: orange text is hard to read in light mode.
                    sectionTitle("Needs You") { SectionGlyph(kind: "needs-you") }
                }
            }
            ForEach(byStatus ? [] : sidebar.projects, id: \.id) { project in
                Section {
                    ForEach(project.threads, id: \.id) { thread in
                        row(thread)
                    }
                    if project.hiddenCount > 0 {
                        showMore("Show \(project.hiddenCount) more") { core.update(.showAllThreads(projectId: project.id)) }
                    }
                    if project.earlierCount > 0 {
                        earlier(project)
                    }
                } header: {
                    HStack(spacing: Space.tight) {
                        Button { iconProject = project } label: {
                            ProjectIconTile(icon: project.icon, size: headerTile, core: core)
                                .iconColumn(headerIconWidth)
                                .tapTarget()
                        }.buttonStyle(.plain).accessibilityLabel("Change icon for \(project.title)")
                        Text(project.title).lineLimit(1).truncationMode(.middle)
                        Spacer(minLength: Space.s)
                        Button { newThread(project.id) } label: {
                            Image(systemName: "square.and.pencil").tapTarget(width: true)
                        }
                        .accessibilityLabel("New thread in \(project.title)")
                    }
                    .contextMenu { Button("Change Icon…") { iconProject = project } }
                }
            }
            if !byStatus, !sidebar.snoozed.isEmpty, search.isEmpty {
                Section {
                    ForEach(sidebar.snoozed, id: \.id) { row($0) }
                } header: {
                    sectionTitle("Snoozed") { SectionGlyph(kind: "snoozed") }
                }
            }
            // Settled threads show under their project ("Earlier sessions").
            if sidebar.unreadOnly, search.isEmpty, nothingListed {
                ContentUnavailableView {
                    Label("All caught up", systemImage: "checkmark.circle")
                } description: {
                    Text("No thread has news you haven't seen.")
                } actions: {
                    Button("Show All Threads") { core.update(.setUnreadOnly(on: false)) }
                }
            } else if sidebar.projects.isEmpty, connection.state == "connected", search.isEmpty {
                emptyState
            } else if sidebar.projects.isEmpty, search.isEmpty {
                // Nothing saved from last time and no server yet: say so
                // rather than showing an empty list.
                ContentUnavailableView {
                    if ["connecting", "reconnecting", "pairing"].contains(connection.state) {
                        Label { Text("Connecting…") } icon: { ProgressView() }
                    } else {
                        Label("Can't Reach the Server", systemImage: "network.slash")
                    }
                } description: {
                    if let error = connection.error { Text(error) }
                } actions: {
                    if !["connecting", "reconnecting", "pairing"].contains(connection.state) {
                        Button("Try Again") {
                            if connection.activeEnvironmentId.isEmpty { core.update(.networkChanged) } else { core.update(.selectEnvironment(id: connection.activeEnvironmentId)) }
                        }
                    }
                }
            }
        }
        // One dialog for the list: a thread that needs you is listed twice
        // (under Needs You and its project), and two dialogs can't both show.
        .confirmationDialog("Snooze until…", isPresented: Binding(get: { snoozing != nil }, set: { if !$0 { snoozing = nil } }), titleVisibility: .visible, presenting: snoozing) { thread in
            snoozeButtons(thread)
            Button("Pick a Date…") { pickingSnoozeDate = thread }
        }
        .sheet(item: Binding(get: { pickingSnoozeDate.map(SnoozingThread.init) }, set: { pickingSnoozeDate = $0?.thread })) { item in
            SnoozeDatePicker(title: item.thread.title) { ms in
                core.update(.snoozeThread(threadId: item.thread.id, untilMs: ms))
                pickingSnoozeDate = nil
            } cancel: { pickingSnoozeDate = nil }
        }
        .confirmationDialog("Delete “\(deleting?.title ?? "this thread")”?", isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } }), titleVisibility: .visible, presenting: deleting) { thread in
            Button("Delete", role: .destructive) { core.update(.deleteThread(threadId: thread.id)) }
        } message: { _ in
            Text("Its conversation is gone for good. Files it changed stay as they are.")
        }
        .modifier(NewProjectPrompt(isPresented: $askingName) { name in
            expectProject()
            core.update(.createProject(name: name))
        })
        // The import picker's count, for the empty state and the + menu.
        .task(id: connection.state) {
            if connection.state == "connected" { core.update(.scanAgentSessions) }
        }
        .modifier(ThreadListStyle(sidebar: sidebarOpen != nil))
        .sheet(isPresented: Binding(get: { iconProject != nil }, set: { if !$0 { iconProject = nil } })) {
            if let project = iconProject { ProjectIconPicker(project: project, core: core).standardSheet() }
        }
        .searchable(text: $search, prompt: "Search threads")
        .onChange(of: search) { _, query in core.update(.setSearch(query: query)) }
        // Back on the list (or a new one, on iPad): the field shows the
        // search the core still filters by.
        .onAppear { if search != sidebar.searchQuery { search = sidebar.searchQuery } }
        // Connected: catch up on anything missed. Disconnected: try now
        // instead of waiting for the next retry.
        .refreshable { await core.refresh(.resync) { core.view.connection.state != "connected" } }
        .navigationTitle(connection.environmentLabel.isEmpty ? "Domovoi" : connection.environmentLabel)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) {
                VStack(spacing: 0) {
                    Text(connection.environmentLabel.isEmpty ? "Domovoi" : connection.environmentLabel).font(.headline)
                    if let route = routeLine {
                        Text(route).font(TypeRole.metaSmall).foregroundStyle(.secondary)
                    }
                }
                .accessibilityElement(children: .combine)
            }
            ToolbarItem(placement: .topBarLeading) {
                ServerMenu(core: core, open: open)
            }
            ToolbarItemGroup(placement: .topBarTrailing) {
                // ⌘K is the Thread menu's (a hardware keyboard).
                Button { core.update(.openPalette) } label: { Image(systemName: "command") }
                    .accessibilityLabel("Command Palette")
                Menu {
                    Toggle("Unread Only", isOn: Binding(get: { sidebar.unreadOnly }, set: { core.update(.setUnreadOnly(on: $0)) }))
                    Picker("Group By", selection: Binding(get: { sidebar.grouping }, set: { core.update(.setSidebarGrouping(grouping: $0)) })) {
                        Label("Status", systemImage: "tray.2").tag("status")
                        Label("Project", systemImage: "folder").tag("project")
                    }
                } label: {
                    Image(systemName: sidebar.unreadOnly ? "line.3.horizontal.decrease.circle.fill" : "line.3.horizontal.decrease.circle")
                }
                .accessibilityLabel("Filter and group threads")
                Menu { actions.menuItems } label: { Image(systemName: "plus") }
                    .accessibilityLabel("Add project")
                Button { newThread(sidebar.selectedProjectId ?? sidebar.projects.first?.id) } label: {
                    Image(systemName: "square.and.pencil")
                }
                .keyboardShortcut("n", modifiers: .command)
                .disabled(sidebar.projects.isEmpty)
                .accessibilityLabel("New thread")
            }
        }
    }

    @ViewBuilder
    private func link(_ thread: ThreadRowView, @ViewBuilder label: () -> some View) -> some View {
        if sidebarOpen != nil {
            // iPad: a selectable row; the list opens it in the detail column.
            label().tag(thread.id)
        } else {
            NavigationLink(value: Screen.thread(thread.id), label: label)
        }
    }

    /// A thread's row; `showsProject` under a status section, where no
    /// header names the project.
    private func row(_ thread: ThreadRowView, showsProject: Bool = false) -> some View {
        let subtitle = thread.subtitle(showsProject: showsProject)
        return link(thread) {
            HStack(alignment: .top, spacing: Space.s) {
                leadingColumn(unread: thread.unread) {
                    if showsProject { ThreadAvatar(icon: thread.projectIcon, status: thread.status, size: rowAvatar, core: core) }
                    else { StatusIcon(status: thread.status).frame(width: rowAvatar, height: rowAvatar) }
                }
                VStack(alignment: .leading, spacing: Space.xs) {
                    HStack(alignment: .firstTextBaseline, spacing: Space.s) {
                        Text(thread.title.isEmpty ? "New thread" : thread.title).lineLimit(typeSize.isAccessibilitySize ? 4 : 2)
                            .font(TypeRole.rowTitle).fontWeight(thread.unread ? .semibold : .regular)
                        if !typeSize.isAccessibilitySize { Spacer(minLength: 0); rowTime(thread) }
                    }
                    if typeSize.isAccessibilitySize {
                        Text(subtitle).font(TypeRole.meta).foregroundStyle(.secondary).lineLimit(2)
                        HStack(spacing: Space.xs) {
                            ThreadRowBadges(thread: thread)
                            if thread.relation == "fork" { Image(systemName: "arrow.triangle.branch") }
                            if !thread.source.isEmpty { Text(thread.source).lineLimit(1).minimumScaleFactor(0.7) }
                            if thread.pinned { Image(systemName: "pin.fill") }
                        }.font(TypeRole.metaSmall).foregroundStyle(.secondary)
                    } else {
                        HStack(spacing: Space.xs) {
                            Text(subtitle.isEmpty ? " " : subtitle).font(TypeRole.meta).foregroundStyle(.secondary).lineLimit(1)
                            Spacer(minLength: 0)
                            ThreadRowBadges(thread: thread).font(TypeRole.metaSmall)
                            if thread.relation == "fork" { Image(systemName: "arrow.triangle.branch").font(TypeRole.metaSmall).foregroundStyle(.secondary) }
                            if !thread.source.isEmpty { SourceBadge(source: thread.source) }
                            if thread.pinned { Image(systemName: "pin.fill").font(TypeRole.metaSmall).foregroundStyle(.secondary) }
                        }
                    }
                    if typeSize.isAccessibilitySize { rowTime(thread) }
                }
                .alignmentGuide(.listRowSeparatorLeading) { d in d[.leading] }
            }
            .accessibilityElement(children: .ignore).accessibilityLabel([thread.spokenLabel, thread.spokenBadges].filter { !$0.isEmpty }.joined(separator: ", "))
        }
        .modifier(ThreadListRowInsets())
        .swipeActions(edge: .trailing) {
            Button(thread.archived ? "Unarchive" : "Archive") { core.update(.archiveThread(threadId: thread.id, archived: !thread.archived)) }
                .tint(.gray)
            if thread.snoozedUntilMs > 0 {
                Button("Wake") { core.update(.snoozeThread(threadId: thread.id, untilMs: 0)) }.tint(.purple)
            } else {
                Button("Snooze") { snoozing = thread }.tint(.purple)
            }
            Button(settleLabel(thread)) { core.update(.settleThread(threadId: thread.id, settled: !thread.settled)) }
                .tint(.indigo)
        }
        .swipeActions(edge: .leading) {
            Button(thread.pinned ? "Unpin" : "Pin") { core.update(.pinThread(threadId: thread.id, pinned: !thread.pinned)) }
                .tint(.orange)
            if !thread.unread {
                Button("Unread") { core.update(.markThreadUnread(threadId: thread.id)) }.tint(.blue)
            }
        }
        .contextMenu { rowMenu(thread) }
    }

    /// The avatar, no bigger than a tap target at the largest text sizes.
    private var rowAvatar: CGFloat { min(avatarSize, Size.tap) }

    /// A row's leading column: the unread dot, centred on the avatar beside it.
    private func leadingColumn(unread: Bool, @ViewBuilder avatar: () -> some View) -> some View {
        HStack(spacing: Space.s) {
            Circle().fill(unread ? Color.accentColor : .clear).frame(width: Size.dot, height: Size.dot).accessibilityHidden(true)
            avatar()
        }
    }

    /// A search hit's avatar: its thread's when the list has it.
    @ViewBuilder
    private func hitAvatar(_ row: ThreadRowView?) -> some View {
        if let row {
            ThreadAvatar(icon: row.projectIcon, status: row.status, size: rowAvatar, core: core)
        } else {
            Image(systemName: "text.magnifyingglass").foregroundStyle(.secondary).frame(width: rowAvatar, height: rowAvatar)
        }
    }

    /// A section header's title: its glyph in a fixed column, so every
    /// header's text starts at the same place.
    private func sectionTitle(_ title: String, @ViewBuilder glyph: () -> some View) -> some View {
        HStack(spacing: Space.tight) {
            glyph().iconColumn(headerIconWidth)
            Text(title)
            Spacer(minLength: Space.s)
        }
    }

    /// "Show N more" under a section: one look for every such row.
    private func showMore(_ title: String, action: @escaping () -> Void) -> some View {
        Button(title, action: action)
            .font(.footnote)
            .modifier(ThreadListRowInsets())
    }

    private func rowTime(_ thread: ThreadRowView) -> some View {
        Text(thread.statusLabel.isEmpty ? (thread.snoozedUntilMs > 0 ? SnoozeTimes.label(thread.snoozedUntilMs) : thread.age) : thread.statusLabel)
            .font(TypeRole.meta).monospacedDigit().lineLimit(1).fixedSize()
            .foregroundStyle(thread.statusTint).fontWeight(thread.isPlanning ? .medium : nil)
            .planningPill(thread)
    }

    /// Everything a row can do, on a long press.
    @ViewBuilder
    private func rowMenu(_ thread: ThreadRowView) -> some View {
        Section {
            Button(thread.pinned ? "Unpin" : "Pin", systemImage: thread.pinned ? "pin.slash" : "pin") {
                core.update(.pinThread(threadId: thread.id, pinned: !thread.pinned))
            }
            if !thread.unread {
                Button("Mark as Unread", systemImage: "circlebadge.fill") { core.update(.markThreadUnread(threadId: thread.id)) }
            }
            if thread.snoozedUntilMs > 0 {
                Button("Wake Now", systemImage: "sun.max") { core.update(.snoozeThread(threadId: thread.id, untilMs: 0)) }
            } else {
                Menu {
                    snoozeButtons(thread)
                    Button("Pick a Date…") { pickingSnoozeDate = thread }
                } label: { Label("Snooze", systemImage: "moon.zzz") }
            }
            Button(settleLabel(thread), systemImage: thread.settled ? "tray.and.arrow.up" : "tray.and.arrow.down") {
                core.update(.settleThread(threadId: thread.id, settled: !thread.settled))
            }
        }
        Section {
            Button("Copy Thread ID", systemImage: "doc.on.doc") { Platform.copy(thread.id) }
            Button(thread.archived ? "Unarchive" : "Archive", systemImage: "archivebox") {
                core.update(.archiveThread(threadId: thread.id, archived: !thread.archived))
            }
            Button("Delete…", systemImage: "trash", role: .destructive) { deleting = thread }
        }
    }

    /// Use the status-list action label when grouped by status.
    private func settleLabel(_ thread: ThreadRowView) -> String {
        if byStatus { return thread.settled ? "Move to Inbox" : "Set Aside" }
        return thread.settled ? "Unsettle" : "Settle"
    }

    @ViewBuilder
    private func snoozeButtons(_ thread: ThreadRowView) -> some View {
        ForEach(SnoozeTimes.presets(), id: \.label) { preset in
            Button(preset.label) { core.update(.snoozeThread(threadId: thread.id, untilMs: preset.ms)) }
        }
    }

    /// A project's settled threads (imported sessions among them), folded.
    @ViewBuilder
    private func earlier(_ project: SidebarProjectView) -> some View {
        let shown = showingEarlier.contains(project.id)
        // A project with no Domovoi threads shows its newest sessions right away.
        let inline = project.threads.isEmpty ? Array(project.earlier.prefix(3)) : []
        ForEach(inline, id: \.id) { thread in row(thread) }
        if Int(project.earlierCount) > inline.count {
            earlierToggle(project, shown: shown, more: Int(project.earlierCount) - inline.count)
        }
        if shown {
            ForEach(project.earlier.dropFirst(inline.count), id: \.id) { thread in row(thread) }
            if project.earlier.count < Int(project.earlierCount) {
                showMore("Show all \(project.earlierCount)") { core.update(.showAllThreads(projectId: project.id)) }
            }
        }
    }

    private func earlierToggle(_ project: SidebarProjectView, shown: Bool, more: Int) -> some View {
        Button {
            withAnimation {
                if shown { showingEarlier.remove(project.id) } else { showingEarlier.insert(project.id) }
            }
        } label: {
            HStack {
                Text(project.threads.isEmpty ? "\(more) more session\(more == 1 ? "" : "s")" : "Earlier sessions (\(more))")
                Spacer()
                Image(systemName: "chevron.right")
                    .rotationEffect(.degrees(shown ? 90 : 0))
                    .font(.caption)
                    .foregroundStyle(.tertiary)
            }
        }
        .font(.footnote)
        .foregroundStyle(.secondary)
        .modifier(ThreadListRowInsets())
    }

    private var emptyState: some View {
        VStack(spacing: Space.l) {
            ContentUnavailableView {
                Label("No projects yet", systemImage: "folder")
            } description: {
                Text(core.view.agentSessions.available > 0
                    ? "The server has Claude Code or Codex sessions in \(core.view.agentSessions.available) folders."
                    : "Pick a folder on the server, start a new project, or import your Claude Code sessions.")
            }
            VStack(spacing: Space.s) {
                Button(action: actions.importSessions) {
                    Label("Import from Claude Code", systemImage: "square.and.arrow.down").frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                Button(action: actions.browse) {
                    Label("Add Folder on Server", systemImage: "folder.badge.plus").frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                HStack(spacing: Space.s) {
                    Button(action: actions.newProject) { Text("New Project").frame(maxWidth: .infinity) }
                    Button(action: actions.withoutProject) { Text("No Project").frame(maxWidth: .infinity) }
                }
                .buttonStyle(.bordered)
            }
            .controlSize(.large)
        }
        // The buttons keep the list's side margin instead of running edge to edge.
        .padding(.horizontal, Space.gutter)
        .padding(.vertical, Space.xl)
        .listRowBackground(Color.clear)
        .listRowInsets(EdgeInsets(top: 0, leading: 0, bottom: 0, trailing: 0))
    }

    private func newThread(_ projectId: String?) {
        core.update(.newThread(projectId: projectId))
        open(.newThread)
    }
}

/// The connection's state and route, saved servers and app screens.
struct ServerMenu: View {
    @ObservedObject var core: Core
    @EnvironmentObject private var companion: CompanionBrain
    let open: (Screen) -> Void

    private var connection: ConnectionView { core.view.connection }

    var body: some View {
        Menu {
            Section(statusLine) {
                ForEach(connection.environments, id: \.id) { env in
                    Button {
                        core.update(.selectEnvironment(id: env.id))
                    } label: {
                        Label(env.label, systemImage: env.active ? "checkmark" : (env.remote ? "globe" : "desktopcomputer"))
                    }
                }
            }
            Button { open(.issues) } label: { Label("Issues", systemImage: "number") }
            Button { open(.pullRequests) } label: { Label("Pull Requests", systemImage: "arrow.triangle.pull") }
            Button { open(.usage) } label: { Label("Usage", systemImage: "chart.bar") }
            Button { open(.settings) } label: { Label("Settings", systemImage: "gearshape") }
        } label: {
            CompanionView(look: connection.companion.look, brain: companion, size: Size.avatar, tracksPointer: true)
        }
        .accessibilityLabel(connection.companionServerLabel)
        .accessibilityValue(connection.companionVoiceOverValue(companion.expression.mood))
    }

    private var statusLine: String {
        var line = connection.state.capitalized
        if connection.remote, !connection.path.isEmpty { line += " · \(connection.path) · \(connection.rttMs) ms" }
        return line
    }
}

/// A row whose snooze date is being picked (`sheet(item:)` needs Identifiable).
private struct SnoozingThread: Identifiable {
    let thread: ThreadRowView
    var id: String { thread.id }
}

/// A date and time to snooze until.
private struct SnoozeDatePicker: View {
    let title: String
    let done: (Int64) -> Void
    let cancel: () -> Void
    @State private var date = Calendar.current.date(byAdding: .day, value: 1, to: .now) ?? .now

    var body: some View {
        NavigationStack {
            Form {
                DatePicker("Wake", selection: $date, in: Date.now.addingTimeInterval(60)..., displayedComponents: [.date, .hourAndMinute])
                    .datePickerStyle(.graphical)
            }
            .navigationTitle("Snooze “\(title)”")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel", action: cancel) }
                ToolbarItem(placement: .confirmationAction) { Button("Snooze") { done(Int64(date.timeIntervalSince1970 * 1000)) } }
            }
        }
        .presentationDetents([.large])
    }
}

/// Every row of the thread list (threads, search hits, "Show more"): one
/// vertical inset, and the system's side margins, which the section headers
/// use too, so rows and headers start at the same edge on every device.
private struct ThreadListRowInsets: ViewModifier {
    func body(content: Content) -> some View {
        content.listRowInsets(.vertical, Space.s)
    }
}

private struct ThreadListStyle: ViewModifier {
    let sidebar: Bool
    @ViewBuilder func body(content: Content) -> some View {
        if sidebar { content.listStyle(.sidebar) } else { content.listStyle(.plain) }
    }
}
