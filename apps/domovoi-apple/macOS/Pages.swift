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
                Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                TextField("Search threads, projects and actions", text: $query)
                    .textFieldStyle(.plain)
                    .font(.title3)
                    .focused($focused)
                    .onChange(of: query) { _, q in core.update(.setPaletteQuery(query: q)) }
                    .onSubmit { core.update(.runPaletteItem(id: "")) }
                    .onKeyPress(.upArrow) {
                        core.update(.movePalette(delta: -1))
                        return .handled
                    }
                    .onKeyPress(.downArrow) {
                        core.update(.movePalette(delta: 1))
                        return .handled
                    }
            }
            .padding(Space.m + Space.xxs)
            Divider()
            if palette.items.isEmpty {
                ContentUnavailableView.search(text: query)
                    .frame(maxHeight: .infinity)
            } else {
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: Space.xxs) {
                        ForEach(Array(palette.items.enumerated()), id: \.element.id) { index, item in
                            Button { core.update(.runPaletteItem(id: item.id)) } label: {
                                HStack(spacing: Space.s + Space.xxs) {
                                    Image(systemName: icon(item.kind)).frame(width: 18).foregroundStyle(.secondary)
                                    VStack(alignment: .leading, spacing: Space.xxs / 2) {
                                        Text(item.title).lineLimit(1)
                                        if !item.subtitle.isEmpty {
                                            Text(item.subtitle).font(.caption).foregroundStyle(.secondary).lineLimit(1)
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
        .onExitCommand { core.update(.closePalette) }
    }

    private func icon(_ kind: String) -> String {
        switch kind {
        case "thread": return "bubble.left"
        case "project": return "folder"
        default: return "bolt"
        }
    }
}

/// A shortcut such as "⌘⇧K" drawn as keys.
struct KeyCaps: View {
    let shortcut: String

    var body: some View {
        HStack(spacing: Space.xxs) {
            ForEach(Array(shortcut.enumerated()), id: \.offset) { _, key in
                Text(String(key))
                    .font(.caption.weight(.medium))
                    .foregroundStyle(.secondary)
                    .frame(minWidth: 18, minHeight: 18)
                    .background(.fill.tertiary, in: RoundedRectangle(cornerRadius: Space.xs, style: .continuous))
            }
        }
        .accessibilityElement()
        .accessibilityLabel(shortcut)
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
                                Text(pr.state.capitalized)
                                Text("+\(pr.additions) −\(pr.deletions) in ^[\(Int(pr.changedFiles)) file](inflect: true)").foregroundStyle(.secondary)
                                Spacer()
                                if let url = URL(string: pr.url) { Link("Open on the web", destination: url) }
                            }
                            .font(.callout)
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

    /// The chosen range in days; the view model only has its label.
    @State private var days = 7

    private var usage: UsageView { core.view.usage }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.m) {
            HStack {
                Picker("Range", selection: Binding(get: { days }, set: {
                    days = $0
                    core.update(.loadUsage(days: UInt32($0)))
                })) {
                    Text("Today").tag(1)
                    Text("7 days").tag(7)
                    Text("30 days").tag(30)
                }
                .pickerStyle(.segmented)
                .frame(maxWidth: 280)
                Spacer()
                Text("\(usage.totalTokens) tokens").font(.title3.monospacedDigit().bold())
            }
            if !usage.rangeLabel.isEmpty { Text(usage.rangeLabel).foregroundStyle(.secondary) }
            if usage.loading {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                Table(usage.rows) {
                    TableColumn("Day", value: \.day)
                    TableColumn("Provider", value: \.provider)
                    TableColumn("Model", value: \.model)
                    TableColumn("Input", value: \.input)
                    TableColumn("Cached", value: \.cached)
                    TableColumn("Output", value: \.output)
                    TableColumn("Total", value: \.total)
                }
            }
            if !usage.notice.isEmpty { Text(usage.notice).font(.caption).foregroundStyle(.secondary) }
        }
        .padding(Space.xl)
        .navigationTitle("Usage")
    }
}

extension UsageRowView: @retroactive Identifiable {
    public var id: String { "\(day)/\(provider)/\(model)" }
}
