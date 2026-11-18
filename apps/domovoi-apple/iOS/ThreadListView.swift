import App
import SwiftUI

/// The thread list, grouped by status (Inbox, Running, Hibernating,
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
                    ForEach(sidebar.searchResults, id: \.threadId) { hit in
                        let label = VStack(alignment: .leading, spacing: 2) {
                            Text(hit.title).foregroundStyle(.primary)
                            Text("\(hit.projectTitle) · \(hit.snippet)").font(.caption).foregroundStyle(.secondary).lineLimit(2)
                        }
                        if sidebarOpen != nil {
                            label.tag(Self.hitTag + hit.threadId)
                        } else {
                            NavigationLink(value: Screen.thread(hit.threadId)) { label }
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
                        Button("Show \(section.hiddenCount) more") { core.update(.showAllInSection(section: section.kind)) }
                            .font(.footnote)
                    }
                } header: {
                    Button { core.update(.toggleSection(section: section.kind)) } label: {
                        HStack(spacing: 6) {
                            Image(systemName: "chevron.right")
                                .font(.caption.weight(.bold))
                                .rotationEffect(.degrees(section.expanded ? 90 : 0))
                            Text(section.title)
                            if section.kind == "running", section.count > 0 { ProgressView().controlSize(.mini) }
                            Spacer()
                            if section.count > 0 { Text("\(section.count)").monospacedDigit().foregroundStyle(.secondary) }
                        }
                        .contentShape(Rectangle())
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
                    Label { Text("Needs You") } icon: { Image(systemName: "hand.raised.fill").foregroundStyle(Tint.caution) }
                }
            }
            ForEach(byStatus ? [] : sidebar.projects, id: \.id) { project in
                Section {
                    ForEach(project.threads, id: \.id) { thread in
                        row(thread)
                    }
                    if project.hiddenCount > 0 {
                        Button("Show \(project.hiddenCount) more") { core.update(.showAllThreads(projectId: project.id)) }
                            .font(.footnote)
                    }
                    if project.earlierCount > 0 {
                        earlier(project)
                    }
                } header: {
                    HStack {
                        Text(project.title).lineLimit(1).truncationMode(.middle)
                        Spacer()
                        Button { newThread(project.id) } label: {
                            Image(systemName: "square.and.pencil")
                                .frame(minWidth: 44, minHeight: 32)
                                .contentShape(Rectangle())
                        }
                        .accessibilityLabel("New thread in \(project.title)")
                    }
                }
            }
            if !byStatus, !sidebar.snoozed.isEmpty, search.isEmpty {
                Section {
                    ForEach(sidebar.snoozed, id: \.id) { row($0) }
                } header: {
                    Label("Snoozed", systemImage: "moon.zzz").foregroundStyle(.secondary)
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
        .listStyle(.insetGrouped)
        .searchable(text: $search, prompt: "Search threads")
        .onChange(of: search) { _, query in core.update(.setSearch(query: query)) }
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
                        Text(route).font(.caption2).foregroundStyle(.secondary)
                    }
                }
                .accessibilityElement(children: .combine)
            }
            ToolbarItem(placement: .topBarLeading) {
                ServerMenu(core: core, open: open)
            }
            ToolbarItemGroup(placement: .topBarTrailing) {
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
        let subtitle = [showsProject ? thread.projectTitle : "", thread.modelLabel].filter { !$0.isEmpty }.joined(separator: " · ")
        return link(thread) {
            HStack(spacing: 10) {
                StatusIcon(status: thread.status, unread: thread.unread)
                VStack(alignment: .leading, spacing: 2) {
                    Text(thread.title).lineLimit(typeSize.isAccessibilitySize ? 4 : 2)
                        .fontWeight(thread.unread ? .semibold : .regular)
                    if !subtitle.isEmpty {
                        Text(subtitle).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                    }
                    if let branch = thread.branch {
                        HStack(spacing: 3) {
                            Image(systemName: "arrow.triangle.branch")
                            Text(branch)
                        }
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                    }
                    // Large text: the details go under the title, which keeps
                    // the row's width.
                    if typeSize.isAccessibilitySize {
                        HStack(spacing: 6) { rowDetails(thread) }
                    }
                }
                Spacer()
                if !typeSize.isAccessibilitySize { rowDetails(thread) }
            }
        }
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

    /// Fork, agent, pin and age (or when a snooze ends).
    @ViewBuilder
    private func rowDetails(_ thread: ThreadRowView) -> some View {
        if thread.relation == "fork" {
            Image(systemName: "arrow.triangle.branch").font(.caption2).foregroundStyle(.secondary).accessibilityLabel("Fork")
        }
        if !thread.source.isEmpty { SourceBadge(source: thread.source) }
        if thread.pinned { Image(systemName: "pin.fill").font(.caption2).foregroundStyle(.secondary).accessibilityLabel("Pinned") }
        if !thread.statusLabel.isEmpty {
            Text(thread.statusLabel).font(.caption.weight(.medium)).monospacedDigit()
                .foregroundStyle(thread.status == "failed" ? Tint.danger : .secondary)
        } else if thread.snoozedUntilMs > 0 {
            Text(SnoozeTimes.label(thread.snoozedUntilMs)).font(.caption).foregroundStyle(.secondary)
        } else {
            Text(thread.age).font(.caption).foregroundStyle(.secondary)
        }
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

    /// Settling, as the grouping names it: grouped by status, a settled
    /// thread hibernates.
    private func settleLabel(_ thread: ThreadRowView) -> String {
        if byStatus { return thread.settled ? "Move to Inbox" : "Hibernate" }
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
                Button("Show all \(project.earlierCount)") { core.update(.showAllThreads(projectId: project.id)) }
                    .font(.footnote)
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
                    .foregroundStyle(.secondary)
            }
        }
        .font(.subheadline)
        .foregroundStyle(.secondary)
    }

    private var emptyState: some View {
        VStack(spacing: 16) {
            ContentUnavailableView {
                Label("No projects yet", systemImage: "folder")
            } description: {
                Text(core.view.agentSessions.available > 0
                    ? "The server has Claude Code or Codex sessions in \(core.view.agentSessions.available) folders."
                    : "Pick a folder on the server, start a new project, or import your Claude Code sessions.")
            }
            VStack(spacing: 10) {
                Button(action: actions.importSessions) {
                    Label("Import from Claude Code", systemImage: "square.and.arrow.down").frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                Button(action: actions.browse) {
                    Label("Add Folder on Server", systemImage: "folder.badge.plus").frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                HStack(spacing: 10) {
                    Button(action: actions.newProject) { Text("New Project").frame(maxWidth: .infinity) }
                    Button(action: actions.withoutProject) { Text("No Project").frame(maxWidth: .infinity) }
                }
                .buttonStyle(.bordered)
            }
            .controlSize(.large)
        }
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
            Button { open(.pullRequests) } label: { Label("Pull Requests", systemImage: "arrow.triangle.pull") }
            Button { open(.usage) } label: { Label("Usage", systemImage: "chart.bar") }
            Button { open(.settings) } label: { Label("Settings", systemImage: "gearshape") }
        } label: {
            Image(systemName: icon).foregroundStyle(color)
        }
        .accessibilityLabel("Server")
        // The icon's colour is the state; say it too.
        .accessibilityValue(statusLine)
    }

    private var statusLine: String {
        var line = connection.state.capitalized
        if connection.remote, !connection.path.isEmpty { line += " · \(connection.path) · \(connection.rttMs) ms" }
        return line
    }

    private var icon: String {
        connection.remote ? "globe" : "desktopcomputer"
    }

    private var color: Color {
        switch connection.state {
        case "connected": return .green
        case "reconnecting", "connecting": return .orange
        default: return .secondary
        }
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
