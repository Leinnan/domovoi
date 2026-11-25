import App
import SwiftUI

/// The ways to get a project: import Claude Code / Codex sessions, pick a
/// folder on the server, make a new one, or start without one.
@MainActor
struct AddProjectActions {
    let core: Core
    let open: (Screen) -> Void
    /// The next project the server creates opens the composer.
    let expectProject: () -> Void
    let askName: () -> Void

    func importSessions() { open(.importSessions) }
    func browse() { open(.browseFolders) }
    func newProject() { askName() }

    func withoutProject() {
        // An existing "No project" project is used as is.
        if let scratch = core.view.sidebar.projects.first(where: { $0.isScratch }) {
            core.update(.newThread(projectId: scratch.id))
            open(.newThread)
        } else {
            expectProject()
            core.update(.openScratch)
        }
    }

    @ViewBuilder
    var menuItems: some View {
        let available = core.view.agentSessions.available
        Button(action: importSessions) {
            Label(available > 0 ? "Import from Claude Code (\(available))" : "Import from Claude Code", systemImage: "square.and.arrow.down")
        }
        Button(action: browse) { Label("Add Folder on Server", systemImage: "folder.badge.plus") }
        Button(action: newProject) { Label("New Project", systemImage: "plus.rectangle.on.folder") }
        Button(action: withoutProject) { Label("Thread Without a Project", systemImage: "bubble.left") }
    }
}

/// Asks for a new project's name; the server makes the folder.
struct NewProjectPrompt: ViewModifier {
    @Binding var isPresented: Bool
    let create: (String) -> Void
    @State private var name = ""

    func body(content: Content) -> some View {
        content.alert("New Project", isPresented: $isPresented) {
            TextField("Name", text: $name)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
            Button("Create") {
                let trimmed = name.trimmingCharacters(in: .whitespaces)
                name = ""
                if !trimmed.isEmpty { create(trimmed) }
            }
            Button("Cancel", role: .cancel) { name = "" }
        } message: {
            Text("A new folder with a Git repository, in the server's projects folder.")
        }
    }
}

/// Folders on the server with Claude Code or Codex sessions; the ticked ones
/// become projects with their recent sessions.
struct ImportSessionsScreen: View {
    @ObservedObject var core: Core
    @Environment(\.dismiss) private var dismiss
    @State private var selected: Set<String> = []
    @State private var preselected = false

    private var sessions: AgentSessionsView { core.view.agentSessions }
    private var fresh: [AgentSessionCandidateView] { sessions.candidates.filter { !$0.alreadyImported } }
    private var imported: [AgentSessionCandidateView] { sessions.candidates.filter(\.alreadyImported) }

    var body: some View {
        List {
            if let error = sessions.error {
                Section {
                    ErrorRow(error)
                    Button("Try Again") { core.update(.scanAgentSessions) }
                }
            }
            if sessions.scanning && !sessions.scanned {
                Section {
                    LoadingRow("Looking for Claude Code and Codex sessions…")
                }
            } else if sessions.scanned && sessions.candidates.isEmpty {
                ContentUnavailableView("No sessions found", systemImage: "tray", description: Text("Claude Code and Codex sessions on the server show up here."))
            }
            if !fresh.isEmpty {
                Section {
                    ForEach(fresh, id: \.path) { row($0) }
                } header: {
                    Text("Not imported")
                } footer: {
                    Text("Each folder becomes a project with its sessions from the last 30 days. Continuing one resumes the same session.")
                }
            }
            if !imported.isEmpty {
                Section {
                    ForEach(imported, id: \.path) { row($0) }
                } header: {
                    Text("Already projects")
                } footer: {
                    Text("Import again to pick up sessions started since.")
                }
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle("Import Sessions")
        .navigationBarTitleDisplayMode(.inline)
        .refreshable { await core.refresh(.scanAgentSessions) { core.view.agentSessions.scanning } }
        .safeAreaInset(edge: .bottom) { importBar }
        .onAppear { core.update(.scanAgentSessions) }
        .onChange(of: sessions.scanned) { _, _ in preselect() }
        .onChange(of: sessions.importing) { was, now in
            // Done: back to the list, where the projects now are. A failed
            // import stays, its error showing.
            if was, !now, sessions.error == nil { dismiss() }
        }
        .task { preselect() }
    }

    private func preselect() {
        guard sessions.scanned, !preselected else { return }
        preselected = true
        selected = Set(fresh.filter(\.recent).map(\.path))
    }

    private func row(_ c: AgentSessionCandidateView) -> some View {
        Button {
            if selected.contains(c.path) { selected.remove(c.path) } else { selected.insert(c.path) }
        } label: {
            HStack(spacing: Space.m) {
                Image(systemName: selected.contains(c.path) ? "checkmark.circle.fill" : "circle")
                    .font(.title3)
                    .foregroundStyle(selected.contains(c.path) ? Color.accentColor : .secondary)
                    .iconColumn(Size.listIcon)
                VStack(alignment: .leading, spacing: Space.xxs) {
                    Text(c.title).foregroundStyle(.primary)
                    Text(c.path).font(TypeRole.metaSmall).foregroundStyle(.secondary).lineLimit(1).truncationMode(.head)
                    HStack(spacing: Space.tight) {
                        SourceBadge(source: c.sources)
                        Text(c.detail).font(TypeRole.meta).foregroundStyle(.secondary)
                    }
                }
                Spacer(minLength: 0)
            }
        }
        .buttonStyle(.plain)
        .contentShape(Rectangle())
        .disabled(c.threadCount == 0 || sessions.importing)
        .opacity(c.threadCount == 0 ? 0.5 : 1)
        .accessibilityAddTraits(selected.contains(c.path) ? .isSelected : [])
    }

    private var importBar: some View {
        VStack(spacing: Space.tight) {
            if sessions.importing {
                ProgressView(value: Double(sessions.done), total: Double(max(sessions.total, 1))) {
                    Text("Importing \(min(sessions.done + 1, sessions.total)) of \(sessions.total)…").font(.footnote)
                }
            } else {
                Button {
                    let ordered = sessions.candidates.map(\.path).filter(selected.contains)
                    core.update(.importAgentSessions(paths: ordered))
                } label: {
                    Text(selected.isEmpty ? "Import" : selected.count == 1 ? "Import 1 Folder" : "Import \(selected.count) Folders")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .disabled(selected.isEmpty)
            }
        }
        .projectActionBar()
    }
}

/// "Claude Code", "Codex", "Pi" or several, as small capsules.
struct SourceBadge: View {
    let source: String

    var body: some View {
        HStack(spacing: Space.xs) {
            ForEach(source.components(separatedBy: " · ").filter { !$0.isEmpty }, id: \.self) { name in
                Text(name)
                    .foregroundStyle(Tint.agent(name))
                    .badgeCapsule(tint: Tint.agent(name))
            }
        }
    }
}

/// The server's folders; the one shown becomes a project.
struct FolderBrowserScreen: View {
    @ObservedObject var core: Core
    let open: (Screen) -> Void
    let expectProject: () -> Void

    private var browser: FolderBrowserView { core.view.folderBrowser }
    private var existing: SidebarProjectView? {
        core.view.sidebar.projects.first { $0.workspaceRoot.trimmingSuffix("/") == browser.path.trimmingSuffix("/") }
    }
    /// The server has it as a project already, one this list doesn't show.
    private var unlistedProject: Bool { browser.isProject && existing == nil }

    var body: some View {
        List {
            Section {
                if !browser.parentPath.isEmpty {
                    Button { core.update(.browseFolders(path: browser.parentPath)) } label: {
                        Label("Up to \(URL(fileURLWithPath: browser.parentPath).lastPathComponent.isEmpty ? "/" : URL(fileURLWithPath: browser.parentPath).lastPathComponent)", systemImage: "arrow.turn.left.up")
                    }
                }
                if let error = browser.error {
                    ErrorRow(error)
                }
                if browser.loading && browser.entries.isEmpty {
                    LoadingRow("Loading folders…")
                } else if !browser.loading && browser.entries.isEmpty && browser.error == nil {
                    Text("No folders inside").foregroundStyle(.secondary)
                }
                ForEach(browser.entries, id: \.fullPath) { entry in
                    Button { core.update(.browseFolders(path: entry.fullPath)) } label: {
                        HStack {
                            Label(entry.name, systemImage: "folder")
                            Spacer()
                            Image(systemName: "chevron.right").font(.caption).foregroundStyle(.tertiary)
                        }
                    }
                    .foregroundStyle(.primary)
                }
            } header: {
                Text(browser.path.isEmpty ? "…" : browser.path).textCase(nil).lineLimit(2).truncationMode(.head)
            } footer: {
                if unlistedProject { Text("This folder is already a project.") }
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle(browser.path.isEmpty ? "Folders" : (URL(fileURLWithPath: browser.path).lastPathComponent.isEmpty ? "/" : URL(fileURLWithPath: browser.path).lastPathComponent))
        .navigationBarTitleDisplayMode(.inline)
        .safeAreaInset(edge: .bottom) {
            Button {
                if let project = existing {
                    core.update(.newThread(projectId: project.id))
                    open(.newThread)
                } else {
                    expectProject()
                    core.update(.addProject(path: browser.path))
                }
            } label: {
                Text(existing != nil ? "Start a Thread Here" : unlistedProject ? "Already a Project" : "Add This Folder").frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .disabled(browser.path.isEmpty || browser.loading || unlistedProject)
            .projectActionBar()
        }
        .onAppear {
            if browser.path.isEmpty { core.update(.browseFolders(path: "")) }
        }
    }
}

private extension View {
    /// The bar under the import and folder screens holding their one action.
    func projectActionBar() -> some View {
        padding(.horizontal, Space.l)
            .padding(.vertical, Space.s)
            .background(.bar)
    }
}

extension String {
    func trimmingSuffix(_ suffix: String) -> String {
        var s = self
        while s.count > 1 && s.hasSuffix(suffix) { s.removeLast(suffix.count) }
        return s
    }
}
