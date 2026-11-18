import App
import AppKit
import SwiftUI

struct SidePanel: View {
    @ObservedObject var core: Core

    private var panel: PanelView { core.view.panel }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: Space.xxs) {
                ForEach(PanelTab.all, id: \.id) { tab in
                    PanelTabButton(tab: tab, selected: panel.tab == tab.id || (tab.id == "diff" && !PanelTab.ids.contains(panel.tab))) {
                        core.update(.showPanel(tab: tab.id))
                    }
                }
            }
            .padding(Space.s)
            Divider()
            switch panel.tab {
            case "terminal": TerminalPane(core: core, terminal: panel.terminal)
            case "files": FilesPane(core: core, files: panel.files)
            case "git": GitPane(core: core, git: panel.git)
            default: DiffPane(core: core, diff: panel.diff)
            }
        }
    }
}

/// The side panel's tabs.
struct PanelTab {
    let id: String
    let title: String
    let icon: String
    let shortcut: String

    static let all = [
        PanelTab(id: "diff", title: "Diff", icon: "plus.forwardslash.minus", shortcut: "⌘⇧D"),
        PanelTab(id: "terminal", title: "Terminal", icon: "terminal", shortcut: "⌘⇧T"),
        PanelTab(id: "files", title: "Files", icon: "folder", shortcut: "⌘⇧E"),
        PanelTab(id: "git", title: "Git", icon: "arrow.triangle.branch", shortcut: "⌥⌘G"),
    ]
    static let ids = Set(all.map(\.id))
}

private struct PanelTabButton: View {
    let tab: PanelTab
    let selected: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Label(tab.title, systemImage: tab.icon)
                .font(.callout.weight(selected ? .semibold : .regular))
                .foregroundStyle(selected ? .primary : .secondary)
                .frame(maxWidth: .infinity)
                .padding(.vertical, Space.tight)
                .contentShape(Rectangle())
                .background {
                    if selected {
                        RoundedRectangle(cornerRadius: Radius.chip, style: .continuous).fill(.fill.secondary)
                    }
                }
                .hoverHighlight(Radius.chip)
        }
        .buttonStyle(.plain)
        .help("\(tab.title) (\(tab.shortcut))")
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

// MARK: - Diff

struct DiffPane: View {
    @ObservedObject var core: Core
    let diff: DiffPanelView

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Picker("Changes", selection: Binding(get: { diff.selectedSource }, set: { core.update(.selectDiffSource(source: $0)) })) {
                    ForEach(diff.sources, id: \.value) { Text($0.label).tag($0.value) }
                }
                .labelsHidden()
                Spacer()
                Button { core.update(.selectDiffSource(source: diff.selectedSource)) } label: { Image(systemName: "arrow.clockwise") }
                    .buttonStyle(.borderless)
                    .help("Refresh")
                    .accessibilityLabel("Refresh diff")
            }
            .padding(.horizontal, Space.m)
            .padding(.vertical, Space.s)
            Divider()
            if diff.loading {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if let error = diff.error {
                ContentUnavailableView("No diff", systemImage: "doc.text.magnifyingglass", description: Text(error))
            } else if diff.files.isEmpty {
                ContentUnavailableView("No changes", systemImage: "checkmark.seal")
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: Space.m) {
                        Text("\(diff.files.count) file\(diff.files.count == 1 ? "" : "s") · +\(diff.files.reduce(0) { $0 + $1.additions }) −\(diff.files.reduce(0) { $0 + $1.deletions })")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        ForEach(diff.files, id: \.path) { file in
                            DiffFileCard(file: file)
                        }
                        if diff.truncated {
                            Text("The diff was truncated.").font(.caption).foregroundStyle(Tint.caution)
                        }
                    }
                    .padding(Space.m)
                }
            }
        }
    }
}

struct DiffFileCard: View {
    let file: DiffFileView
    @State private var expanded = true

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Button { expanded.toggle() } label: {
                HStack {
                    Image(systemName: expanded ? "chevron.down" : "chevron.right").font(.caption2.bold())
                    Text(file.oldPath.map { "\($0) → \(file.path)" } ?? file.path)
                        .font(TypeRole.mono)
                        .lineLimit(1)
                        .truncationMode(.head)
                    Spacer()
                    if file.change != "modified" {
                        Text(file.change).font(.caption2).padding(.horizontal, Space.xs + 1).background(.quaternary, in: Capsule())
                    }
                    Text("+\(file.additions)").foregroundStyle(Tint.success).font(.caption.monospacedDigit())
                    Text("−\(file.deletions)").foregroundStyle(Tint.danger).font(.caption.monospacedDigit())
                }
                .padding(.horizontal, Space.m)
                .padding(.vertical, Space.s)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .background(.fill.tertiary)
            if expanded {
                ScrollView(.horizontal) {
                    DiffFileBody(file: file).fixedSize(horizontal: true, vertical: false)
                }
                .defaultScrollAnchor(.leading)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .clipShape(RoundedRectangle(cornerRadius: Radius.chip + 2, style: .continuous))
        .cardSurface(Radius.chip + 2)
    }
}

// MARK: - Files

struct FilesPane: View {
    @ObservedObject var core: Core
    let files: FilesPanelView
    @State private var query = ""

    var body: some View {
        VSplitView {
            VStack(spacing: 0) {
                TextField("Search files", text: $query)
                    .textFieldStyle(.roundedBorder)
                    .padding(Space.s)
                    .onChange(of: query) { _, q in core.update(.setFileQuery(query: q)) }
                List {
                    if !files.query.isEmpty {
                        ForEach(files.results, id: \.path) { entry in
                            FileRow(core: core, entry: entry, showPath: true)
                        }
                        if files.results.isEmpty, !files.loading {
                            Text("No files match “\(files.query)”").foregroundStyle(.secondary)
                        }
                    } else {
                        if files.loading { ProgressView() }
                        ForEach(files.entries, id: \.path) { entry in
                            FileRow(core: core, entry: entry, showPath: false)
                        }
                    }
                }
                .listStyle(.plain)
            }
            .frame(minHeight: 160)
            // Back on this tab, the field shows the search still applied.
            .onAppear { if query != files.query { query = files.query } }
            if let preview = files.preview {
                VStack(alignment: .leading, spacing: 0) {
                    HStack {
                        Text(preview.path).font(TypeRole.monoSmall).lineLimit(1).truncationMode(.head)
                        Spacer()
                        Button { Platform.copy(preview.text) } label: { Image(systemName: "doc.on.doc") }
                            .buttonStyle(.borderless)
                            .help("Copy the file's text")
                            .accessibilityLabel("Copy file text")
                    }
                    .padding(Space.s)
                    Divider()
                    if preview.loading {
                        ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
                    } else if let error = preview.error {
                        Text(error).foregroundStyle(.secondary).padding()
                    } else {
                        ScrollView([.vertical, .horizontal]) {
                            Text(CodeHighlight.text(preview.spans, fallback: preview.text))
                                .font(TypeRole.code)
                                .textSelection(.enabled)
                                .fixedSize()
                                .padding(Space.s)
                        }
                        .defaultScrollAnchor(.topLeading)
                        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                        if preview.truncated {
                            Text("Showing the first megabyte.").font(.caption).foregroundStyle(Tint.caution).padding(Space.tight)
                        }
                    }
                }
                .frame(minHeight: 160)
            }
        }
    }
}

struct FileRow: View {
    @ObservedObject var core: Core
    let entry: FileEntryView
    let showPath: Bool

    var body: some View {
        Button {
            if entry.directory {
                core.update(.toggleDirectory(path: entry.path))
            } else {
                core.update(.openFile(path: entry.path))
            }
        } label: {
            HStack(spacing: Space.xs) {
                if entry.directory {
                    Image(systemName: entry.expanded ? "chevron.down" : "chevron.right").font(.caption2).frame(width: 10)
                    Image(systemName: "folder.fill").foregroundStyle(.blue.opacity(0.8))
                } else {
                    Spacer().frame(width: 10)
                    Image(systemName: "doc").foregroundStyle(.secondary)
                }
                Text(showPath ? entry.path : entry.name).lineLimit(1).truncationMode(.head)
                Spacer()
            }
            .padding(.leading, showPath ? 0 : CGFloat(entry.depth) * Space.m)
            .opacity(entry.ignored ? 0.5 : 1)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .font(.callout)
    }
}

// MARK: - Git

struct GitPane: View {
    @ObservedObject var core: Core
    let git: GitPanelView
    @State private var message = ""
    @State private var newBranch = ""
    @State private var showNewBranch = false
    /// A commit that also pushes, waiting for a yes.
    @State private var confirming: String?
    /// A commit under way: its message clears once it has gone through.
    @State private var committing = false

    var body: some View {
        if !git.isRepo {
            ContentUnavailableView("Not a git repository", systemImage: "arrow.triangle.branch", description: Text("This folder is not under version control."))
        } else {
            Form {
                Section {
                    HStack {
                        Menu {
                            ForEach(git.branches, id: \.self) { b in
                                Button(b) { core.update(.switchBranch(name: b)) }
                            }
                            Divider()
                            Button("New Branch…") { showNewBranch = true }
                        } label: {
                            Label(git.branch ?? "Detached HEAD", systemImage: "arrow.triangle.branch")
                        }
                        Spacer()
                        if git.hasUpstream {
                            Text("↑\(git.ahead) ↓\(git.behind)").font(.caption.monospacedDigit()).foregroundStyle(.secondary)
                        }
                        Button { core.update(.refreshGit) } label: { Image(systemName: "arrow.clockwise") }
                            .buttonStyle(.borderless)
                            .help("Refresh")
                            .accessibilityLabel("Refresh git status")
                    }
                    if let pr = git.pullRequest {
                        Link(destination: URL(string: pr.url) ?? URL(string: "about:blank")!) {
                            Label("#\(pr.number) \(pr.title)", systemImage: "arrow.triangle.pull")
                        }
                    }
                }
                Section("Changes (+\(git.insertions) −\(git.deletions))") {
                    if git.changedFiles.isEmpty {
                        Text("Working tree clean").foregroundStyle(.secondary)
                    }
                    ForEach(git.changedFiles, id: \.path) { f in
                        HStack {
                            Text(f.path).font(TypeRole.mono).lineLimit(1).truncationMode(.head)
                            Spacer()
                            Text("+\(f.insertions)").foregroundStyle(Tint.success).font(.caption.monospacedDigit())
                            Text("−\(f.deletions)").foregroundStyle(Tint.danger).font(.caption.monospacedDigit())
                        }
                    }
                }
                Section("Commit") {
                    TextField("Commit message", text: $message, axis: .vertical).lineLimit(2...5)
                    // One button (the push variants in its menu) fits a
                    // narrow panel where three in a row were cut short.
                    Menu("Commit") {
                        Button("Commit & Push…") { confirming = "commit_push" }.disabled(!git.hasRemote)
                        Button("Commit, Push & Open PR…") { confirming = "commit_push_pr" }.disabled(!git.hasRemote)
                    } primaryAction: {
                        run("commit")
                    }
                    .menuStyle(.button)
                    .fixedSize()
                    .disabled(git.changedFiles.isEmpty || message.isEmpty)
                    HStack {
                        Button("Push") { run("push") }.disabled(!git.hasRemote || git.ahead == 0 && git.hasUpstream)
                        Button("Pull") { core.update(.gitPull) }.disabled(!git.hasUpstream || git.behind == 0)
                        Button("Create PR") { run("create_pr") }.disabled(!git.hasRemote)
                        if git.busy { ProgressView().controlSize(.small) }
                    }
                }
                .disabled(git.busy)
                if !git.progress.isEmpty || git.error != nil {
                    Section("Activity") {
                        ForEach(Array(git.progress.enumerated()), id: \.offset) { _, line in
                            Text(line).font(TypeRole.monoSmall)
                        }
                        if let error = git.error {
                            Text(error).foregroundStyle(Tint.danger).font(.caption)
                        }
                    }
                }
            }
            .formStyle(.grouped)
            .confirmationDialog(
                "Commit and push to \(git.branch ?? "the branch")?",
                isPresented: Binding(get: { confirming != nil }, set: { if !$0 { confirming = nil } }),
                titleVisibility: .visible,
                presenting: confirming
            ) { action in
                Button(action == "commit_push_pr" ? "Commit, Push & Open PR" : "Commit & Push") { run(action) }
            } message: { _ in
                Text("Commits ^[\(git.changedFiles.count) changed file](inflect: true) and pushes to the remote.")
            }
            .onChange(of: git.busy) { _, busy in
                guard !busy, committing else { return }
                committing = false
                if git.error == nil { message = "" }
            }
            .alert("New Branch", isPresented: $showNewBranch) {
                TextField("Name", text: $newBranch)
                Button("Create") {
                    core.update(.createBranch(name: newBranch))
                    newBranch = ""
                }
                Button("Cancel", role: .cancel) {}
            }
        }
    }

    private func run(_ action: String) {
        core.update(.gitAction(action: action, message: message))
        if action.hasPrefix("commit") { committing = true }
    }
}
