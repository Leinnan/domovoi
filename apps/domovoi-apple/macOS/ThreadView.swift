import App
import AppKit
import SwiftUI

struct ThreadView: View {
    @ObservedObject var core: Core
    @State private var confirmDelete = false
    @State private var artifacts = false
    @State private var progress = false
    @State private var artifactJump: String?
    /// The thread the delete dialog was opened for: the selection can move
    /// while it is up (a notification, a fork), and must not take the delete.
    @State private var deletingId: String?
    /// A scheduled task for the open thread, being set up.
    @State private var scheduling: ScheduledTaskEdit?

    /// Reads only the thread's header: the rows (which change on every
    /// streamed token) are read by `ThreadContent`, so the title, toolbar
    /// and composer here don't redraw with them.
    var body: some View {
        #if DEBUG
        let _ = DebugViews.printChanges ? Self._printChanges() : ()
        #endif
        let header = core.view.header
        VStack(spacing: 0) {
            if header != nil {
                ThreadContent(core: core, artifactJump: artifactJump)
                    .modifier(TranscriptTextScaleModifier())
            }
            ForEach(header?.waitingChildren ?? [], id: \.threadId) { child in
                WaitingChildBanner(child: child) { core.update(.selectThread(threadId: $0)) }
                    .padding(.horizontal, Space.gutter)
                    .padding(.top, Space.xs)
                    .frame(maxWidth: Space.column)
                    .frame(maxWidth: .infinity)
            }
            if let header, header.plan != nil || header.tasks != nil {
                WorkProgressStrip(plan: header.plan, tasks: header.tasks) { progress = true }
                    .padding(.horizontal, Space.gutter)
                    .padding(.top, Space.xs)
                    .frame(maxWidth: Space.column)
                    .frame(maxWidth: .infinity)
            }
            ComposerPane(core: core)
        }
        .navigationTitle(header.map { $0.title.isEmpty ? "New Thread" : $0.title } ?? "Thread")
        .navigationSubtitle(header?.subtitle ?? "")
        .toolbar {
            if let thread = header {
                ToolbarItem { PlanBadge(core: core) }
                ToolbarItem { ScriptsToolbarButton(core: core) }
                ToolbarItem {
                    Button {
                        core.update(.showPanel(tab: "artifacts"))
                    } label: {
                        HStack(spacing: Space.xxs) {
                            Image(systemName: "sparkles.rectangle.stack")
                            if thread.artifactCount > 0 {
                                Text("\(thread.artifactCount)")
                                    .font(.caption2.monospacedDigit().weight(.semibold))
                            }
                        }
                    }
                    .help("Artifacts (⌘⇧A)")
                    .accessibilityLabel(thread.artifactCount > 0 ? "Artifacts (\(thread.artifactCount))" : "Artifacts")
                }
                if let usage = thread.usage {
                    ToolbarItem {
                        UsageMeter(usage: usage, showsLabel: true, compact: { core.update(.compact) }, showAll: { core.update(.showRoute(route: "usage")) })
                            .padding(.horizontal, Space.xs)
                    }
                }
                ToolbarItemGroup {
                    Menu {
                        Button("Fork Thread") { core.update(.forkThread(threadId: thread.id)) }
                            .disabled(!thread.canFork)
                            .help("A new thread that starts from this conversation")
                        if thread.canMergeBack, let parent = thread.parentTitle {
                            Button("Merge Back into “\(parent)”") { core.update(.mergeBack) }
                        }
                        Button("Regenerate Title") { core.update(.regenerateTitle(threadId: thread.id)) }
                            .disabled(!thread.hasItems)
                        Divider()
                        Button(thread.pinned ? "Unpin" : "Pin") { core.update(.pinThread(threadId: thread.id, pinned: !thread.pinned)) }
                        Button(thread.settled ? "Unsettle" : "Settle") { core.update(.settleThread(threadId: thread.id, settled: !thread.settled)) }
                        Button(thread.archived ? "Unarchive" : "Archive") { core.update(.archiveThread(threadId: thread.id, archived: !thread.archived)) }
                        if thread.worktreePath != nil, thread.projectTitle == "No project" {
                            Button("Make This Folder a Project") { core.update(.promoteScratchThread(threadId: thread.id)) }
                        }
                        Divider()
                        if core.view.connection.isOnThisMac {
                            Button("Reveal Working Folder") { NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: thread.cwd) }
                        }
                        Button("Copy Thread ID") { Platform.copy(thread.id) }
                        Button("Schedule This Thread…") {
                            guard let page = core.view.thread, page.id == thread.id else { return }
                            core.update(.loadSettings(section: "scheduled"))
                            scheduling = ScheduledTaskEditor.new(thread: page)
                        }
                        .disabled(thread.archived)
                        Button("Forget Allowed Tools") { core.update(.forgetThreadPermissions(threadId: thread.id)) }
                            .help("The tools “Allow for Thread” allowed here are asked about again from the agent’s next start")
                        Divider()
                        Button("Delete Thread…", role: .destructive) {
                            deletingId = thread.id
                            confirmDelete = true
                        }
                    } label: {
                        Label("Thread", systemImage: "ellipsis.circle")
                    }
                }
            }
        }
        .sheet(isPresented: $artifacts) {
            if let page = core.view.thread {
                ArtifactGallery(core: core, thread: page, close: { artifacts = false }) { item in
                    core.update(.closeFind)
                    artifactJump = item
                    artifacts = false
                }
                .id(page.id)
            }
        }
        .sheet(item: $scheduling) { draft in
            NavigationStack {
                ScheduledTaskEditor(core: core, draft: draft) { scheduling = nil }
                    .navigationTitle("Schedule Thread")
            }
            .frame(minWidth: 520, minHeight: 560)
        }
        .sheet(isPresented: $progress) {
            if let header {
                WorkProgressPanel(core: core, threadID: header.id) { progress = false }
            }
        }
        .onChange(of: header?.id) { artifacts = false; progress = false; artifactJump = nil }
        .animation(.snappy, value: header?.tasks == nil && header?.plan == nil)
        .animation(.snappy, value: header?.waitingChildren.map(\.threadId))
        .confirmationDialog("Delete this thread?", isPresented: $confirmDelete, titleVisibility: .visible) {
            Button("Delete Thread", role: .destructive) {
                if let id = deletingId { core.update(.deleteThread(threadId: id)) }
            }
        } message: {
            Text("This can’t be undone.")
        }
    }
}

/// The open thread's find bar, transcript and approvals: the part of the
/// thread view that follows its rows.
private struct ThreadContent: View {
    @ObservedObject var core: Core
    let artifactJump: String?

    var body: some View {
        if let thread = core.view.thread {
            if let find = thread.find {
                FindBar(core: core, find: find)
                Divider()
            }
            // One per thread: another thread starts at its own end (or where
            // it was read), not with this one's scroll state.
            WorkLinksStrip(links: thread.links)
            Transcript(core: core, thread: thread, artifactJump: artifactJump)
                .id(thread.id)
            ApprovalArea(core: core)
        }
    }
}

/// Approvals, questions and secret requests waiting on the reader, under the transcript.
private struct ApprovalArea: View {
    @ObservedObject var core: Core

    var body: some View {
        let requests = core.view.pendingRequests
        if !requests.isEmpty {
            ApprovalBar(core: core, requests: requests)
                .padding(.horizontal, Space.gutter - Space.s)
                .frame(maxWidth: Space.column)
                .frame(maxWidth: .infinity)
        }
        let secrets = core.view.secretRequests
        if !secrets.isEmpty {
            SecretRequestArea(core: core, requests: secrets)
                .padding(.horizontal, Space.gutter - Space.s)
                .frame(maxWidth: Space.column)
                .frame(maxWidth: .infinity)
        }
    }
}

/// "Plan mode" in the toolbar while it's on. A view of its own, so the
/// thread doesn't redraw as the draft changes.
private struct PlanBadge: View {
    @ObservedObject var core: Core

    var body: some View {
        if core.view.composer.interactionMode == "plan" {
            Button { ComposerCommands.shared.togglePlan(core: core) } label: {
                Label("Plan mode", systemImage: "list.bullet.clipboard")
                    .labelStyle(.titleAndIcon)
                    .font(TypeRole.meta.weight(.semibold))
                    .foregroundStyle(Tint.plan)
                    .padding(.horizontal, Space.s)
                    .padding(.vertical, Space.xxs)
                    .background(Tint.plan.opacity(0.14), in: Capsule())
            }
            .buttonStyle(.plain)
            .help("The agent plans first and waits for your go-ahead. Click to turn off.")
        }
    }
}

/// The connection banner and the toast, above the content, in the reading
/// column.
struct StatusStrip: View {
    @ObservedObject var core: Core

    var body: some View {
        VStack(spacing: Space.s) {
            ConnectionBanner(core: core)
                .padding(.horizontal, Space.gutter - Space.m)
        }
        .frame(maxWidth: Space.column)
        .frame(maxWidth: .infinity)
        .padding(.top, ConnectionBanner.showing(core) ? Space.s : 0)
        .animation(.snappy, value: core.view.connection.state)
        .animation(.snappy, value: core.view.connection.offline)
    }
}

struct NewThreadView: View {
    @ObservedObject var core: Core
    @State private var showImport = false

    private var project: SidebarProjectView? {
        let id = core.view.sidebar.selectedProjectId
        return core.view.sidebar.projects.first { $0.id == id }
    }

    var body: some View {
        VStack(spacing: 0) {
            Spacer()
            VStack(spacing: Space.m) {
                if let project {
                    ProjectIconTile(icon: project.icon, size: 52, core: core)
                } else {
                    Image(systemName: "bubble.left.and.text.bubble.right")
                        .font(.system(size: 32))
                        .foregroundStyle(.tertiary)
                }
                Text(title)
                    .font(.title.bold())
                    .multilineTextAlignment(.center)
                if core.view.sidebar.projects.isEmpty {
                    Text("Use Add at the bottom of the sidebar to add a folder, create a project, or start a thread without one.")
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                    Button("Import from Claude Code…") { showImport = true }
                        .glassButton(prominent: true)
                } else {
                    Text("Ask, or turn on Plan to agree on a plan first. Type @ to mention a file.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                }
            }
            .padding(.horizontal, Space.xl)
            .padding(.bottom, Space.xl)
            ComposerPane(core: core)
            Spacer()
            Spacer()
        }
        .frame(maxWidth: .infinity)
        .navigationTitle("New Thread")
        .sheet(isPresented: $showImport) { ImportSessionsSheet(core: core) }
    }

    private var title: String {
        guard let project else { return core.view.sidebar.projects.isEmpty ? "Add a project to start" : "What should we build?" }
        return project.isScratch ? "What should we work on?" : "What should we build in \(project.title)?"
    }
}
