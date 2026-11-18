import App
import AppKit
import SwiftUI

struct ThreadView: View {
    @ObservedObject var core: Core
    @State private var confirmDelete = false
    /// The thread the delete dialog was opened for: the selection can move
    /// while it is up (a notification, a fork), and must not take the delete.
    @State private var deletingId: String?

    /// Reads only the thread's header: the rows (which change on every
    /// streamed token) are read by `ThreadContent`, so the title, toolbar
    /// and composer here don't redraw with them.
    var body: some View {
        #if DEBUG
        let _ = DebugViews.printChanges ? Self._printChanges() : ()
        #endif
        let header = core.view.header
        VStack(spacing: 0) {
            if header != nil { ThreadContent(core: core) }
            ComposerPane(core: core)
        }
        .navigationTitle(header.map { $0.title.isEmpty ? "New Thread" : $0.title } ?? "Thread")
        .navigationSubtitle(header?.subtitle ?? "")
        .toolbar {
            if let thread = header {
                ToolbarItem { PlanBadge(core: core) }
                if let context = thread.context {
                    ToolbarItem { ContextMeter(context: context, showsLabel: true).padding(.horizontal, Space.xs) }
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

    var body: some View {
        if let thread = core.view.thread {
            if let find = thread.find {
                FindBar(core: core, find: find)
                Divider()
            }
            // One per thread: another thread starts at its own end (or where
            // it was read), not with this one's scroll state.
            Transcript(core: core, thread: thread)
                .id(thread.id)
            ApprovalArea(core: core)
        }
    }
}

/// Approvals and questions waiting on the reader, under the transcript.
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
                if let project, !project.isScratch {
                    Text(project.monogram)
                        .font(.system(size: 20, weight: .bold))
                        .frame(width: 52, height: 52)
                        .background(Color.accentColor.opacity(0.18), in: RoundedRectangle(cornerRadius: Radius.card, style: .continuous))
                        .foregroundStyle(.tint)
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
