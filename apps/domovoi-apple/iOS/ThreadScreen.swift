import App
import SwiftUI

struct ThreadScreen: View {
    @ObservedObject var core: Core
    let threadId: String
    let open: (Screen) -> Void
    @State private var confirmDelete = false
    @State private var renaming = false
    @State private var newTitle = ""
    @Environment(\.dismiss) private var dismiss
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    /// Still loading after a while: offer to try again.
    @State private var slow = false
    /// A fork was asked for: its screen opens once the core selects it.
    @State private var forking = false

    private var thread: ThreadPageView? {
        core.view.thread.flatMap { $0.id == threadId ? $0 : nil }
    }

    /// The thread's title; while its page loads, the one the thread list
    /// already has (no "Thread" → real title jump on every open).
    private var title: String {
        let known: String? = thread?.title ?? core.view.sidebar.listedRows.first(where: { $0.id == threadId })?.title
        guard let known else { return "Thread" }
        return known.isEmpty ? "New Thread" : known
    }

    /// A "No project" thread: only one with its own folder can become a project.
    private func isScratch(_ thread: ThreadPageView) -> Bool {
        core.view.sidebar.projects.first { $0.id == thread.projectId }?.isScratch ?? false
    }

    var body: some View {
        #if DEBUG
        let _ = DebugViews.printChanges ? Self._printChanges() : ()
        #endif
        VStack(spacing: 0) {
            if let thread {
                if let find = thread.find {
                    FindBar(core: core, find: find)
                    Divider()
                }
                Transcript(core: core, thread: thread)
                    .environment(\.openDiff) { open(.diff) }
                    .environment(\.openThread) { open(.thread($0)) }
            } else if slow {
                ContentUnavailableView {
                    Label("Couldn't load this thread", systemImage: "exclamationmark.bubble")
                } description: {
                    Text(core.view.connection.state == "connected" ? "The server hasn't sent it yet." : "Not connected to the server.")
                } actions: {
                    Button(core.view.connection.state == "connected" ? "Try Again" : "Reconnect") {
                        slow = false
                        core.update(core.view.connection.state == "connected" ? .reloadThread : .reconnect)
                    }
                    .buttonStyle(.borderedProminent)
                }
            } else {
                Spacer()
                ProgressView()
                Spacer()
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        // The transcript scrolls on under the approval card and the composer.
        .safeAreaBar(edge: .bottom, spacing: 0) {
            VStack(spacing: 0) {
                if let requests = thread?.pendingRequests, !requests.isEmpty {
                    ApprovalBar(core: core, requests: requests)
                        .transition(reduceMotion ? .opacity : .move(edge: .bottom).combined(with: .opacity))
                }
                MobileComposer(core: core)
            }
            .animation(.snappy, value: thread?.pendingRequests.map(\.id))
        }
        .task(id: threadId) {
            slow = false
            try? await Task.sleep(for: .seconds(15))
            if thread == nil || thread?.loading == true && thread?.items.isEmpty == true { slow = true }
        }
        // Connection lost while the thread was still loading: say so now
        // rather than after the timer.
        .onChange(of: core.view.connection.state) { _, state in
            if state != "connected", thread == nil || thread?.items.isEmpty == true && thread?.loading == true { slow = true }
        }
        .navigationTitle(title)
        // Where it works: project and branch, under the title.
        .navigationSubtitle(thread.map { [$0.projectTitle, $0.branch ?? ""].filter { !$0.isEmpty }.joined(separator: " · ") } ?? "")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            if let context = thread?.context {
                ToolbarItem(placement: .topBarTrailing) { ContextMeter(context: context, showsLabel: true) }
            }
            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    Section {
                        Button { open(.diff) } label: { Label("Changes", systemImage: "plus.forwardslash.minus") }
                        Button { open(.terminal) } label: { Label("Terminal", systemImage: "terminal") }
                        Button { open(.files) } label: { Label("Files", systemImage: "folder") }
                        Button { open(.git) } label: { Label("Git", systemImage: "arrow.triangle.branch") }
                    }
                    if let thread {
                        Section {
                            // ⌘F is in the app's Thread menu (a hardware keyboard).
                            Button { core.update(.findInThread(query: "")) } label: { Label("Find in Thread", systemImage: "magnifyingglass") }
                            Button {
                                newTitle = thread.title
                                renaming = true
                            } label: { Label("Rename", systemImage: "pencil") }
                            Button(thread.pinned ? "Unpin" : "Pin") { core.update(.pinThread(threadId: thread.id, pinned: !thread.pinned)) }
                            Button(thread.settled ? "Unsettle" : "Settle") { core.update(.settleThread(threadId: thread.id, settled: !thread.settled)) }
                            Button(thread.archived ? "Unarchive" : "Archive") {
                                core.update(.archiveThread(threadId: thread.id, archived: !thread.archived))
                                if !thread.archived { dismiss() }
                            }
                            Button("Copy Thread ID") { Platform.copy(thread.id) }
                        }
                        Section {
                            Button {
                                forking = true
                                core.update(.forkThread(threadId: thread.id))
                            } label: { Label("Fork Thread", systemImage: "arrow.triangle.branch") }
                            .disabled(!thread.canFork)
                            if thread.canMergeBack, let parent = thread.parent {
                                Button { core.update(.mergeBack) } label: { Label("Merge Back into “\(parent.title)”", systemImage: "arrow.uturn.left") }
                            }
                            Button { core.update(.regenerateTitle(threadId: thread.id)) } label: { Label("Regenerate Title", systemImage: "sparkles") }
                                .disabled(thread.items.isEmpty)
                        }
                        if thread.worktreePath != nil, isScratch(thread) {
                            Button {
                                core.update(.promoteScratchThread(threadId: thread.id))
                            } label: { Label("Make This Folder a Project", systemImage: "folder.badge.plus") }
                            .disabled(thread.working)
                        }
                        Button("Delete Thread", role: .destructive) { confirmDelete = true }
                    }
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
                .accessibilityLabel("Thread actions")
            }
        }
        .confirmationDialog("Delete this thread?", isPresented: $confirmDelete, titleVisibility: .visible) {
            Button("Delete", role: .destructive) {
                core.update(.deleteThread(threadId: threadId))
                // Gone: back to the list instead of a page that can't load.
                dismiss()
            }
        }
        .alert("Rename Thread", isPresented: $renaming) {
            TextField("Title", text: $newTitle)
            Button("Rename") {
                let title = newTitle.trimmingCharacters(in: .whitespaces)
                if !title.isEmpty { core.update(.renameThread(threadId: threadId, title: title)) }
            }
            Button("Cancel", role: .cancel) {}
        }
        .onAppear {
            // Shown again (back from a thread pushed over it): not "couldn't
            // load" for the time it spent covered.
            slow = false
            if core.view.sidebar.selectedThreadId != threadId { core.update(.selectThread(threadId: threadId)) }
        }
        .onChange(of: core.view.sidebar.selectedThreadId) { _, id in
            guard forking, let id, id != threadId else { return }
            forking = false
            open(.thread(id))
        }
    }
}

struct NewThreadScreen: View {
    @ObservedObject var core: Core
    let launched: (String) -> Void

    private var composer: ComposerView { core.view.composer }
    private var project: SidebarProjectView? {
        core.view.sidebar.projects.first { $0.id == core.view.sidebar.selectedProjectId }
    }

    var body: some View {
        // Scrolls when it doesn't fit (landscape, large text, keyboard up).
        GeometryReader { geo in
        ScrollView {
        VStack(spacing: Space.l) {
            Spacer()
            Image(systemName: "sparkles")
                .font(.system(size: 40))
                .foregroundStyle(.tint)
                .symbolEffect(.bounce, value: core.view.sidebar.selectedProjectId)
            Text(project.map { "What should we do in \($0.title)?" } ?? "Pick a project to start")
                .font(.title3.weight(.semibold))
                .multilineTextAlignment(.center)
                .padding(.horizontal, Space.xl)
            GlassGroup(spacing: Space.s) {
                FlowChips {
                    projectMenu
                    if composer.prWorktree.isEmpty {
                        workspaceMenu
                        if composer.workspaceMode == "worktree", !composer.branches.isEmpty { branchMenu }
                    } else {
                        // Checked out from a pull request: the thread works there.
                        chipLabel(composer.prWorktree, "arrow.triangle.pull")
                    }
                }
            }
            .padding(.horizontal, Space.l)
            Text(composer.workspaceMode == "worktree" ? "A new worktree keeps the agent's changes apart from your checkout." : "The agent works in your current checkout.")
                .font(TypeRole.meta)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .padding(.horizontal, Space.xl)
            Spacer()
            Spacer()
        }
        .frame(maxWidth: .infinity, minHeight: geo.size.height)
        }
        .scrollBounceBehavior(.basedOnSize)
        .scrollDismissesKeyboard(.interactively)
        }
        .safeAreaBar(edge: .bottom, spacing: 0) { MobileComposer(core: core, autofocus: true) }
        .navigationTitle(project.map { "New in \($0.title)" } ?? "New Thread")
        .navigationBarTitleDisplayMode(.inline)
        .onChange(of: core.view.thread?.id) { _, id in
            if let id, core.view.route == "thread" { launched(id) }
        }
    }

    private func chipLabel(_ text: String, _ icon: String) -> some View {
        HStack(spacing: Space.xs) {
            Image(systemName: icon)
            Text(text).lineLimit(1)
            Image(systemName: "chevron.up.chevron.down").imageScale(.small).foregroundStyle(.secondary)
        }
        .font(.footnote.weight(.medium))
        .padding(.horizontal, Space.m)
        .frame(minHeight: 34)
        .glassChip()
    }

    private var projectMenu: some View {
        Menu {
            Picker("Project", selection: Binding(
                get: { core.view.sidebar.selectedProjectId ?? "" },
                set: { if !$0.isEmpty { core.update(.newThread(projectId: $0)) } }
            )) {
                ForEach(core.view.sidebar.projects, id: \.id) { p in Text(p.title).tag(p.id) }
            }
        } label: { chipLabel(project?.title ?? "Project", "folder") }
        .accessibilityLabel("Project")
        .accessibilityValue(project?.title ?? "None")
    }

    private var workspaceMenu: some View {
        Menu {
            Picker("Workspace", selection: Binding(
                get: { composer.workspaceMode },
                set: { core.update(.setWorkspaceMode(mode: $0)) }
            )) {
                Label("Current checkout", systemImage: "folder").tag("local")
                Label("New worktree", systemImage: "arrow.triangle.branch").tag("worktree")
            }
        } label: {
            chipLabel(composer.workspaceMode == "worktree" ? "New worktree" : "Current checkout", composer.workspaceMode == "worktree" ? "arrow.triangle.branch" : "laptopcomputer")
        }
        .accessibilityLabel("Workspace")
    }

    private var branchMenu: some View {
        Menu {
            Picker("Start from", selection: Binding(
                get: { composer.baseBranch ?? "" },
                set: { core.update(.setBaseBranch(branch: $0)) }
            )) {
                ForEach(composer.branches, id: \.self) { Text($0).tag($0) }
            }
        } label: { chipLabel("from \(composer.baseBranch ?? "HEAD")", "point.topleft.down.to.point.bottomright.curvepath") }
        .accessibilityLabel("Start from branch")
    }
}

/// Chips that wrap onto more lines when they don't fit on one.
struct FlowChips: Layout {
    var spacing: CGFloat = Space.s

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let rows = arrange(width: proposal.width ?? .infinity, subviews: subviews)
        let height = rows.map(\.height).reduce(0, +) + spacing * CGFloat(max(rows.count - 1, 0))
        let width = rows.map(\.width).max() ?? 0
        return CGSize(width: width, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var y = bounds.minY
        for row in arrange(width: bounds.width, subviews: subviews) {
            var x = bounds.minX + (bounds.width - row.width) / 2
            for index in row.items {
                let size = subviews[index].sizeThatFits(.unspecified)
                subviews[index].place(at: CGPoint(x: x, y: y + (row.height - size.height) / 2), proposal: .unspecified)
                x += size.width + spacing
            }
            y += row.height + spacing
        }
    }

    private struct Row { var items: [Int] = []; var width: CGFloat = 0; var height: CGFloat = 0 }

    private func arrange(width: CGFloat, subviews: Subviews) -> [Row] {
        var rows: [Row] = [Row()]
        for (index, view) in subviews.enumerated() {
            let size = view.sizeThatFits(.unspecified)
            let extra = rows[rows.count - 1].items.isEmpty ? size.width : size.width + spacing
            if rows[rows.count - 1].width + extra > width, !rows[rows.count - 1].items.isEmpty {
                rows.append(Row())
            }
            let added = rows[rows.count - 1].items.isEmpty ? size.width : size.width + spacing
            rows[rows.count - 1].items.append(index)
            rows[rows.count - 1].width += added
            rows[rows.count - 1].height = max(rows[rows.count - 1].height, size.height)
        }
        return rows
    }
}
