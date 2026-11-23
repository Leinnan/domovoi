import App
import SwiftUI

/// The repository's worktrees in the git panel: which branch each has, the
/// threads working there, removing one, and making a new one.
struct WorktreesSection: View {
    let git: GitPanelView
    let update: (Event) -> Void
    @State private var removing: Removal?
    @State private var creating = false

    private struct Removal: Identifiable {
        let worktree: WorktreeView
        let force: Bool
        var id: String { worktree.path + (force ? "!" : "") }
    }

    var body: some View {
        Section {
            ForEach(git.worktrees, id: \.path) { worktree in
                // A settings row's columns (`SettingsRow`), with a badge and a third line.
                HStack(alignment: .firstTextBaseline, spacing: Space.m) {
                    Image(systemName: "arrow.triangle.branch").foregroundStyle(worktree.current ? Color.accentColor : .secondary)
                        .iconColumn(Size.listIcon)
                    VStack(alignment: .leading, spacing: Space.xxs) {
                        HStack(spacing: Space.xs) {
                            Text(worktree.branch.isEmpty ? "Detached" : worktree.branch).lineLimit(1)
                            if worktree.current {
                                Text("This thread").font(TypeRole.metaSmall.weight(.semibold)).foregroundStyle(.tint)
                            }
                        }
                        Text(worktree.path).font(TypeRole.monoSmall).foregroundStyle(.secondary).lineLimit(1).truncationMode(.head)
                        if !worktree.threads.isEmpty {
                            Text("^[\(worktree.threads.count) thread](inflect: true)")
                                .font(TypeRole.meta)
                                .foregroundStyle(.secondary)
                                .help(worktree.threads.joined(separator: "\n"))
                        }
                    }
                    Spacer(minLength: Space.s)
                    // The open thread's own worktree isn't removed from under it.
                    if !worktree.current {
                        Menu {
                            Button("Remove…", role: .destructive) { removing = Removal(worktree: worktree, force: false) }
                            Button("Force Remove…", role: .destructive) { removing = Removal(worktree: worktree, force: true) }
                        } label: {
                            Image(systemName: "ellipsis.circle").foregroundStyle(.secondary)
                                .tapTarget(width: true)
                        }
                        .menuIndicator(.hidden)
                        .fixedSize()
                        .buttonStyle(.borderless)
                        .accessibilityLabel("Worktree actions")
                    }
                }
                .accessibilityElement(children: .combine)
            }
            Button("New Worktree…", systemImage: "plus") { creating = true }
                .disabled(git.busy || git.branches.isEmpty)
        } header: {
            Text("Worktrees")
        }
        .confirmationDialog(
            removing.map { $0.force ? "Force remove the worktree of \($0.worktree.branch)?" : "Remove the worktree of \($0.worktree.branch)?" } ?? "",
            isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }),
            titleVisibility: .visible,
            presenting: removing
        ) { removal in
            Button(removal.force ? "Force Remove" : "Remove", role: .destructive) {
                update(.removeWorktree(path: removal.worktree.path, force: removal.force))
            }
        } message: { removal in
            let threads = removal.worktree.threads.count
            Text((removal.force ? "Its folder goes, uncommitted changes included." : "Its folder goes; a worktree with changes isn't removed.")
                + (threads > 0 ? " \(threads == 1 ? "A thread works" : "\(threads) threads work") there." : ""))
        }
        .sheet(isPresented: $creating) {
            NewWorktreeSheet(branches: git.branches, current: git.branch) { base, branch in
                update(.createWorktree(base: base, branch: branch))
            }
        }
    }
}

/// A new worktree: the branch it starts from, and the branch it gets.
private struct NewWorktreeSheet: View {
    let branches: [String]
    let current: String?
    let create: (_ base: String, _ branch: String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var base = ""
    @State private var branch = ""

    var body: some View {
        NavigationStack {
            Form {
                Picker("Start From", selection: $base) {
                    ForEach(branches, id: \.self) { Text($0).tag($0) }
                }
                TextField("New branch (empty: check out the base)", text: $branch)
                    .autocorrectionDisabled()
                    #if os(iOS)
                    .textInputAutocapitalization(.never)
                    #endif
            }
            .formStyle(.grouped)
            .navigationTitle("New Worktree")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Create") {
                        create(base, branch.trimmingCharacters(in: .whitespaces))
                        dismiss()
                    }
                    .disabled(base.isEmpty)
                }
            }
        }
        #if os(macOS)
        .frame(width: 420, height: 220)
        #endif
        .onAppear { base = current.flatMap { branches.contains($0) ? $0 : nil } ?? branches.first ?? "" }
    }
}

extension ScriptView {
    /// An SF Symbol for the script's kind of icon.
    var symbol: String {
        switch icon {
        case "test": "checkmark.seal"
        case "lint": "wand.and.stars"
        case "configure": "gearshape"
        case "build": "hammer"
        case "debug": "ladybug"
        default: "play"
        }
    }
}

/// The project's scripts, for the thread's toolbar: one runs in a new terminal.
struct ScriptsMenu: View {
    let scripts: [ScriptView]
    let run: (ScriptView) -> Void

    var body: some View {
        Menu {
            ForEach(scripts, id: \.id) { script in
                Button { run(script) } label: { Label(script.name, systemImage: script.symbol) }
                    .help(script.command)
            }
        } label: {
            Label("Run Script", systemImage: "play")
        }
        .help("Run a project script in a terminal")
    }
}

/// The scripts menu in a thread's toolbar, when the project has scripts. A
/// view of its own: it reads the panel, which changes with terminal output.
struct ScriptsToolbarButton: View {
    @ObservedObject var core: Core
    /// After the script starts (iOS shows the terminal).
    var ran: () -> Void = {}

    var body: some View {
        let scripts = core.view.panel.git.scripts
        if !scripts.isEmpty {
            ScriptsMenu(scripts: scripts) { script in
                core.update(.runScript(id: script.id))
                ran()
            }
        }
    }
}

/// A file's text being changed: monospace, with Save (⌘S) and Cancel.
/// The caller keeps the text, so it knows when leaving would lose changes.
/// On iOS Save and Cancel are in the navigation bar, as in other editors;
/// on the Mac they're in a bar under the text.
struct FileEditor: View {
    @Binding var text: String
    let changed: Bool
    let save: () -> Void
    let cancel: () -> Void

    #if os(iOS)
    /// Where the text starts, editing or not: the preview pads its text by
    /// this, and the editor by this less the text view's own inset (5 pt at
    /// the sides, 8 pt above and below in UIKit), so Edit doesn't move it.
    static let inset: CGFloat = Space.m
    #endif

    var body: some View {
        #if os(iOS)
        TextEditor(text: $text)
            .font(TypeRole.code)
            .autocorrectionDisabled()
            .textInputAutocapitalization(.never)
            .scrollContentBackground(.hidden)
            .padding(.horizontal, Self.inset - 5)
            .padding(.vertical, Self.inset - 8)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel", action: cancel) }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save", action: save)
                        .keyboardShortcut("s")
                        .disabled(!changed)
                }
            }
        #else
        VStack(spacing: 0) {
            TextEditor(text: $text)
                .font(TypeRole.code)
                .autocorrectionDisabled()
                .scrollContentBackground(.hidden)
                .padding(Space.xs)
            Divider()
            HStack {
                if changed { Text("Edited").font(TypeRole.meta).foregroundStyle(.secondary) }
                Spacer()
                Button("Cancel", action: cancel)
                Button("Save", action: save)
                    .keyboardShortcut("s")
                    .buttonStyle(.borderedProminent)
                    .disabled(!changed)
                    .help("Save (⌘S)")
            }
            .padding(Space.s)
        }
        #endif
    }
}
