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
                HStack { ProgressView(); Text("Loading changes…").foregroundStyle(.secondary) }
            } else if let error = diff.error {
                Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(.red)
            } else if diff.files.isEmpty {
                ContentUnavailableView("No changes", systemImage: "checkmark.circle")
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
                    HStack {
                        Text(file.path).font(.footnote.monospaced()).lineLimit(1).truncationMode(.head)
                        Spacer()
                        Text("+\(file.additions)").foregroundStyle(.green).font(.caption)
                        Text("−\(file.deletions)").foregroundStyle(.red).font(.caption)
                    }
                }
            }
            if diff.truncated {
                Text("Large diff: some files are not shown.").font(.caption).foregroundStyle(.secondary)
            }
        }
        .navigationTitle("Changes")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { core.update(.showPanel(tab: "diff")) }
        .refreshable { await core.refresh(.showPanel(tab: "diff")) { core.view.panel.diff.loading } }
    }
}

struct FilesScreen: View {
    @ObservedObject var core: Core
    @State private var query = ""
    @State private var previewing = false

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
                        previewing = true
                    }
                } label: {
                    HStack(spacing: 6) {
                        if entry.directory {
                            Image(systemName: entry.expanded ? "chevron.down" : "chevron.right").font(.caption2).foregroundStyle(.secondary).frame(width: 10)
                        }
                        Image(systemName: entry.directory ? "folder.fill" : "doc").foregroundStyle(entry.directory ? Color.accentColor : .secondary)
                        Text(query.isEmpty ? entry.name : entry.path)
                            .foregroundStyle(entry.ignored ? .secondary : .primary)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    .padding(.leading, query.isEmpty ? CGFloat(entry.depth) * 14 : 0)
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
            if files.loading {
                ProgressView()
            } else if entries.isEmpty {
                if query.isEmpty {
                    ContentUnavailableView("No files", systemImage: "folder", description: Text(core.view.connection.state == "connected" ? "This folder is empty." : "Not connected to the server."))
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
        .onAppear { core.update(.showPanel(tab: "files")) }
        .sheet(isPresented: $previewing) {
            NavigationStack {
                FilePreviewScreen(preview: files.preview) { path in
                    core.update(.appendToDraft(text: "@\(path) "))
                    previewing = false
                }
                .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { previewing = false } } }
            }
        }
    }
}

struct FilePreviewScreen: View {
    let preview: FilePreviewView?
    /// Mentions the file in the open thread's message, when there is one.
    var addToMessage: ((String) -> Void)?
    /// Long lines wrap instead of scrolling sideways.
    @AppStorage("filePreviewWraps") private var wraps = false

    var body: some View {
        Group {
            if let preview {
                if preview.loading {
                    ProgressView()
                } else if let error = preview.error {
                    ContentUnavailableView(error, systemImage: "doc.questionmark")
                } else {
                    ScrollView(wraps ? [.vertical] : [.vertical, .horizontal]) {
                        Text(CodeHighlight.text(preview.spans, fallback: preview.text) + AttributedString(preview.truncated ? "\n…" : ""))
                            .font(TypeRole.code)
                            .textSelection(.enabled)
                            .padding()
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .defaultScrollAnchor(.topLeading)
                }
            }
        }
        .navigationTitle((preview?.path as NSString?)?.lastPathComponent ?? "File")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            if let preview, preview.error == nil {
                ToolbarItem(placement: .topBarLeading) {
                    Menu {
                        Toggle("Wrap Lines", systemImage: "text.word.spacing", isOn: $wraps)
                        Button("Copy Path", systemImage: "doc.on.doc") { Platform.copy(preview.path) }
                        Button("Copy Text", systemImage: "doc.on.clipboard") { Platform.copy(preview.text) }
                        ShareLink(item: preview.text, subject: Text((preview.path as NSString).lastPathComponent)) {
                            Label("Share", systemImage: "square.and.arrow.up")
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
            if git.loading {
                HStack(spacing: 10) {
                    ProgressView()
                    Text("Reading the repository…").foregroundStyle(.secondary)
                }
            } else if !git.isRepo {
                ContentUnavailableView("Not a git repository", systemImage: "arrow.triangle.branch")
            } else {
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
                            HStack(spacing: 4) {
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
                        HStack {
                            Text(file.path).font(.footnote.monospaced()).lineLimit(1).truncationMode(.head)
                            Spacer()
                            Text("+\(file.insertions)").foregroundStyle(Tint.success).font(.caption)
                            Text("−\(file.deletions)").foregroundStyle(Tint.danger).font(.caption)
                        }
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
                if git.busy || !git.progress.isEmpty || git.error != nil {
                    Section {
                        if git.busy { ProgressView() }
                        ForEach(Array(git.progress.suffix(6).enumerated()), id: \.offset) { _, line in
                            Text(line).font(.caption.monospaced()).foregroundStyle(.secondary)
                        }
                        if let error = git.error {
                            Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(Tint.danger)
                        }
                    }
                }
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
            if let error = prs.error { Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(.red) }
            ForEach(prs.items, id: \.key) { pr in
                Button {
                    core.update(.selectPullRequest(key: pr.key))
                    showing = true
                } label: {
                    VStack(alignment: .leading, spacing: 3) {
                        Text(pr.title).foregroundStyle(.primary).lineLimit(2)
                        Text("\(pr.repository) #\(pr.number) · \(pr.author) · \(pr.age)").font(.caption).foregroundStyle(.secondary)
                        HStack(spacing: 6) {
                            Text(pr.draft ? "Draft" : pr.state.capitalized).font(.caption2.bold())
                            if !pr.checks.isEmpty { Text("Checks: \(pr.checks)").font(.caption2).foregroundStyle(.secondary) }
                        }
                    }
                }
            }
            if prs.loading { ProgressView() }
            if !prs.loading, prs.items.isEmpty, prs.error == nil {
                ContentUnavailableView("No pull requests", systemImage: "arrow.triangle.pull")
            }
        }
        .navigationTitle("Pull Requests")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { core.update(.loadPullRequests) }
        .refreshable { await core.refresh(.loadPullRequests) { core.view.pullRequests.loading } }
        .sheet(isPresented: $showing) {
            NavigationStack {
                PullRequestDetailScreen(core: core, detail: prs.selected, busy: prs.busy)
                    .toolbar { Button("Done") { showing = false } }
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
                if d.loading { ProgressView() }
                Section {
                    Text(d.title).font(.headline)
                    Text("\(d.draft ? "Draft" : d.state.capitalized) · \(d.changedFiles) files · +\(d.additions) −\(d.deletions)").font(.caption).foregroundStyle(.secondary)
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
                    if busy { HStack(spacing: Space.s) { ProgressView(); Text("Working on it…") } }
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
    @State private var days: UInt32 = 7

    private var usage: UsageView { core.view.usage }

    var body: some View {
        List {
            Picker("Range", selection: $days) {
                Text("Today").tag(UInt32(1))
                Text("7 days").tag(UInt32(7))
                Text("30 days").tag(UInt32(30))
            }
            .pickerStyle(.segmented)
            Section {
                LabeledContent("Total tokens", value: usage.totalTokens)
                if !usage.rangeLabel.isEmpty { LabeledContent("Range", value: usage.rangeLabel) }
            } footer: {
                if !usage.notice.isEmpty { Text(usage.notice) }
            }
            if usage.loading && usage.rows.isEmpty {
                ProgressView().frame(maxWidth: .infinity)
            } else if usage.rows.isEmpty {
                ContentUnavailableView("No usage yet", systemImage: "chart.bar", description: Text("Turns run in this range show up here."))
            }
            Section("By day and model") {
                ForEach(Array(usage.rows.enumerated()), id: \.offset) { _, row in
                    VStack(alignment: .leading, spacing: 2) {
                        HStack {
                            Text(row.model).lineLimit(1)
                            Spacer()
                            Text(row.total).monospacedDigit()
                        }
                        Text("\(row.day) · \(row.provider) · in \(row.input) · cached \(row.cached) · out \(row.output)")
                            .font(.caption2).foregroundStyle(.secondary)
                    }
                }
            }
            // Refreshing rows already there (the first load has its own spinner).
            if usage.loading, !usage.rows.isEmpty { ProgressView() }
        }
        .navigationTitle("Usage")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { core.update(.loadUsage(days: days)) }
        .onChange(of: days) { _, d in core.update(.loadUsage(days: d)) }
    }
}

struct SettingsScreen: View {
    @ObservedObject var core: Core
    @State private var adding = false
    @State private var renaming: EnvironmentView?
    @State private var newName = ""
    @State private var notifications: UNAuthorizationStatus?
    @State private var renamingProject: SidebarProjectView?
    @State private var projectName = ""
    @State private var forgetting: EnvironmentView?
    @State private var removing: SidebarProjectView?
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
                    if connection.environments.count > 1 { EditButton().font(.caption) }
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
                    ForEach(core.view.sidebar.projects, id: \.id) { project in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(project.title)
                            Text(project.workspaceRoot).font(.caption2).foregroundStyle(.secondary).lineLimit(1).truncationMode(.head)
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
                            VStack(alignment: .leading, spacing: 2) {
                                Text(thread.title).lineLimit(2)
                                Text(thread.age).font(.caption2).foregroundStyle(.secondary)
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
        .sheet(isPresented: $adding) {
            NavigationStack {
                AddServerView(core: core)
                    .toolbar { Button("Cancel") { adding = false } }
            }
        }
    }
}
