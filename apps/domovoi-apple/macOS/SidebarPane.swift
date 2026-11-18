import App
import AppKit
import SwiftUI

struct SidebarPane: View {
    @ObservedObject var core: Core
    @State private var search = ""
    @State private var renaming: ThreadRowView?
    @State private var renameText = ""
    @State private var newProjectName = ""
    @State private var showNewProject = false
    @State private var showImport = false
    @State private var showBrowser = false
    /// Projects whose earlier sessions are shown.
    @State private var earlierShown: Set<String> = []
    @State private var deleting: ThreadRowView?

    /// The row just clicked, shown selected until the core's selection
    /// catches up (arrow keys otherwise flicker back to the old row).
    @State private var picked: String?

    private var sidebar: SidebarView { core.view.sidebar }

    var body: some View {
        #if DEBUG
        let _ = DebugViews.printChanges ? Self._printChanges() : ()
        #endif
        List(selection: Binding(
            get: { picked ?? sidebar.selectedThreadId },
            set: { tag in
                // Search hits are tagged apart from the rows, so one thread
                // is never selected twice in the list.
                guard let tag else { return }
                picked = tag
                let id = tag.hasPrefix(Self.hitTag) ? String(tag.dropFirst(Self.hitTag.count)) : tag
                if id != sidebar.selectedThreadId { core.update(.selectThread(threadId: id)) }
            }
        )) {
            if !sidebar.searchQuery.isEmpty, !sidebar.searchResults.isEmpty {
                Section("Messages") {
                    ForEach(sidebar.searchResults, id: \.self) { hit in
                        VStack(alignment: .leading, spacing: Space.xxs) {
                            Text(hit.title).lineLimit(1)
                            Text(hit.snippet).font(.caption).foregroundStyle(.secondary).lineLimit(2)
                            Text(hit.projectTitle).font(.caption2).foregroundStyle(.tertiary)
                        }
                        .padding(.vertical, Space.xxs)
                        .tag(Self.hitTag + hit.threadId)
                    }
                }
            }
            if byStatus {
                ForEach(sidebar.sections, id: \.kind) { section in
                    Section {
                        if section.expanded {
                            ForEach(section.rows, id: \.id) { thread in
                                ThreadRow(thread: thread, showsProject: true).equatable()
                                    .tag(thread.id)
                                    .contextMenu { threadMenu(thread) }
                            }
                            if section.kind == "inbox", section.rows.isEmpty, search.isEmpty {
                                Text("All caught up").font(.caption).foregroundStyle(.secondary)
                            }
                            if section.hiddenCount > 0 {
                                Button("Show \(section.hiddenCount) more") { core.update(.showAllInSection(section: section.kind)) }
                                    .buttonStyle(.link)
                                    .font(.caption)
                            }
                        }
                    } header: {
                        SectionHeader(section: section, core: core)
                    }
                }
            }
            ForEach(byStatus ? [] : sidebar.projects, id: \.id) { project in
                Section {
                    if project.expanded {
                        ForEach(project.threads, id: \.id) { thread in
                            ThreadRow(thread: thread).equatable()
                                .tag(thread.id)
                                .contextMenu { threadMenu(thread) }
                        }
                        if project.hiddenCount > 0 {
                            Button("Show \(project.hiddenCount) more") { core.update(.showAllThreads(projectId: project.id)) }
                                .buttonStyle(.link)
                                .font(.caption)
                        }
                        if project.earlierCount > 0 {
                            earlier(project)
                        }
                    }
                } header: {
                    ProjectHeader(project: project, core: core)
                }
            }
            if !byStatus, !sidebar.snoozed.isEmpty, search.isEmpty {
                Section("Snoozed") {
                    ForEach(sidebar.snoozed, id: \.id) { thread in
                        ThreadRow(thread: thread).equatable()
                            .tag(thread.id)
                            .contextMenu { threadMenu(thread) }
                    }
                }
            }
            // Settled threads show under their project ("Earlier sessions").
        }
        .listStyle(.sidebar)
        // The core reorders and relabels rows every few seconds while threads
        // run; those changes land without animation, so no row is left
        // mid-slide when the next one comes.
        .transaction { $0.disablesAnimations = true }
        .overlay {
            if !search.isEmpty, sidebar.searchResults.isEmpty, sidebar.projects.allSatisfy({ $0.threads.isEmpty && $0.earlier.isEmpty }),
               sidebar.sections.allSatisfy({ $0.count == 0 }) {
                ContentUnavailableView.search(text: search)
            }
        }
        .searchable(text: $search, placement: .sidebar, prompt: "Search threads")
        .onChange(of: search) { _, query in core.update(.setSearch(query: query)) }
        .onChange(of: sidebar.selectedThreadId) { _, _ in picked = nil }
        .safeAreaInset(edge: .bottom) { footer }
        .navigationTitle("Threads")
        .alert("Rename Thread", isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })) {
            TextField("Title", text: $renameText)
            Button("Rename") {
                if let thread = renaming { core.update(.renameThread(threadId: thread.id, title: renameText)) }
                renaming = nil
            }
            Button("Cancel", role: .cancel) { renaming = nil }
        }
        .sheet(isPresented: $showImport) { ImportSessionsSheet(core: core) }
        .sheet(isPresented: $showBrowser) { FolderBrowserSheet(core: core) }
        .confirmationDialog(
            deleting.map { "Delete “\($0.title.isEmpty ? "New thread" : $0.title)”?" } ?? "",
            isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } }),
            titleVisibility: .visible,
            presenting: deleting
        ) { thread in
            Button("Delete Thread", role: .destructive) { core.update(.deleteThread(threadId: thread.id)) }
        } message: { _ in
            Text("This can’t be undone.")
        }
        .alert("New Project", isPresented: $showNewProject) {
            TextField("Name", text: $newProjectName)
            Button("Create") {
                core.update(.createProject(name: newProjectName))
                newProjectName = ""
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Creates a folder with a git repository on the server.")
        }
    }

    static let hitTag = "hit:"

    /// Grouped by what each thread needs (Inbox, Running, …), not by project.
    private var byStatus: Bool { sidebar.grouping == "status" }

    private var footer: some View {
        HStack(spacing: Space.m) {
            Menu {
                Button("New Thread") { core.update(.newThread(projectId: nil)) }
                Divider()
                Button("Import from Claude Code…") { showImport = true }
                Button("Add Existing Folder…", action: addFolder)
                Button("New Project…") { showNewProject = true }
                Button("New Thread Without a Project") { core.update(.openScratch) }
            } label: {
                Label("Add", systemImage: "plus")
            }
            .menuStyle(.borderlessButton)
            .fixedSize()
            Menu {
                Picker("Group By", selection: Binding(get: { sidebar.grouping }, set: { core.update(.setSidebarGrouping(grouping: $0)) })) {
                    Text("Status").tag("status")
                    Text("Project").tag("project")
                }
                .pickerStyle(.inline)
            } label: {
                Label("Group By", systemImage: "line.3.horizontal.decrease.circle")
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .help("Group threads by status or by project")
            Spacer()
            if sidebar.threadsNeedingAttention > 0 {
                Button(action: nextNeedingYou) {
                    Label("\(sidebar.threadsNeedingAttention) \(sidebar.threadsNeedingAttention == 1 ? "needs" : "need") you", systemImage: "hand.raised.fill")
                        .font(TypeRole.meta.weight(.semibold))
                        .foregroundStyle(Tint.caution)
                        .padding(.horizontal, Space.s)
                        .padding(.vertical, Space.xxs)
                        .background(Tint.caution.opacity(0.14), in: Capsule())
                }
                .buttonStyle(.plain)
                .help("Go to the next thread waiting for you")
            }
            SettingsLink { Image(systemName: "gearshape") }
                .buttonStyle(.borderless)
                .help("Settings")
                .accessibilityLabel("Settings")
        }
        .padding(.horizontal, Space.m)
        .padding(.vertical, Space.s + Space.xxs)
        .background(.bar)
    }

    /// The next thread waiting for an answer, after the open one.
    private func nextNeedingYou() {
        let waiting = sidebar.surfaceThreads.filter { $0.status == "approval" }
        guard !waiting.isEmpty else { return }
        let current = waiting.firstIndex { $0.id == sidebar.selectedThreadId }
        let next = current.map { waiting[($0 + 1) % waiting.count] } ?? waiting[0]
        core.update(.selectThread(threadId: next.id))
    }

    @ViewBuilder
    private func threadMenu(_ thread: ThreadRowView) -> some View {
        Button("Rename…") {
            renameText = thread.title
            renaming = thread
        }
        Button("Regenerate Title") { core.update(.regenerateTitle(threadId: thread.id)) }
        Button("Fork Thread") { core.update(.forkThread(threadId: thread.id)) }
            .disabled(thread.status == "idle")
        Divider()
        Button(thread.pinned ? "Unpin" : "Pin") { core.update(.pinThread(threadId: thread.id, pinned: !thread.pinned)) }
        if byStatus {
            Button(thread.settled ? "Move to Inbox" : "Hibernate") { core.update(.settleThread(threadId: thread.id, settled: !thread.settled)) }
        } else {
            Button(thread.settled ? "Unsettle" : "Settle") { core.update(.settleThread(threadId: thread.id, settled: !thread.settled)) }
        }
        if thread.snoozedUntilMs > 0 {
            Button("Wake Now") { core.update(.snoozeThread(threadId: thread.id, untilMs: 0)) }
        } else {
            Menu("Snooze") {
                ForEach(SnoozeTimes.presets(), id: \.label) { preset in
                    Button(preset.label) { core.update(.snoozeThread(threadId: thread.id, untilMs: preset.ms)) }
                }
            }
        }
        if !thread.unread {
            Button("Mark as Unread") { core.update(.markThreadUnread(threadId: thread.id)) }
        }
        Button(thread.archived ? "Unarchive" : "Archive") { core.update(.archiveThread(threadId: thread.id, archived: !thread.archived)) }
        Divider()
        Button("Copy Thread ID") { Platform.copy(thread.id) }
        Divider()
        Button("Delete…", role: .destructive) { deleting = thread }
    }

    /// A project's settled threads (imported sessions among them), folded.
    @ViewBuilder
    private func earlier(_ project: SidebarProjectView) -> some View {
        let shown = earlierShown.contains(project.id)
        Button {
            if shown { earlierShown.remove(project.id) } else { earlierShown.insert(project.id) }
        } label: {
            Label(project.threads.isEmpty ? "Sessions (\(project.earlierCount))" : "Earlier sessions (\(project.earlierCount))",
                  systemImage: shown ? "chevron.down" : "chevron.right")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .buttonStyle(.plain)
        if shown {
            ForEach(project.earlier, id: \.id) { thread in
                ThreadRow(thread: thread).equatable()
                    .tag(thread.id)
                    .contextMenu { threadMenu(thread) }
            }
            if project.earlier.count < Int(project.earlierCount) {
                Button("Show all \(project.earlierCount)") { core.update(.showAllThreads(projectId: project.id)) }
                    .buttonStyle(.link)
                    .font(.caption)
            }
        }
    }

    private var serverIsLocal: Bool { core.view.connection.isOnThisMac }

    private func addFolder() {
        guard serverIsLocal else {
            showBrowser = true
            return
        }
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.prompt = "Add Project"
        panel.message = "Choose a folder on this Mac (the server must be running here to use it)."
        if panel.runModal() == .OK, let url = panel.url {
            core.update(.addProject(path: url.path))
        }
    }
}

struct ProjectHeader: View {
    let project: SidebarProjectView
    @ObservedObject var core: Core
    @State private var renaming = false
    @State private var title = ""
    @State private var removing = false
    @State private var hovering = false

    var body: some View {
        HStack(spacing: Space.tight) {
            Button { core.update(.toggleProject(projectId: project.id)) } label: {
                Image(systemName: project.expanded ? "chevron.down" : "chevron.right")
                    .font(.caption2.weight(.bold))
                    .frame(width: 10)
            }
            .buttonStyle(.plain)
            .help(project.expanded ? "Collapse" : "Expand")
            .accessibilityLabel(project.expanded ? "Collapse \(project.title)" : "Expand \(project.title)")
            if project.isScratch {
                Image(systemName: "bubble.left.and.text.bubble.right").foregroundStyle(.secondary)
            } else {
                Text(project.monogram)
                    .font(.system(size: 9, weight: .bold))
                    .frame(width: 18, height: 18)
                    .background(Color.accentColor.opacity(0.25), in: RoundedRectangle(cornerRadius: Space.xs))
            }
            Text(project.title).lineLimit(1).help(project.workspaceRoot).layoutPriority(1)
            Spacer()
            Button { core.update(.newThread(projectId: project.id)) } label: { Image(systemName: "square.and.pencil") }
                .buttonStyle(.plain)
                .help("New thread in \(project.title)")
                .opacity(hovering ? 1 : 0)
                // Invisible means not there: no thread from a stray click.
                .allowsHitTesting(hovering)
                .accessibilityLabel("New thread in \(project.title)")
        }
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .contextMenu {
            Button("New Thread") { core.update(.newThread(projectId: project.id)) }
            Button("Rename…") {
                title = project.title
                renaming = true
            }
            if core.view.connection.isOnThisMac {
                Button("Reveal in Finder") { NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: project.workspaceRoot) }
            }
            Divider()
            Button("Remove from Domovoi…", role: .destructive) { removing = true }
        }
        .confirmationDialog("Remove “\(project.title)” from Domovoi?", isPresented: $removing, titleVisibility: .visible) {
            Button("Remove Project", role: .destructive) { core.update(.removeProject(projectId: project.id)) }
        } message: {
            Text("Its folder stays on the server. Its threads go with it.")
        }
        .alert("Rename Project", isPresented: $renaming) {
            TextField("Title", text: $title)
            Button("Rename") { core.update(.renameProject(projectId: project.id, title: title)) }
            Button("Cancel", role: .cancel) {}
        }
    }
}

/// A status section's header: a chevron, its title and how many threads it holds.
struct SectionHeader: View {
    let section: SidebarSectionView
    @ObservedObject var core: Core

    var body: some View {
        Button { core.update(.toggleSection(section: section.kind)) } label: {
            HStack(spacing: Space.tight) {
                Image(systemName: section.expanded ? "chevron.down" : "chevron.right")
                    .font(.caption2.weight(.bold))
                    .frame(width: 10)
                Text(section.title)
                if section.kind == "running", section.count > 0 {
                    ProgressView().controlSize(.mini)
                }
                Spacer()
                if section.count > 0 {
                    Text("\(section.count)").monospacedDigit().foregroundStyle(.tertiary)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help(section.expanded ? "Collapse" : "Expand")
        .accessibilityLabel("\(section.title), \(section.count) threads, \(section.expanded ? "expanded" : "collapsed")")
    }
}

struct ThreadRow: View, Equatable {
    let thread: ThreadRowView
    /// Under a status section the project is not in a header, so the row names it.
    var showsProject = false

    /// "project · model", the project left out when a header already names it.
    private var subtitle: String {
        [showsProject ? thread.projectTitle : "", thread.modelLabel].filter { !$0.isEmpty }.joined(separator: " · ")
    }

    var body: some View {
        HStack(spacing: Space.s) {
            StatusIcon(status: thread.status, unread: thread.unread)
            VStack(alignment: .leading, spacing: Space.xxs / 2) {
                Text(thread.title.isEmpty ? "New thread" : thread.title)
                    .lineLimit(1)
                    .fontWeight(thread.unread ? .semibold : .regular)
                // Always two lines: rows of one height, so the list never
                // keeps a stale height (a gap) when a row's second line comes
                // or goes.
                HStack(spacing: Space.xs) {
                    Text(subtitle.isEmpty ? " " : subtitle).lineLimit(1)
                    if let branch = thread.branch, thread.inWorktree {
                        Label(branch, systemImage: "arrow.triangle.branch").lineLimit(1)
                    }
                }
                .font(.caption2)
                .foregroundStyle(.secondary)
            }
            .layoutPriority(1)
            Spacer(minLength: Space.xs)
            if thread.relation == "fork" {
                Image(systemName: "arrow.triangle.branch").font(.caption2).foregroundStyle(.secondary).help("A fork of another thread")
                    .accessibilityLabel("Fork")
            }
            if !thread.source.isEmpty {
                Text(thread.source).font(.caption2).foregroundStyle(Tint.agent(thread.source)).lineLimit(1).fixedSize()
            }
            if thread.pinned { Image(systemName: "pin.fill").font(.caption2).foregroundStyle(.secondary) }
            // Short and always whole: the title gives way first.
            Group {
                if !thread.statusLabel.isEmpty {
                    Text(thread.statusLabel).font(.caption2.weight(.medium)).foregroundStyle(thread.status == "failed" ? Tint.danger : thread.status == "approval" ? Tint.caution : .secondary).monospacedDigit()
                } else {
                    Text(thread.snoozedUntilMs > 0 ? SnoozeTimes.label(thread.snoozedUntilMs) : thread.age).font(.caption2).foregroundStyle(.tertiary).monospacedDigit()
                }
            }
            .lineLimit(1)
            .fixedSize()
        }
        .frame(minHeight: 22)
        // Never wider than the column: a row asking for more makes the list
        // scroll sideways, cutting every title's start.
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}


extension ConnectionView {
    /// The server runs on this Mac: its folders are this Mac's (Finder can
    /// show them, an open panel can pick them).
    var isOnThisMac: Bool {
        guard !remote, let host = URL(string: serverUrl)?.host else { return false }
        return ["127.0.0.1", "localhost", "::1", "[::1]"].contains(host)
    }
}
