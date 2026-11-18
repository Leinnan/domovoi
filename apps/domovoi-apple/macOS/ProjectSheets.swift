import App
import SwiftUI

/// Folders on the server with Claude Code or Codex sessions; the ticked ones
/// become projects with their recent sessions.
struct ImportSessionsSheet: View {
    @ObservedObject var core: Core
    @Environment(\.dismiss) private var dismiss
    @State private var selected: Set<String> = []
    @State private var preselected = false

    private var sessions: AgentSessionsView { core.view.agentSessions }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.m) {
            Text("Import from Claude Code and Codex").font(.title2.bold())
            Text("Each folder becomes a project with its sessions from the last 30 days. Continuing one resumes the same session.")
                .foregroundStyle(.secondary)
            if let error = sessions.error {
                Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(Tint.danger)
            }
            List {
                if sessions.scanning && !sessions.scanned {
                    HStack {
                        ProgressView().controlSize(.small)
                        Text("Looking for sessions…").foregroundStyle(.secondary)
                    }
                } else if sessions.scanned && sessions.candidates.isEmpty {
                    Text("No Claude Code or Codex sessions on the server.").foregroundStyle(.secondary)
                }
                ForEach(sessions.candidates, id: \.path) { c in
                    Toggle(isOn: Binding(
                        get: { selected.contains(c.path) },
                        set: { on in if on { selected.insert(c.path) } else { selected.remove(c.path) } }
                    )) {
                        VStack(alignment: .leading, spacing: Space.xxs) {
                            HStack(spacing: Space.tight) {
                                Text(c.title).fontWeight(.medium)
                                if c.alreadyImported { Text("Project").font(.caption2).foregroundStyle(.secondary) }
                            }
                            Text(c.path).font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.head)
                            Text("\(c.sources) · \(c.detail)").font(.caption).foregroundStyle(.secondary)
                        }
                    }
                    .disabled(c.threadCount == 0 || sessions.importing)
                }
            }
            .frame(minHeight: 280)
            HStack {
                if sessions.importing {
                    ProgressView(value: Double(sessions.done), total: Double(max(sessions.total, 1)))
                        .frame(maxWidth: 200)
                    Text("Importing \(min(sessions.done + 1, sessions.total)) of \(sessions.total)…").foregroundStyle(.secondary)
                }
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Button(selected.count > 1 ? "Import \(selected.count) Folders" : "Import") {
                    core.update(.importAgentSessions(paths: sessions.candidates.map(\.path).filter(selected.contains)))
                }
                .keyboardShortcut(.defaultAction)
                .disabled(selected.isEmpty || sessions.importing)
            }
        }
        .padding(Space.l + Space.xs)
        .frame(width: 560, height: 520)
        .onAppear { core.update(.scanAgentSessions) }
        .onChange(of: sessions.scanned) { _, _ in preselect() }
        // A failed import stays open, its error showing.
        .onChange(of: sessions.importing) { was, now in if was, !now, sessions.error == nil { dismiss() } }
        .task { preselect() }
    }

    private func preselect() {
        guard sessions.scanned, !preselected else { return }
        preselected = true
        selected = Set(sessions.candidates.filter { $0.recent && !$0.alreadyImported }.map(\.path))
    }
}

/// The server's folders, for a server on another machine.
struct FolderBrowserSheet: View {
    @ObservedObject var core: Core
    @Environment(\.dismiss) private var dismiss

    private var browser: FolderBrowserView { core.view.folderBrowser }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s + Space.xxs) {
            Text("Add a Folder on the Server").font(.title2.bold())
            HStack {
                Button { core.update(.browseFolders(path: browser.parentPath)) } label: { Image(systemName: "chevron.up") }
                    .disabled(browser.parentPath.isEmpty)
                    .help("Enclosing folder")
                Text(browser.path).font(.callout.monospaced()).lineLimit(1).truncationMode(.head)
                Spacer()
                if browser.loading { ProgressView().controlSize(.small) }
            }
            if let error = browser.error {
                Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(Tint.danger)
            }
            List(browser.entries, id: \.fullPath) { entry in
                Button { core.update(.browseFolders(path: entry.fullPath)) } label: {
                    Label(entry.name, systemImage: "folder").frame(maxWidth: .infinity, alignment: .leading).contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
            .frame(minHeight: 280)
            HStack {
                if browser.isProject { Text("Already a project.").foregroundStyle(.secondary) }
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Add Project") {
                    core.update(.addProject(path: browser.path))
                    dismiss()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(browser.path.isEmpty || browser.loading || browser.isProject)
            }
        }
        .padding(Space.l + Space.xs)
        .frame(width: 520, height: 480)
        .onAppear { core.update(.browseFolders(path: browser.path)) }
    }
}
