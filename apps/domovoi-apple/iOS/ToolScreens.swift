import App
import SwiftUI
import UserNotifications

struct DiffScreen: View {
    @ObservedObject var core: Core
    @Environment(\.dismiss) private var dismiss

    private var diff: DiffPanelView { core.view.panel.diff }

    var body: some View {
        List {
            if diff.sources.count > 1 {
                Picker("Show", selection: Binding(
                    get: { diff.selectedSource },
                    set: { core.update(.selectDiffSource(source: $0)) }
                )) {
                    ForEach(diff.sources, id: \.value) { Text($0.label).tag($0.value) }
                }
            }
            if diff.loading {
                LoadingRow("Loading changes…")
            } else if let error = diff.error {
                ErrorRow(error)
            }
            ForEach(diff.files, id: \.path) { file in
                DisclosureGroup {
                    ScrollView(.horizontal) {
                        DiffFileBody(file: file, onLine: core.view.thread == nil ? nil : { line in
                            // Quoted into the thread's message, which opens again.
                            let number = line.newLine ?? line.oldLine
                            let place = number.map { "\(file.path):\($0)" } ?? file.path
                            let marker = line.kind == "add" ? "+" : (line.kind == "del" ? "-" : " ")
                            core.update(.appendToDraft(text: "`\(place)`\n```\n\(marker)\(line.text)\n```\n"))
                            dismiss()
                        }).fixedSize()
                    }
                    .defaultScrollAnchor(.leading)
                    .listRowInsets(EdgeInsets())
                } label: {
                    FileChangeRow(path: file.path, added: Int(file.additions), removed: Int(file.deletions), oldPath: file.oldPath, change: file.change)
                }
            }
            if diff.truncated {
                Text("Large diff: some files are not shown.").font(TypeRole.meta).foregroundStyle(.secondary)
            }
        }
        .overlay {
            if !diff.loading, diff.error == nil, diff.files.isEmpty {
                ContentUnavailableView("No Changes", systemImage: "checkmark.circle", description: Text("Nothing here differs from the last commit."))
            }
        }
        .navigationTitle("Changes")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { core.update(.showPanel(tab: "diff")) }
        .refreshable { await core.refresh(.showPanel(tab: "diff")) { core.view.panel.diff.loading } }
    }
}

struct FilesScreen: View {
    private struct Selection: Identifiable {
        let id: String
    }

    @ObservedObject var core: Core
    @State private var query = ""
    @State private var previewing: Selection?
    /// The preview has edits not saved: leaving it asks first.
    @State private var editsUnsaved = false
    /// The preview is in its editor, whose Save and Cancel take the bar.
    @State private var previewEditing = false
    @State private var confirmDiscard = false

    /// A file to preview as soon as the screen shows (picked in the palette).
    @MainActor static var reveal: String?

    private var files: FilesPanelView { core.view.panel.files }

    var body: some View {
        List {
            let entries = query.isEmpty ? files.entries : files.results
            ForEach(entries, id: \.path) { entry in
                Button {
                    if entry.directory {
                        core.update(.toggleDirectory(path: entry.path))
                    } else {
                        core.update(.openFile(path: entry.path))
                        previewing = Selection(id: entry.path)
                    }
                } label: {
                    HStack(spacing: Space.s) {
                        // The chevron's column is kept for files too, so names line up.
                        if entry.directory {
                            DisclosureChevron(open: entry.expanded)
                        } else {
                            Color.clear.frame(width: Size.chevron, height: 1)
                        }
                        Image(systemName: entry.directory ? "folder.fill" : "doc").foregroundStyle(entry.directory ? Color.accentColor : .secondary)
                            .iconColumn(Size.rowIcon)
                        Text(query.isEmpty ? entry.name : entry.path)
                            .foregroundStyle(entry.ignored ? .secondary : .primary)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    // The same step as the process tree's.
                    .padding(.leading, query.isEmpty ? CGFloat(entry.depth) * Space.m : 0)
                }
                .contextMenu {
                    Button("Copy Path", systemImage: "doc.on.doc") { Platform.copy(entry.path) }
                    if core.view.thread != nil {
                        Button("Add to Message", systemImage: "text.bubble") {
                            core.update(.appendToDraft(text: "@\(entry.path) "))
                        }
                    }
                }
            }
            // More coming into a tree already shown (a folder opening).
            if files.loading, !entries.isEmpty { LoadingRow("Loading…") }
        }
        .overlay {
            if (query.isEmpty ? files.entries : files.results).isEmpty {
                if files.loading {
                    ProgressView()
                } else if query.isEmpty {
                    ContentUnavailableView("No Files", systemImage: "folder", description: Text(core.view.connection.state == "connected" ? "This folder is empty." : "Not connected to the server."))
                } else {
                    ContentUnavailableView.search(text: query)
                }
            }
        }
        .listStyle(.plain)
        .searchable(text: $query, prompt: "Find a file")
        .onChange(of: query) { _, q in core.update(.setFileQuery(query: q)) }
        .navigationTitle((files.root as NSString).lastPathComponent)
        .navigationBarTitleDisplayMode(.inline)
        .onAppear {
            core.update(.showPanel(tab: "files"))
            if let path = Self.reveal {
                Self.reveal = nil
                previewing = Selection(id: path)
            }
        }
        .sheet(item: $previewing, onDismiss: { previewEditing = false }) { selection in
            NavigationStack {
                FilePreviewScreen(core: core, path: selection.id, unsaved: $editsUnsaved, editing: $previewEditing) { path in
                    core.update(.appendToDraft(text: "@\(path) "))
                    previewing = nil
                }
                .toolbar {
                    if !previewEditing {
                        ToolbarItem(placement: .confirmationAction) {
                            Button("Done") { if editsUnsaved { confirmDiscard = true } else { previewing = nil } }
                        }
                    }
                }
                .confirmationDialog("Discard your changes?", isPresented: $confirmDiscard, titleVisibility: .visible) {
                    Button("Discard Changes", role: .destructive) {
                        editsUnsaved = false
                        previewing = nil
                    }
                } message: {
                    Text("They haven't been saved.")
                }
            }
            // A swipe down doesn't throw edits away either.
            .interactiveDismissDisabled(editsUnsaved)
        }
    }
}

struct FilePreviewScreen: View {
    @ObservedObject var core: Core
    let path: String
    /// Edits not saved yet (the screen holding this one asks before leaving).
    @Binding var unsaved: Bool
    /// In the editor (the screen holding this one leaves the bar to it).
    @Binding var editing: Bool
    /// Mentions the file in the open thread's message, when there is one.
    var addToMessage: ((String) -> Void)?
    /// Long lines wrap instead of scrolling sideways.
    @AppStorage("filePreviewWraps") private var wraps = false
    /// The text being edited (`nil`: not editing), and what it started from.
    @State private var editText: String?
    @State private var editOriginal = ""
    /// Text sent to be saved, until the preview shows it.
    @State private var saving: String?
    @State private var confirmDiscard = false
    /// An image file's bytes once its preview loaded them (Share, Copy, Save).
    @State private var imageFile: ImageFile?

    private var preview: FilePreviewView? {
        guard let preview = core.view.panel.files.preview, preview.path == path else { return nil }
        return preview
    }

    var body: some View {
        Group {
            if let preview, editText != nil {
                FileEditor(
                    text: Binding(get: { editText ?? "" }, set: { editText = $0 }),
                    changed: editText != editOriginal,
                    save: {
                        saving = editText
                        core.update(.saveFile(path: preview.path, text: editText ?? ""))
                    },
                    cancel: { if editText != editOriginal { confirmDiscard = true } else { stopEditing() } }
                )
            } else if let preview, !preview.loading {
                if let error = preview.error {
                    ContentUnavailableView(error, systemImage: "doc.questionmark")
                } else {
                    VStack(spacing: 0) {
                        if let lfs = preview.lfs {
                            LfsCard(lfs: lfs) { core.update(.fetchLfsObject(path: preview.path)) }
                                .padding(.horizontal, Space.s)
                            Divider()
                        }
                        if let address = preview.imageUrl {
                            FileImagePreview(address: address, path: preview.path, file: $imageFile)
                        } else {
                            ScrollView(wraps ? [.vertical] : [.vertical, .horizontal]) {
                                Text(CodeHighlight.text(preview.spans, fallback: preview.text) + AttributedString(preview.truncated ? "\n…" : ""))
                                    .font(TypeRole.code)
                                    .textSelection(.enabled)
                                    // The editor's inset, so the text doesn't move on Edit.
                                    .padding(FileEditor.inset)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                            }
                            .defaultScrollAnchor(.topLeading)
                        }
                    }
                }
            } else {
                ProgressView()
            }
        }
        .navigationTitle((path as NSString).lastPathComponent)
        .navigationBarTitleDisplayMode(.inline)
        .onChange(of: editText) { _, text in
            unsaved = text != nil && text != editOriginal
            editing = text != nil
        }
        // Saved: the preview comes back with the new text.
        .onChange(of: preview?.text) { _, text in
            if let saved = saving, text == saved { stopEditing() }
        }
        .confirmationDialog("Discard your changes?", isPresented: $confirmDiscard, titleVisibility: .visible) {
            Button("Discard Changes", role: .destructive) { stopEditing() }
        }
        .toolbar {
            if let preview, preview.editable, !preview.loading, preview.error == nil, editText == nil {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Edit", systemImage: "pencil") {
                        editOriginal = preview.text
                        editText = preview.text
                    }
                }
            }
            if let preview, !preview.loading, preview.error == nil, preview.imageUrl == nil, !preview.truncated,
               ["html", "htm", "svg"].contains((preview.path as NSString).pathExtension.lowercased()) {
                ToolbarItem(placement: .topBarTrailing) { HtmlArtifactButton(source: preview.text, title: preview.path) }
            }
            // Not while editing: Cancel takes that side of the bar.
            if let preview, preview.error == nil, editText == nil {
                ToolbarItem(placement: .topBarLeading) {
                    Menu {
                        Button("Copy Path", systemImage: "doc.on.doc") { Platform.copy(preview.path) }
                        if preview.imageUrl != nil, let imageFile {
                            ImageActionItems(file: imageFile, name: (preview.path as NSString).lastPathComponent, preview: nil)
                        }
                        if !preview.loading, preview.imageUrl == nil {
                            Toggle("Wrap Lines", systemImage: "text.word.spacing", isOn: $wraps)
                            Button("Copy Text", systemImage: "doc.on.clipboard") { Platform.copy(preview.text) }
                            ShareLink(item: preview.text, subject: Text((preview.path as NSString).lastPathComponent)) {
                                Label("Share", systemImage: "square.and.arrow.up")
                            }
                        }
                        if let addToMessage {
                            Button("Ask About This File", systemImage: "text.bubble") { addToMessage(preview.path) }
                        }
                    } label: { Image(systemName: "ellipsis.circle") }
                    .accessibilityLabel("File actions")
                }
            }
        }
    }

    private func stopEditing() {
        editText = nil
        saving = nil
        unsaved = false
        editing = false
    }
}

struct GitScreen: View {
    @ObservedObject var core: Core
    @State private var message = ""
    @State private var newBranch = ""
    @State private var showNewBranch = false
    /// A stacked action that pushes, waiting for a yes.
    @State private var confirming: String?
    /// A branch to switch to over uncommitted changes, waiting for a yes.
    @State private var switchingTo: String?
    /// A commit under way: its message clears once it has gone through.
    @State private var committing = false

    private var git: GitPanelView { core.view.panel.git }
    private var hasChanges: Bool { !git.changedFiles.isEmpty }
    private var hasMessage: Bool { !message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
    /// Commits pending a push: ahead of the upstream, or a branch never pushed.
    private var canPush: Bool { git.hasRemote && (git.ahead > 0 || !git.hasUpstream) }

    var body: some View {
        Form {
            if !git.loading, git.isRepo {
                Section {
                    Menu {
                        ForEach(git.branches, id: \.self) { b in
                            Button {
                                if hasChanges, b != git.branch { switchingTo = b } else { core.update(.switchBranch(name: b)) }
                            } label: {
                                if b == git.branch { Label(b, systemImage: "checkmark") } else { Text(b) }
                            }
                        }
                        Divider()
                        Button("New Branch…", systemImage: "plus") { showNewBranch = true }
                    } label: {
                        LabeledContent("Branch") {
                            HStack(spacing: Space.xs) {
                                Text(git.branch ?? "detached").lineLimit(1)
                                Image(systemName: "chevron.up.chevron.down").font(.caption2)
                            }
                        }
                    }
                    .disabled(git.busy)
                    if git.hasUpstream {
                        LabeledContent("Upstream", value: "↑\(git.ahead) ↓\(git.behind)")
                    }
                    if let pr = git.pullRequest {
                        if let url = URL(string: pr.url) {
                            Link(destination: url) {
                                LabeledContent("Pull request", value: "#\(pr.number) \(pr.state)")
                            }
                        } else {
                            LabeledContent("Pull request", value: "#\(pr.number) \(pr.state)")
                        }
                    }
                }
                Section(hasChanges ? "\(git.changedFiles.count) changed · +\(git.insertions) −\(git.deletions)" : "Changes") {
                    if !hasChanges {
                        Text("Working tree clean").foregroundStyle(.secondary)
                    }
                    ForEach(git.changedFiles, id: \.path) { file in
                        FileChangeRow(path: file.path, added: Int(file.insertions), removed: Int(file.deletions))
                    }
                }
                if hasChanges {
                    Section {
                        TextField("Commit message", text: $message, axis: .vertical).lineLimit(1...5)
                        Button("Commit") { run("commit") }
                            .disabled(git.busy || !hasMessage)
                        if git.hasRemote {
                            Button("Commit & Push…") { confirming = "commit_push" }
                                .disabled(git.busy || !hasMessage)
                            if git.pullRequest == nil {
                                Button("Commit, Push & Open PR…") { confirming = "commit_push_pr" }
                                    .disabled(git.busy || !hasMessage)
                            }
                        }
                    } header: {
                        Text("Commit")
                    } footer: {
                        if !hasMessage { Text("Commits need a message.") }
                    }
                }
                if git.hasRemote {
                    Section("Remote") {
                        if canPush {
                            Button(git.hasUpstream ? "Push \(git.ahead) Commit\(git.ahead == 1 ? "" : "s")…" : "Publish Branch…") { confirming = "push" }
                                .disabled(git.busy)
                        }
                        Button("Pull") { core.update(.gitPull) }
                            .disabled(git.busy || !git.hasUpstream || git.behind == 0)
                        if git.pullRequest == nil {
                            Button("Create Pull Request…") { confirming = "create_pr" }
                                .disabled(git.busy || git.branch == nil)
                        }
                    }
                }
                WorktreesSection(git: git) { core.update($0) }
                if git.busy || !git.progress.isEmpty || git.error != nil {
                    Section {
                        if git.busy { LoadingRow("Working…") }
                        ForEach(Array(git.progress.suffix(6).enumerated()), id: \.offset) { _, line in
                            Text(line).font(TypeRole.monoSmall).foregroundStyle(.secondary)
                        }
                        if let error = git.error { ErrorRow(error) }
                    }
                }
            }
        }
        .overlay {
            if git.loading {
                ProgressView("Reading the repository…")
            } else if !git.isRepo {
                ContentUnavailableView("Not a Git Repository", systemImage: "arrow.triangle.branch", description: Text("This folder isn't tracked by git."))
            }
        }
        .navigationTitle("Git")
        .navigationBarTitleDisplayMode(.inline)
        .confirmationDialog(
            confirmTitle,
            isPresented: Binding(get: { confirming != nil }, set: { if !$0 { confirming = nil } }),
            titleVisibility: .visible,
            presenting: confirming
        ) { action in
            Button(confirmButton(action)) { run(action) }
        } message: { action in
            switch action {
            case "push": Text("Pushes \(git.hasUpstream ? "^[\(Int(git.ahead)) commit](inflect: true)" : "the branch") to the remote.")
            case "create_pr": Text("Pushes \(git.branch ?? "the branch") if needed and opens a pull request for it on GitHub.")
            default: Text("Commits ^[\(git.changedFiles.count) changed file](inflect: true) and pushes to the remote.")
            }
        }
        .confirmationDialog(
            switchingTo.map { "Switch to \($0)?" } ?? "",
            isPresented: Binding(get: { switchingTo != nil }, set: { if !$0 { switchingTo = nil } }),
            titleVisibility: .visible,
            presenting: switchingTo
        ) { branch in
            Button("Switch Branch") { core.update(.switchBranch(name: branch)) }
        } message: { _ in
            Text("There are ^[\(git.changedFiles.count) uncommitted change](inflect: true). Git carries them over when it can and refuses when they'd conflict.")
        }
        .onChange(of: git.busy) { _, busy in
            guard !busy, committing else { return }
            committing = false
            if git.error == nil { message = "" }
        }
        .alert("New Branch", isPresented: $showNewBranch) {
            TextField("Name", text: $newBranch)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
            Button("Create") {
                let name = newBranch.trimmingCharacters(in: .whitespaces)
                if !name.isEmpty { core.update(.createBranch(name: name)) }
                newBranch = ""
            }
            Button("Cancel", role: .cancel) { newBranch = "" }
        } message: {
            Text("Creates the branch from \(git.branch ?? "HEAD") and switches to it.")
        }
        .onAppear {
            core.update(.showPanel(tab: "git"))
            core.update(.refreshGit)
        }
        .refreshable { await core.refresh(.refreshGit) { core.view.panel.git.loading } }
    }

    private var confirmTitle: String {
        switch confirming {
        case "push": "Push \(git.branch ?? "the branch")?"
        case "create_pr": "Open a pull request for \(git.branch ?? "the branch")?"
        default: "Commit and push to \(git.branch ?? "the branch")?"
        }
    }

    private func confirmButton(_ action: String) -> String {
        switch action {
        case "commit_push_pr": "Commit, Push & Open PR"
        case "push": git.hasUpstream ? "Push" : "Publish"
        case "create_pr": "Create Pull Request"
        default: "Commit & Push"
        }
    }

    private func run(_ action: String) {
        core.update(.gitAction(action: action, message: message))
        // The message stays until the commit has worked.
        if action.hasPrefix("commit") { committing = true }
    }
}

struct PullRequestsScreen: View {
    @ObservedObject var core: Core
    @State private var showing = false

    private var prs: PullRequestsView { core.view.pullRequests }

    var body: some View {
        List {
            if let notice = prs.notice { Text(notice).font(.footnote).foregroundStyle(.secondary) }
            if let error = prs.error { ErrorRow(error) }
            ForEach(prs.items, id: \.key) { pr in
                Button {
                    core.update(.selectPullRequest(key: pr.key))
                    showing = true
                } label: {
                    VStack(alignment: .leading, spacing: Space.xxs) {
                        Text(pr.title).foregroundStyle(.primary).lineLimit(2)
                        Text("\(pr.repository) #\(pr.number) · \(pr.author) · \(pr.age)").font(TypeRole.meta).foregroundStyle(.secondary)
                        HStack(spacing: Space.tight) {
                            Text(pr.draft ? "Draft" : pr.state.capitalized).font(TypeRole.metaSmall.bold())
                            if !pr.checks.isEmpty { Text("Checks: \(pr.checks)").font(TypeRole.metaSmall).foregroundStyle(.secondary) }
                        }
                    }
                }
            }
            if prs.loading, !prs.items.isEmpty { LoadingRow("Refreshing…") }
        }
        .overlay {
            // Not over a notice (it says why there are none).
            if prs.items.isEmpty, prs.error == nil, prs.notice == nil {
                if prs.loading {
                    ProgressView()
                } else {
                    ContentUnavailableView("No Pull Requests", systemImage: "arrow.triangle.pull", description: Text("Open pull requests on this project's repositories show up here."))
                }
            }
        }
        .navigationTitle("Pull Requests")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { core.update(.loadPullRequests) }
        .refreshable { await core.refresh(.loadPullRequests) { core.view.pullRequests.loading } }
        .sheet(isPresented: $showing) {
            NavigationStack {
                PullRequestDetailScreen(core: core, detail: prs.selected, busy: prs.busy)
                    .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { showing = false } } }
            }
        }
        // A thread from a pull request: its composer, in the PR's worktree.
        .onChange(of: core.view.composer.prWorktree) { _, label in
            guard !label.isEmpty else { return }
            showing = false
            IOSSnapshots.navigate.send([.newThread])
        }
    }
}

struct PullRequestDetailScreen: View {
    @ObservedObject var core: Core
    let detail: PullRequestDetailView?
    let busy: Bool
    @State private var merging: String?
    @State private var closing = false
    @State private var reviewing: String?
    @State private var commenting = false
    @State private var text = ""

    private var open: Bool { detail?.state.lowercased() == "open" }

    var body: some View {
        List {
            if let d = detail {
                if d.loading { LoadingRow("Loading…") }
                Section {
                    Text(d.title).font(.headline)
                    Text("\(d.draft ? "Draft" : d.state.capitalized) · \(d.changedFiles) files · +\(d.additions) −\(d.deletions)").font(TypeRole.meta).foregroundStyle(.secondary)
                    if let url = URL(string: d.url) { Link("Open on GitHub", destination: url) }
                }
                Section {
                    Button { core.update(.startPullRequestThread(key: d.key)) } label: {
                        Label("Start a Thread on It", systemImage: "bubble.left.and.text.bubble.right")
                    }
                    if open, !d.draft {
                        Menu {
                            Button("Create a Merge Commit") { merging = "merge" }
                            Button("Squash and Merge") { merging = "squash" }
                            Button("Rebase and Merge") { merging = "rebase" }
                        } label: { Label("Merge…", systemImage: "arrow.triangle.merge") }
                    }
                    if open {
                        Menu {
                            Button("Approve", systemImage: "checkmark.circle") { reviewing = "approve" }
                            Button("Request Changes", systemImage: "exclamationmark.bubble") { reviewing = "request-changes" }
                            Button("Comment", systemImage: "text.bubble") { reviewing = "comment" }
                        } label: { Label("Review…", systemImage: "checkmark.seal") }
                    }
                    Button { commenting = true } label: { Label("Comment…", systemImage: "text.bubble") }
                    if open, d.draft {
                        Button { core.update(.pullRequestAction(key: d.key, action: "ready", mergeMethod: "")) } label: { Label("Ready for Review", systemImage: "eye") }
                    }
                    if open {
                        Button(role: .destructive) { closing = true } label: { Label("Close…", systemImage: "xmark.circle") }
                    } else if d.state.lowercased() == "closed" {
                        Button { core.update(.pullRequestAction(key: d.key, action: "reopen", mergeMethod: "")) } label: { Label("Reopen", systemImage: "arrow.uturn.backward.circle") }
                    }
                } footer: {
                    if busy { LoadingRow("Working on it…") }
                }
                .disabled(busy)
                if !d.checks.isEmpty {
                    Section("Checks") {
                        ForEach(Array(d.checks.enumerated()), id: \.offset) { _, check in
                            LabeledContent(check.name, value: check.status)
                        }
                    }
                }
                if !d.body.isEmpty {
                    Section("Description") { MarkdownBlocks(blocks: d.body) }
                }
            }
        }
        .navigationTitle(detail.map { $0.number > 0 ? "#\($0.number)" : "Pull Request" } ?? "Pull Request")
        .navigationBarTitleDisplayMode(.inline)
        .confirmationDialog(
            mergeTitle,
            isPresented: Binding(get: { merging != nil }, set: { if !$0 { merging = nil } }),
            titleVisibility: .visible,
            presenting: merging
        ) { method in
            Button(mergeButton(method)) {
                if let key = detail?.key { core.update(.pullRequestAction(key: key, action: "merge", mergeMethod: method)) }
            }
        } message: { _ in
            Text("This merges \(detail.map { "#\($0.number)" } ?? "the pull request")\(detail.map { $0.base.isEmpty ? "" : " into \($0.base)" } ?? "") on GitHub.")
        }
        // Others see a closed pull request (and get told): ask first.
        .confirmationDialog(
            "Close \(detail.map { "#\($0.number)" } ?? "this pull request") without merging?",
            isPresented: $closing,
            titleVisibility: .visible
        ) {
            Button("Close Pull Request", role: .destructive) {
                if let key = detail?.key { core.update(.pullRequestAction(key: key, action: "close", mergeMethod: "")) }
            }
        } message: {
            Text("It can be reopened later.")
        }
        .sheet(isPresented: Binding(get: { reviewing != nil || commenting }, set: { if !$0 { reviewing = nil; commenting = false } })) {
            NavigationStack {
                Form {
                    TextField(reviewing == "approve" ? "Summary (optional)" : (commenting ? "Comment" : "Summary"), text: $text, axis: .vertical)
                        .lineLimit(4...12)
                }
                .navigationTitle(sheetTitle)
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) { Button("Cancel") { closeSheet() } }
                    ToolbarItem(placement: .confirmationAction) {
                        Button(commenting ? "Post" : "Submit") {
                            guard let key = detail?.key else { return }
                            if commenting {
                                core.update(.commentOnPullRequest(key: key, body: text))
                            } else if let verdict = reviewing {
                                core.update(.reviewPullRequest(key: key, verdict: verdict, body: text))
                            }
                            closeSheet()
                        }
                        .disabled(reviewing != "approve" && text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    }
                }
            }
            .presentationDetents([.medium, .large])
        }
    }

    private var mergeTitle: String { "Merge #\(detail?.number ?? 0)?" }

    private func mergeButton(_ method: String) -> String {
        switch method {
        case "squash": "Squash and Merge"
        case "rebase": "Rebase and Merge"
        default: "Create a Merge Commit"
        }
    }

    private var sheetTitle: String {
        if commenting { return "Comment" }
        switch reviewing {
        case "approve": return "Approve"
        case "request-changes": return "Request Changes"
        default: return "Review"
        }
    }

    private func closeSheet() {
        reviewing = nil
        commenting = false
        text = ""
    }
}

struct UsageScreen: View {
    @ObservedObject var core: Core

    private var usage: UsageView { core.view.usage }

    var body: some View {
        List {
            // In a section, so it sits on the list's inset like the rows.
            Section {
                Picker("Range", selection: Binding(get: { usage.days == 0 ? 7 : usage.days }, set: { core.update(.loadUsage(days: $0)) })) {
                    ForEach(UsageSpan.days, id: \.0) { Text($0.1).tag($0.0) }
                }
                .pickerStyle(.segmented)
                if !usage.providers.isEmpty {
                    UsageProviderFilter(usage: usage) { core.update(.filterUsageProvider(provider: $0)) }
                }
            } footer: {
                if !usage.notice.isEmpty { Text(usage.notice) }
            }
            if usage.error != nil || !usage.sourceNotices.isEmpty {
                Section {
                    if let error = usage.error { ErrorRow(error) }
                    UsageSourceNotices(notices: usage.sourceNotices)
                }
            }
            // A row, not a full-screen state: the range and totals stay above it.
            if usage.loading && usage.rows.isEmpty {
                LoadingRow("Loading usage…")
            } else if usage.rows.isEmpty {
                Text("No usage yet. Turns run in this range show up here.").font(TypeRole.meta).foregroundStyle(.secondary)
            } else {
                // The cards draw their own surface, on the list's background.
                Section {
                    UsageKPIs(usage: usage)
                        .listRowInsets(EdgeInsets())
                        .listRowBackground(Color.clear)
                }
                if !usage.series.isEmpty {
                    Section { UsageActivityChart(usage: usage).padding(.vertical, Space.xs) }
                }
                if !usage.tokenMix.isEmpty {
                    Section { UsageTokenMix(slices: usage.tokenMix).padding(.vertical, Space.xs) }
                }
            }
            if !usage.byModel.isEmpty {
                Section("By model") {
                    ForEach(Array(usage.byModel.enumerated()), id: \.offset) { _, row in usageRow(row, byDay: false) }
                }
            }
            if !usage.rows.isEmpty {
                Section(usage.hourly ? "By hour" : "By day and model") {
                    ForEach(Array(usage.rows.enumerated()), id: \.offset) { _, row in usageRow(row, byDay: true) }
                }
            }
            if !usage.limits.isEmpty {
                Section("Limits") {
                    ProviderLimitsSection(limits: usage.limits).padding(.vertical, Space.xs)
                }
            }
            // Refreshing rows already there (the first load has its own spinner).
            if usage.loading, !usage.rows.isEmpty { LoadingRow("Refreshing…") }
        }
        .navigationTitle("Usage")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                RefreshLimitsButton(refreshing: usage.limitsRefreshing) { core.update(.refreshUsageLimits) }
            }
        }
        .onAppear { core.update(.loadUsage(days: usage.days == 0 ? 7 : usage.days)) }
    }

    private func usageRow(_ row: UsageRowView, byDay: Bool) -> some View {
        VStack(alignment: .leading, spacing: Space.xxs) {
            HStack {
                Circle().fill(Tint.provider(row.providerKey)).frame(width: Size.dot, height: Size.dot)
                Text(row.model).lineLimit(1)
                Spacer()
                Text(row.total).monospacedDigit()
                if !row.cost.isEmpty { Text(row.cost).monospacedDigit().foregroundStyle(.secondary) }
            }
            Text([byDay ? row.when(hourly: usage.hourly) : "", row.provider, "\(row.shareLabel) of tokens", "\(row.cacheHitText) cache hit", row.sessionsText].filter { !$0.isEmpty }.joined(separator: " · "))
                .font(TypeRole.metaSmall).foregroundStyle(.secondary)
            Text(row.breakdown).font(TypeRole.metaSmall).monospacedDigit().foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .combine)
    }
}

struct SettingsScreen: View {
    @ObservedObject var core: Core
    @EnvironmentObject private var companion: CompanionBrain
    @State private var adding = false
    @State private var renaming: EnvironmentView?
    @State private var newName = ""
    @State private var notifications: UNAuthorizationStatus?
    @State private var pickingProjectIcon: ProjectSettingsView?
    @State private var renamingProject: ProjectSettingsView?
    @State private var projectName = ""
    @State private var forgetting: EnvironmentView?
    @State private var removing: ProjectSettingsView?
    @Environment(\.scenePhase) private var scenePhase

    private var connection: ConnectionView { core.view.connection }
    private var remoteAccess: RemoteAccessView { core.view.settings.remote }

    private func readNotifications() async {
        notifications = await UNUserNotificationCenter.current().notificationSettings().authorizationStatus
    }

    private var notificationsLine: String {
        switch notifications {
        case .authorized, .provisional, .ephemeral: return "On"
        case .denied: return "Off"
        case .notDetermined: return "Not asked yet"
        default: return "…"
        }
    }

    var body: some View {
        Form {
            Section("Connected server") {
                LabeledContent("Name", value: connection.environmentLabel)
                LabeledContent("State", value: connection.state.capitalized)
                if connection.remote {
                    LabeledContent("Route", value: connection.path.isEmpty ? "—" : "\(connection.path.capitalized), \(connection.rttMs) ms")
                } else {
                    LabeledContent("Address", value: connection.serverUrl)
                }
                LabeledContent("Server", value: "domovoi-server \(connection.serverVersion)")
                LabeledContent("Platform", value: connection.platform)
                if !connection.activeEnvironmentId.isEmpty {
                    NavigationLink(value: Screen.companion(connection.activeEnvironmentId)) {
                        SettingsRow(title: "Companion") {
                            CompanionView(look: connection.companion.look, brain: companion, size: 26)
                        }
                    }
                }
            }
            Section {
                ForEach(connection.environments, id: \.id) { env in
                    Button { core.update(.selectEnvironment(id: env.id)) } label: { EnvironmentRow(env: env) }
                        .swipeActions {
                            Button("Forget", role: .destructive) { forgetting = env }
                            Button("Rename") {
                                newName = env.label
                                renaming = env
                            }
                            .tint(.blue)
                        }
                        .contextMenu {
                            Button("Rename…") {
                                newName = env.label
                                renaming = env
                            }
                            Button("Forget", role: .destructive) { forgetting = env }
                        }
                }
                .onMove { from, to in
                    // Indexes into the rows as drawn: the list may have changed since.
                    guard let index = from.first, connection.environments.indices.contains(index) else { return }
                    let id = connection.environments[index].id
                    // `to` counts the moved row itself when moving down.
                    let delta = to > index ? to - index - 1 : to - index
                    core.update(.moveEnvironment(id: id, delta: Int32(delta)))
                }
                Button { adding = true } label: { Label("Add Server", systemImage: "plus") }
            } header: {
                HStack {
                    Text("Servers")
                    Spacer()
                    if connection.environments.count > 1 { EditButton().font(TypeRole.meta) }
                }
            }
            if connection.state == "connected" {
                Section("This Server") {
                    NavigationLink(value: Screen.serverSettings) { Label("Server Settings", systemImage: "gearshape.2") }
                    NavigationLink(value: Screen.providers) { Label("Providers", systemImage: "cpu") }
                    NavigationLink(value: Screen.scheduledTasks) { Label("Scheduled Tasks", systemImage: "calendar.badge.clock") }
                    NavigationLink(value: Screen.access) { Label("Access & Devices", systemImage: "person.badge.key") }
                    NavigationLink(value: Screen.maintenance) { Label("Maintenance", systemImage: "wrench.and.screwdriver") }
                }
            }
            if connection.state == "connected" {
                Section {
                    ForEach(core.view.settings.projects, id: \.id) { project in
                        // A settings row's layout, with the icon a button and the
                        // path cut at its start.
                        HStack(spacing: Space.m) {
                            Button { pickingProjectIcon = project } label: {
                                ProjectIconTile(icon: project.icon, size: Size.listIcon, core: core)
                                    .tapTarget()
                            }
                            .buttonStyle(.borderless)
                            .disabled(project.icon.saving)
                            .accessibilityLabel("Change icon for \(project.title)")
                            .iconColumn(Size.listIcon)
                            VStack(alignment: .leading, spacing: Space.xxs) {
                                Text(project.title)
                                Text(project.workspaceRoot).font(TypeRole.meta).foregroundStyle(.secondary).lineLimit(1).truncationMode(.head)
                            }
                        }
                        .swipeActions {
                            Button("Remove", role: .destructive) { removing = project }
                            Button("Rename") {
                                projectName = project.title
                                renamingProject = project
                            }
                            .tint(.blue)
                        }
                        .contextMenu {
                            Button("Change Icon…") { pickingProjectIcon = project }.disabled(project.icon.saving)
                            Button("Rename…") {
                                projectName = project.title
                                renamingProject = project
                            }
                            Button("Remove", role: .destructive) { removing = project }
                        }
                    }
                    NavigationLink(value: Screen.importSessions) { Label("Import from Claude Code", systemImage: "square.and.arrow.down") }
                    NavigationLink(value: Screen.browseFolders) { Label("Add Folder on Server", systemImage: "folder.badge.plus") }
                } header: {
                    Text("Projects")
                } footer: {
                    Text("Removing a project keeps its folder. A removed project is not imported again on its own.")
                }
            }
            if connection.state == "connected", !core.view.settings.archived.isEmpty {
                Section("Archived Threads") {
                    ForEach(core.view.settings.archived, id: \.id) { thread in
                        HStack {
                            VStack(alignment: .leading, spacing: Space.xxs) {
                                Text(thread.title).lineLimit(2)
                                Text(thread.age).font(TypeRole.meta).foregroundStyle(.secondary)
                            }
                            Spacer()
                            Button("Unarchive") { core.update(.archiveThread(threadId: thread.id, archived: false)) }
                                .buttonStyle(.borderless)
                        }
                    }
                }
            }
            Section("Notifications") {
                LabeledContent("On this device", value: notificationsLine)
                if notifications == .notDetermined {
                    Button("Turn On Notifications") {
                        AppDelegate.requestPush()
                        Task {
                            // The system's prompt answers in its own time.
                            for _ in 0..<20 where notifications == .notDetermined {
                                try? await Task.sleep(for: .milliseconds(500))
                                await readNotifications()
                            }
                        }
                    }
                }
                if connection.state == "connected" {
                    LabeledContent("From this server", value: remoteAccess.pushConfigured ? "On" : "Off (no APNs key on the server)")
                }
                if notifications == .denied, let url = URL(string: UIApplication.openSettingsURLString) {
                    Link("Turn On in Settings", destination: url)
                }
            }
            Section {
                if connection.state == "connected" {
                    Button("Disconnect") { core.update(.disconnect) }
                } else {
                    Button("Reconnect") { core.update(.reconnect) }
                }
            } footer: {
                Text("Forgetting the server in use also signs this device out there and stops its notifications. Forgetting another one only removes it here.")
            }
        }
        .navigationTitle("Settings")
        .navigationBarTitleDisplayMode(.inline)
        .confirmationDialog(
            "Forget \(forgetting?.label ?? "this server")?",
            isPresented: Binding(get: { forgetting != nil }, set: { if !$0 { forgetting = nil } }),
            titleVisibility: .visible,
            presenting: forgetting
        ) { env in
            Button("Forget", role: .destructive) { core.update(.forgetEnvironment(id: env.id)) }
        } message: { env in
            Text(env.id == connection.activeEnvironmentId ? "This device signs out there and stops getting its notifications. Pairing again needs a new code." : "It's only removed from this device.")
        }
        .confirmationDialog(
            "Remove \(removing?.title ?? "this project")?",
            isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }),
            titleVisibility: .visible,
            presenting: removing
        ) { project in
            Button("Remove", role: .destructive) { core.update(.removeProject(projectId: project.id)) }
        } message: { _ in
            Text("Its folder stays on the server. Its threads go with it.")
        }
        .alert("Rename Project", isPresented: Binding(get: { renamingProject != nil }, set: { if !$0 { renamingProject = nil } })) {
            TextField("Name", text: $projectName)
            Button("Rename") {
                if let project = renamingProject { core.update(.renameProject(projectId: project.id, title: projectName)) }
            }
            Button("Cancel", role: .cancel) {}
        }
        .alert("Rename Server", isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })) {
            TextField("Name", text: $newName)
            Button("Rename") {
                if let env = renaming { core.update(.renameEnvironment(id: env.id, label: newName)) }
            }
            Button("Cancel", role: .cancel) {}
        }
        .task {
            await readNotifications()
            if connection.state == "connected" { core.update(.loadRemoteAccess) }
        }
        // Back from Settings, where it may have changed.
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { Task { await readNotifications() } }
        }
        .sheet(isPresented: Binding(get: { pickingProjectIcon != nil }, set: { if !$0 { pickingProjectIcon = nil } })) {
            if let project = pickingProjectIcon { ProjectIconPicker(project: project, core: core) }
        }
        .sheet(isPresented: $adding) {
            NavigationStack {
                AddServerView(core: core)
                    .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Cancel") { adding = false } } }
            }
        }
    }
}
