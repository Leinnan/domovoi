import App
import SwiftUI

// MARK: - Command palette

struct PaletteSheet: View {
    @ObservedObject var core: Core
    let palette: PaletteView
    @State private var query = ""
    @FocusState private var focused: Bool

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: Space.s) {
                if palette.searchesFiles {
                    // Back to the command list (Esc too).
                    Button { core.update(.setPaletteMode(mode: "commands")) } label: {
                        Label(palette.modeTitle, systemImage: "chevron.left")
                            .font(.callout.weight(.medium))
                            .padding(.horizontal, Space.s)
                            .padding(.vertical, Space.xxs)
                            .background(.fill.tertiary, in: Capsule())
                    }
                    .buttonStyle(.plain)
                    .help("Back to commands (esc)")
                } else {
                    Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                }
                TextField(palette.prompt, text: $query)
                    .textFieldStyle(.plain)
                    .font(.title3)
                    .focused($focused)
                    .onChange(of: query) { _, q in if q != palette.query { core.update(.setPaletteQuery(query: q)) } }
                    .onSubmit { core.update(.runPaletteItem(id: "")) }
                    .onKeyPress(.upArrow) {
                        core.update(.movePalette(delta: -1))
                        return .handled
                    }
                    .onKeyPress(.downArrow) {
                        core.update(.movePalette(delta: 1))
                        return .handled
                    }
                if palette.searching { ProgressView().controlSize(.small) }
            }
            .padding(Space.m + Space.xxs)
            Divider()
            if palette.items.isEmpty {
                Group {
                    if palette.searching {
                        ProgressView()
                    } else if palette.searchesFiles, query.isEmpty {
                        ContentUnavailableView(palette.prompt, systemImage: palette.mode == "files" ? "doc" : "text.magnifyingglass")
                    } else {
                        ContentUnavailableView.search(text: query)
                    }
                }
                .frame(maxHeight: .infinity)
            } else {
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: Space.xxs) {
                        ForEach(Array(palette.items.enumerated()), id: \.element.id) { index, item in
                            Button { core.update(.runPaletteItem(id: item.id)) } label: {
                                HStack(spacing: Space.s + Space.xxs) {
                                    Image(systemName: item.symbol).frame(width: 18).foregroundStyle(.secondary)
                                    VStack(alignment: .leading, spacing: Space.xxs / 2) {
                                        Text(item.title).lineLimit(1)
                                            .font(item.kind == "match" ? TypeRole.mono : .body)
                                        if !item.subtitle.isEmpty {
                                            Text(item.subtitle).font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.head)
                                        }
                                    }
                                    Spacer()
                                    if !item.shortcut.isEmpty {
                                        KeyCaps(shortcut: item.shortcut)
                                    }
                                }
                                .padding(.horizontal, Space.m)
                                .padding(.vertical, Space.s)
                                .contentShape(Rectangle())
                                .hoverHighlight(Radius.chip, active: index == Int(palette.highlighted))
                            }
                            .buttonStyle(.plain)
                            .id(index)
                        }
                    }
                }
                .onChange(of: palette.highlighted) { _, h in proxy.scrollTo(Int(h)) }
            }
            }
        }
        .frame(width: 600, height: 420)
        .onAppear {
            query = palette.query
            focused = true
        }
        // A new mode starts with an empty field (the core cleared its query).
        .onChange(of: palette.mode) { _, _ in
            query = palette.query
            focused = true
        }
        .onExitCommand {
            if palette.searchesFiles { core.update(.setPaletteMode(mode: "commands")) } else { core.update(.closePalette) }
        }
    }
}

// MARK: - Pull requests

struct PullRequestsPage: View {
    @ObservedObject var core: Core

    private var prs: PullRequestsView { core.view.pullRequests }

    var body: some View {
        HSplitView {
            VStack(spacing: 0) {
                if let notice = prs.notice {
                    Label(notice, systemImage: "info.circle").font(.caption).padding(Space.s).frame(maxWidth: .infinity, alignment: .leading)
                }
                if prs.loading {
                    ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
                } else if let error = prs.error {
                    ContentUnavailableView("Could not load pull requests", systemImage: "exclamationmark.triangle", description: Text(error))
                } else if prs.items.isEmpty {
                    ContentUnavailableView("No open pull requests", systemImage: "arrow.triangle.pull", description: Text("Open pull requests of your projects' GitHub repositories show here (needs the gh CLI on the server)."))
                } else {
                    List(selection: Binding(get: { prs.selected?.key }, set: { if let k = $0 { core.update(.selectPullRequest(key: k)) } })) {
                        ForEach(prs.items, id: \.key) { pr in
                            VStack(alignment: .leading, spacing: Space.xxs) {
                                HStack {
                                    Text("#\(pr.number)").foregroundStyle(.secondary).monospacedDigit()
                                    Text(pr.title).lineLimit(1)
                                    if pr.draft { Text("Draft").font(.caption2).padding(.horizontal, Space.xs).background(.quaternary, in: Capsule()) }
                                }
                                HStack(spacing: Space.tight) {
                                    Text(pr.repository)
                                    if !pr.author.isEmpty {
                                        Text("·")
                                        Text(pr.author).lineLimit(1)
                                    }
                                    Text("·")
                                    Text("\(pr.head) → \(pr.base)").lineLimit(1)
                                    Spacer()
                                    ChecksBadge(state: pr.checks)
                                    Text(pr.age)
                                }
                                .font(.caption)
                                .foregroundStyle(.secondary)
                            }
                            .tag(pr.key)
                        }
                    }
                }
            }
            .frame(minWidth: 360)
            if let pr = prs.selected {
                ScrollView {
                    VStack(alignment: .leading, spacing: Space.m) {
                        if pr.loading {
                            ProgressView()
                        } else {
                            Text(pr.title).font(.title2.bold())
                            HStack {
                                Text(pr.draft ? "Draft" : pr.state.capitalized)
                                Text("+\(pr.additions) −\(pr.deletions) in ^[\(Int(pr.changedFiles)) file](inflect: true)").foregroundStyle(.secondary)
                                Spacer()
                                if let url = URL(string: pr.url) { Link("Open on the web", destination: url) }
                            }
                            .font(.callout)
                            PullRequestActions(core: core, detail: pr, busy: prs.busy)
                            GroupBox("AI Code Review") {
                                AiCodeReviewBody(core: core, detail: pr, busy: prs.busy)
                                    .padding(Space.xs)
                            }
                            if !pr.checks.isEmpty {
                                GroupBox("Checks") {
                                    VStack(alignment: .leading, spacing: Space.xs) {
                                        ForEach(pr.checks, id: \.name) { check in
                                            HStack {
                                                ChecksBadge(state: check.status)
                                                Text(check.name)
                                                Spacer()
                                                if let url = URL(string: check.url), !check.url.isEmpty { Link("Details", destination: url).font(.caption) }
                                            }
                                        }
                                    }
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                }
                            }
                            MarkdownBlocks(blocks: pr.body)
                        }
                    }
                    .padding(Space.xl)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(minWidth: 360)
            } else if !prs.items.isEmpty {
                ContentUnavailableView("Select a Pull Request", systemImage: "arrow.triangle.pull", description: Text("Its checks, description and changes show here."))
                    .frame(minWidth: 360, maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .navigationTitle("Pull Requests")
        .toolbar {
            Button { core.update(.loadPullRequests) } label: { Label("Refresh", systemImage: "arrow.clockwise") }
        }
    }
}

/// What can be done to the selected pull request: start a thread on it,
/// merge, review, comment, and change its state.
struct PullRequestActions: View {
    @ObservedObject var core: Core
    let detail: PullRequestDetailView
    let busy: Bool
    @State private var merging: String?
    @State private var closing = false
    /// The review or comment being written.
    @State private var writing: PullRequestNote?

    private var open: Bool { detail.state.lowercased() == "open" }
    private var closed: Bool { detail.state.lowercased() == "closed" }

    var body: some View {
        // A narrow column keeps the titles of the first button only.
        ViewThatFits(in: .horizontal) {
            row(compact: false)
            row(compact: true)
        }
        .disabled(busy)
        .confirmationDialog(
            "Merge #\(detail.number)?",
            isPresented: Binding(get: { merging != nil }, set: { if !$0 { merging = nil } }),
            titleVisibility: .visible,
            presenting: merging
        ) { method in
            Button(mergeButton(method)) { core.update(.pullRequestAction(key: detail.key, action: "merge", mergeMethod: method)) }
        } message: { _ in
            Text("This merges #\(detail.number)\(detail.base.isEmpty ? "" : " into \(detail.base)") on GitHub.")
        }
        // Others see a closed pull request (and get told): ask first.
        .confirmationDialog("Close #\(detail.number) without merging?", isPresented: $closing, titleVisibility: .visible) {
            Button("Close Pull Request", role: .destructive) { act("close") }
        } message: {
            Text("It can be reopened later.")
        }
        .sheet(item: $writing) { note in
            PullRequestNoteSheet(note: note, number: detail.number) { text in
                if let verdict = note.verdict {
                    core.update(.reviewPullRequest(key: detail.key, verdict: verdict, body: text))
                } else {
                    core.update(.commentOnPullRequest(key: detail.key, body: text))
                }
            }
        }
    }

    private func row(compact: Bool) -> some View {
        HStack(spacing: Space.s) {
            Button { core.update(.startPullRequestThread(key: detail.key)) } label: {
                Label("Start a Thread on It", systemImage: "bubble.left.and.text.bubble.right")
            }
            .buttonStyle(.borderedProminent)
            .labelStyle(.titleAndIcon)
            .help("Check the pull request out in a worktree and start a thread there")
            if open, !detail.draft {
                Menu {
                    Button("Create a Merge Commit…") { merging = "merge" }
                    Button("Squash and Merge…") { merging = "squash" }
                    Button("Rebase and Merge…") { merging = "rebase" }
                } label: { Label("Merge", systemImage: "arrow.triangle.merge") }
                .fixedSize()
                .help("Merge on GitHub")
            }
            if open {
                Menu {
                    Button("Approve…") { writing = PullRequestNote(verdict: "approve") }
                    Button("Request Changes…") { writing = PullRequestNote(verdict: "request-changes") }
                    Button("Comment…") { writing = PullRequestNote(verdict: "comment") }
                } label: { Label("Review", systemImage: "checkmark.seal") }
                .fixedSize()
                .help("Approve, request changes or comment as a review")
            }
            Button { writing = PullRequestNote(verdict: nil) } label: { Label("Comment…", systemImage: "text.bubble") }
                .help("Comment on the pull request")
            if open || closed {
                Menu {
                    if open, detail.draft {
                        Button("Ready for Review") { act("ready") }
                    } else if open {
                        Button("Convert to Draft") { act("draft") }
                    }
                    if open {
                        Divider()
                        Button("Close Pull Request…", role: .destructive) { closing = true }
                    } else {
                        Button("Reopen") { act("reopen") }
                    }
                } label: { Label("More", systemImage: "ellipsis.circle") }
                .menuIndicator(.hidden)
                .fixedSize()
                .help("More actions")
            }
            if busy { ProgressView().controlSize(.small) }
        }
        .labelStyle(CompactLabelStyle(compact: compact))
        .fixedSize()
    }

    private func act(_ action: String) {
        core.update(.pullRequestAction(key: detail.key, action: action, mergeMethod: ""))
    }

    private func mergeButton(_ method: String) -> String {
        switch method {
        case "squash": "Squash and Merge"
        case "rebase": "Rebase and Merge"
        default: "Create a Merge Commit"
        }
    }
}

private struct CompactLabelStyle: LabelStyle {
    let compact: Bool

    func makeBody(configuration: Configuration) -> some View {
        if compact {
            configuration.icon
        } else {
            HStack(spacing: Space.xs) {
                configuration.icon
                configuration.title
            }
        }
    }
}

/// A review (`verdict` set) or a plain comment, being written.
struct PullRequestNote: Identifiable {
    let id = UUID()
    let verdict: String?
}

private struct PullRequestNoteSheet: View {
    let note: PullRequestNote
    let number: UInt32
    let submit: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var text = ""
    @FocusState private var focused: Bool

    /// Only an approval may go without a summary.
    private var canSubmit: Bool {
        note.verdict == "approve" || !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.m) {
            Text("\(title) #\(number)").font(.headline)
            TextEditor(text: $text)
                .font(.body)
                .focused($focused)
                .scrollContentBackground(.hidden)
                .padding(Space.xs)
                .background(.fill.quaternary, in: RoundedRectangle(cornerRadius: Radius.chip, style: .continuous))
                .frame(minHeight: 160)
                .overlay(alignment: .topLeading) {
                    if text.isEmpty {
                        Text(note.verdict == "approve" ? "Summary (optional)" : (note.verdict == nil ? "Comment" : "Summary"))
                            .foregroundStyle(.tertiary)
                            .padding(Space.xs + 5)
                            .allowsHitTesting(false)
                    }
                }
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                // ⌘↩: a plain ↩ is a new line in the text.
                Button(note.verdict == nil ? "Post" : "Submit") {
                    submit(text)
                    dismiss()
                }
                .keyboardShortcut(.return, modifiers: .command)
                .buttonStyle(.borderedProminent)
                .disabled(!canSubmit)
                .help("⌘↩")
            }
        }
        .padding(Space.l)
        .frame(width: 480)
        .onAppear { focused = true }
    }

    private var title: String {
        switch note.verdict {
        case "approve": "Approve"
        case "request-changes": "Request Changes on"
        case "comment": "Review"
        default: "Comment on"
        }
    }
}

struct ChecksBadge: View {
    let state: String

    var body: some View {
        switch state {
        case "passing", "success": Image(systemName: "checkmark.circle.fill").foregroundStyle(Tint.success)
        case "failing", "failure": Image(systemName: "xmark.circle.fill").foregroundStyle(Tint.danger)
        case "pending", "action-required": Image(systemName: "clock.fill").foregroundStyle(Tint.caution)
        case "": EmptyView()
        default: Image(systemName: "minus.circle").foregroundStyle(.secondary)
        }
    }
}

// MARK: - Usage

struct UsagePage: View {
    @ObservedObject var core: Core

    private var usage: UsageView { core.view.usage }
    /// Prices known: the tables get a cost column.
    private var priced: Bool { !usage.totalCost.isEmpty || usage.rows.contains { !$0.cost.isEmpty } }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Space.l) {
                Picker("Range", selection: Binding(get: { usage.days == 0 ? 7 : usage.days }, set: { core.update(.loadUsage(days: $0)) })) {
                    ForEach(UsageSpan.days, id: \.0) { Text($0.1).tag($0.0) }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .frame(maxWidth: 360)
                if let error = usage.error { ErrorRow(error) }
                UsageSourceNotices(notices: usage.sourceNotices)
                if !usage.providers.isEmpty {
                    UsageProviderFilter(usage: usage) { core.update(.filterUsageProvider(provider: $0)) }
                }
                if usage.loading, usage.rows.isEmpty {
                    ProgressView().frame(maxWidth: .infinity, minHeight: 200)
                } else if usage.rows.isEmpty {
                    ContentUnavailableView("No usage yet", systemImage: "chart.bar", description: Text("Turns run in this range show up here."))
                } else {
                    UsageKPIs(usage: usage)
                    if !usage.series.isEmpty {
                        GroupBox { UsageActivityChart(usage: usage).padding(Space.s) }
                    }
                    if !usage.tokenMix.isEmpty {
                        GroupBox { UsageTokenMix(slices: usage.tokenMix).padding(Space.s) }
                    }
                    if !usage.byModel.isEmpty {
                        GroupBox("By model") { table(usage.byModel, byDay: false).frame(height: tableHeight(usage.byModel.count)) }
                    }
                    GroupBox(usage.hourly ? "By hour" : "By day") { table(usage.rows, byDay: true).frame(height: tableHeight(usage.rows.count, max: 360)) }
                }
                if !usage.limits.isEmpty {
                    GroupBox("Limits") {
                        ProviderLimitsSection(limits: usage.limits)
                            .padding(Space.s)
                            .frame(maxWidth: 520, alignment: .leading)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
                if !usage.notice.isEmpty { Text(usage.notice).font(.caption).foregroundStyle(.secondary) }
            }
            .padding(Space.xl)
            .frame(maxWidth: 1100, alignment: .leading)
        }
        .navigationTitle("Usage")
        .toolbar {
            if usage.loading, !usage.rows.isEmpty { ToolbarItem { ProgressView().controlSize(.small) } }
            ToolbarItem {
                RefreshLimitsButton(refreshing: usage.limitsRefreshing) { core.update(.refreshUsageLimits) }
            }
        }
    }

    /// Rows fit without the table scrolling inside the page, up to `max`.
    private func tableHeight(_ rows: Int, max: CGFloat = 280) -> CGFloat {
        min(CGFloat(rows + 1) * 24 + 8, max)
    }

    private func table(_ rows: [UsageRowView], byDay: Bool) -> some View {
        Table(rows) {
            Group {
                if byDay { TableColumn(usage.hourly ? "Hour" : "When") { (row: UsageRowView) in Text(row.when(hourly: usage.hourly)).monospacedDigit() } }
                TableColumn("Provider") { (row: UsageRowView) in
                    HStack(spacing: Space.xs) {
                        Circle().fill(Tint.provider(row.providerKey)).frame(width: Size.dot, height: Size.dot)
                        Text(row.provider)
                    }
                }
                TableColumn("Model") { (row: UsageRowView) in Text(row.model) }
                TableColumn("Input") { (row: UsageRowView) in Text(row.input).monospacedDigit() }
                TableColumn("Cached") { (row: UsageRowView) in Text(row.cached).monospacedDigit() }
                TableColumn("Output") { (row: UsageRowView) in Text(row.output).monospacedDigit() }
            }
            Group {
                // Every kind of token, cache writes and reasoning too, on the total.
                TableColumn("Total") { (row: UsageRowView) in Text(row.total).monospacedDigit().help(row.breakdown) }
                TableColumn("Share") { (row: UsageRowView) in Text(row.shareLabel).monospacedDigit() }
                TableColumn("Cache hit") { (row: UsageRowView) in Text(row.cacheHitText).monospacedDigit() }
                TableColumn("Sessions") { (row: UsageRowView) in Text("\(row.sessions)").monospacedDigit() }
                if priced { TableColumn("Cost") { (row: UsageRowView) in Text(row.cost).monospacedDigit() } }
            }
        }
    }
}

extension UsageRowView: @retroactive Identifiable {
    public var id: String { "\(day)/\(hourLabel)/\(providerKey)/\(model)" }
}
