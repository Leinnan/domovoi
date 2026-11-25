import App
import Charts
import SwiftUI

/// How full something is, as a bar; `tick` marks a point on it (0: none).
struct PercentBar: View {
    let percent: UInt32
    var tick: UInt32 = 0
    var tint: Color = .accentColor

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule().fill(.quaternary)
                Capsule().fill(tint).frame(width: geo.size.width * CGFloat(min(percent, 100)) / 100)
                if tick > 0, tick < 100 {
                    Rectangle()
                        .fill(.primary.opacity(0.7))
                        .frame(width: 2, height: geo.size.height + 4)
                        .offset(x: geo.size.width * CGFloat(tick) / 100 - 1)
                }
            }
        }
        .frame(height: 6)
        .accessibilityHidden(true)
    }
}

extension ContextUsageView {
    /// Red all but full, orange nearly full, the accent from half.
    var tint: Color {
        critical ? Tint.danger : high ? Tint.caution : percent >= 50 ? Color.accentColor : .secondary
    }
}

extension LimitWindowView {
    var tint: Color { percent >= 90 ? Tint.danger : high ? Tint.caution : Color.accentColor }
}

/// The context window: how full, where it compacts, the counts behind it,
/// the cost, and compacting now (no padding of its own).
struct ContextSection: View {
    let context: ContextUsageView
    var compact: (() -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: Space.m) {
            HStack(alignment: .firstTextBaseline) {
                Label("Context", systemImage: "gauge.with.dots.needle.50percent").font(.headline)
                Spacer()
                if context.maxTokens > 0 {
                    Text("\(context.percent)%").font(.headline.monospacedDigit()).foregroundStyle(context.tint)
                }
            }
            Text([context.label, context.remainingLabel].filter { !$0.isEmpty }.joined(separator: " · "))
                .font(.callout).foregroundStyle(.secondary)
            if context.maxTokens > 0 {
                PercentBar(percent: context.percent, tick: context.compactAtPercent, tint: context.tint)
                if context.compactAtPercent > 0 {
                    Text("Compacts at \(context.compactAtPercent)%").font(TypeRole.meta).foregroundStyle(.secondary)
                }
            }
            if context.critical {
                Label("Nearly full: the agent will compact the conversation soon.", systemImage: "exclamationmark.triangle.fill")
                    .font(TypeRole.meta)
                    .foregroundStyle(Tint.danger)
                    .fixedSize(horizontal: false, vertical: true)
            } else if context.high {
                Label("Getting full: the agent will compact the conversation soon.", systemImage: "exclamationmark.triangle")
                    .font(TypeRole.meta)
                    .foregroundStyle(Tint.caution)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if !context.breakdown.isEmpty || !context.cost.isEmpty || !context.cacheHitLabel.isEmpty {
                Divider()
                VStack(spacing: Space.xs) {
                    ForEach(context.breakdown, id: \.label) { line in
                        LabeledContent(line.label) { Text(line.value).monospacedDigit() }
                    }
                    if !context.cacheHitLabel.isEmpty {
                        LabeledContent("Cache hit") { Text(context.cacheHitLabel).monospacedDigit() }
                            .help("\(context.cacheHitPercent)% of the input came from the prompt cache")
                    }
                    if !context.cost.isEmpty {
                        LabeledContent("Cost so far") { Text(context.cost).monospacedDigit() }
                    }
                }
                .font(.callout)
            }
            if context.canCompact, let compact {
                Button(action: compact) {
                    Label("Compact Now", systemImage: "arrow.down.right.and.arrow.up.left")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                .help("Have the agent summarize the conversation to free the context window")
            }
        }
    }
}

/// The open thread's usage in one place: its context window, then the
/// limits of the provider it runs on, and the way to the Usage page.
struct SessionUsageDetails: View {
    let usage: SessionUsageView
    var compact: (() -> Void)?
    var showAll: (() -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: Space.m) {
            if let context = usage.context {
                ContextSection(context: context, compact: compact)
            }
            if let limits = usage.limits {
                if usage.context != nil { Divider() }
                HStack(alignment: .firstTextBaseline) {
                    Label("\(limits.provider) Limits", systemImage: "gauge.with.dots.needle.67percent").font(.headline)
                    Spacer()
                    if !usage.limitsChecked.isEmpty {
                        Text(usage.limitsChecked == "now" ? "Updated now" : "Updated \(usage.limitsChecked) ago")
                            .font(TypeRole.meta)
                            .foregroundStyle(.secondary)
                    }
                }
                if !usage.limitsNotice.isEmpty {
                    Text(usage.limitsNotice).font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }
                if !limits.windows.isEmpty {
                    LimitWindowsList(windows: limits.windows)
                }
                if !limits.externalUrl.isEmpty, let url = URL(string: limits.externalUrl) {
                    Link(limits.externalLabel.isEmpty ? "Usage on the web" : limits.externalLabel, destination: url).font(.callout)
                }
            }
            if let showAll {
                Divider()
                Button(action: showAll) {
                    Label("All Usage", systemImage: "chart.bar").frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderless)
                .help("Every provider's limits and the tokens used over time")
            }
        }
        .padding(Space.l)
        .frame(width: 320)
    }
}

extension LimitWindowView {
    /// `Ahead of pace`, `On pace`, `Under pace`; empty when not known.
    var paceLabel: String {
        switch pace {
        case "ahead": "Ahead of pace"
        case "on": "On pace"
        case "under": "Under pace"
        default: ""
        }
    }

    /// `Resets in 2d · Mon 14:00`; empty when not known.
    var resetsLabel: String {
        guard !resets.isEmpty else { return "" }
        if resets == "now" { return "Resets now" }
        return self.resetsAt.isEmpty ? "Resets \(resets)" : "Resets \(resets) · \(ServerTime.local(self.resetsAt))"
    }
}

/// A provider's usage windows: each with how much is used, how far through
/// its period it is (the tick), and when it refills.
struct LimitWindowsList: View {
    let windows: [LimitWindowView]

    var body: some View {
        VStack(alignment: .leading, spacing: Space.m) {
            ForEach(windows, id: \.label) { window in
                VStack(alignment: .leading, spacing: Space.xs) {
                    HStack(alignment: .firstTextBaseline) {
                        Text(window.label).font(.callout.weight(.medium))
                        Spacer()
                        Text("\(window.percent)%").font(.callout.monospacedDigit()).foregroundStyle(window.high ? window.tint : .secondary)
                    }
                    PercentBar(percent: window.percent, tick: window.elapsedPercent, tint: window.tint)
                    let details = [window.resetsLabel, window.paceLabel].filter { !$0.isEmpty }
                    if !details.isEmpty {
                        Text(details.joined(separator: " · ")).font(TypeRole.meta).foregroundStyle(window.pace == "ahead" && window.high ? window.tint : .secondary)
                    }
                }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(window.label)
                .accessibilityValue(["\(window.percent) percent used", window.resetsLabel, window.paceLabel].filter { !$0.isEmpty }.joined(separator: ", "))
            }
        }
    }
}

/// Every provider's windows, for the Usage page.
struct ProviderLimitsSection: View {
    let limits: [ProviderLimitsView]

    var body: some View {
        VStack(alignment: .leading, spacing: Space.l) {
            ForEach(limits, id: \.instanceId) { provider in
                VStack(alignment: .leading, spacing: Space.s) {
                    HStack(alignment: .firstTextBaseline) {
                        Text(provider.provider).font(TypeRole.sectionHeader)
                        if !provider.checked.isEmpty {
                            Text(provider.checked == "now" ? "Updated now" : "Updated \(provider.checked) ago")
                                .font(TypeRole.meta)
                                .foregroundStyle(.secondary)
                        }
                        Spacer()
                        if !provider.externalUrl.isEmpty, let url = URL(string: provider.externalUrl) {
                            // Borderless: the section is one list row, and a link in it took the row's tap.
                            Link(provider.externalLabel.isEmpty ? "Usage on the web" : provider.externalLabel, destination: url)
                                .font(.callout)
                                .buttonStyle(.borderless)
                        }
                    }
                    if !provider.notice.isEmpty {
                        Text(provider.notice).font(TypeRole.meta).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                    }
                    if provider.windows.isEmpty {
                        if provider.notice.isEmpty {
                            Text("No usage windows reported.").font(TypeRole.meta).foregroundStyle(.secondary)
                        }
                    } else {
                        LimitWindowsList(windows: provider.windows)
                    }
                }
            }
        }
    }
}

/// The span the Usage page covers.
enum UsageSpan {
    static let days: [(UInt32, String)] = [(1, "Today"), (7, "7 days"), (30, "30 days"), (90, "90 days")]
}

// MARK: - Usage page

enum UsageFormat {
    /// `42%`, `<1%` for a share that rounds to nothing, or `—` with no tokens.
    static func share(_ percent: UInt32, tokens: UInt64) -> String {
        tokens == 0 ? "—" : percent == 0 ? "<1%" : "\(percent)%"
    }

    /// An axis figure: `1.2M`, `640k`.
    static func compact(_ tokens: Double) -> String {
        switch tokens {
        case 1_000_000_000...: String(format: "%.1fB", tokens / 1_000_000_000)
        case 1_000_000...: String(format: "%.1fM", tokens / 1_000_000)
        case 1_000...: String(format: "%.0fk", tokens / 1_000)
        default: String(format: "%.0f", tokens)
        }
    }

    static func count(_ n: UInt64, _ one: String, _ many: String) -> String {
        "\(n) \(n == 1 ? one : many)"
    }
}

extension UsageRowView {
    /// The hour for Today, the day (and hour, when bucketed so) otherwise.
    func when(hourly: Bool) -> String {
        if hourly, !self.hourLabel.isEmpty { return self.hourLabel }
        return [day, self.hourLabel].filter { !$0.isEmpty }.joined(separator: " ")
    }

    /// Its tokens by kind: `Input 41k · Cache read 30k · Output 6k`.
    var breakdown: String {
        [("Input", self.input), ("Cache read", self.cached), ("Cache write", self.cacheWrite), ("Output", self.output), ("Reasoning", self.reasoning)]
            .filter { !$0.1.isEmpty && $0.1 != "0" }
            .map { "\($0.0) \($0.1)" }
            .joined(separator: " · ")
    }

    var shareLabel: String { UsageFormat.share(self.sharePercent, tokens: totalTokens) }
    var cacheHitText: String { "\(self.cacheHitPercent)%" }
    var sessionsText: String { UsageFormat.count(self.sessions, "session", "sessions") }
}

extension UsageSliceView {
    /// One colour per kind of token, apart from the providers'.
    var tint: Color {
        switch kind {
        case "input": .blue
        case "cached": .mint
        case "cache_write": .cyan
        case "output": .indigo
        case "reasoning": Tint.plan
        default: .gray
        }
    }
}

/// Log sources that couldn't be read in full: the totals may be short.
struct UsageSourceNotices: View {
    let notices: [String]

    var body: some View {
        ForEach(notices, id: \.self) { note in
            Label(note, systemImage: "exclamationmark.triangle")
                .font(TypeRole.meta)
                .foregroundStyle(Tint.caution)
                .fixedSize(horizontal: false, vertical: true)
        }
    }
}

/// Re-reads every provider's limits now.
struct RefreshLimitsButton: View {
    let refreshing: Bool
    let refresh: () -> Void

    var body: some View {
        Button(action: refresh) {
            if refreshing {
                Label { Text("Refreshing…") } icon: { ProgressView().controlSize(.small) }
            } else {
                Label("Refresh Limits", systemImage: "arrow.clockwise")
            }
        }
        .disabled(refreshing)
        .help("Read every provider’s limits again")
    }
}

/// All providers or one: each chip with its tokens and share, and their
/// shares as one bar under them.
struct UsageProviderFilter: View {
    let usage: UsageView
    let select: (String) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s) {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: Space.xs) {
                    chip("All providers", tint: nil, selected: usage.selectedProvider.isEmpty) { select("") }
                    ForEach(usage.providers, id: \.key) { provider in
                        chip("\(provider.label) · \(provider.tokensLabel) · \(UsageFormat.share(provider.sharePercent, tokens: provider.tokens))",
                             tint: Tint.provider(provider.key), selected: usage.selectedProvider == provider.key) { select(provider.key) }
                            .help(provider.cost.isEmpty ? provider.label : "\(provider.label): \(provider.cost)")
                    }
                }
                .padding(.vertical, Space.xxs)
            }
            if usage.providers.count > 1 { shareBar }
        }
    }

    private func chip(_ title: String, tint: Color?, selected: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: Space.xs) {
                if let tint { Circle().fill(tint).frame(width: Size.dot, height: Size.dot) }
                Text(title).font(.callout).monospacedDigit().lineLimit(1)
            }
            .padding(.horizontal, Space.m)
            .padding(.vertical, Space.tight)
            .contentShape(Capsule())
            .glassChip(tint: tint, selected: selected)
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    private var shareBar: some View {
        let shown = usage.providers.filter { $0.tokens > 0 }
        let total = max(1, shown.reduce(0) { $0 + $1.tokens })
        return GeometryReader { geo in
            HStack(spacing: Space.xxs) {
                ForEach(shown, id: \.key) { provider in
                    Capsule()
                        .fill(Tint.provider(provider.key))
                        .frame(width: max(2, (geo.size.width - Space.xxs * CGFloat(shown.count - 1)) * CGFloat(provider.tokens) / CGFloat(total)))
                }
            }
        }
        .frame(height: 6)
        .accessibilityHidden(true)
    }
}

/// The span's headline figures, as cards.
struct UsageKPIs: View {
    let usage: UsageView

    /// All four in a row on the Mac, two by two on a phone or tablet.
    #if os(macOS)
    private static let columns = 4
    #else
    private static let columns = 2
    #endif

    private var activity: String {
        let counts = "\(UsageFormat.count(usage.sessions, "session", "sessions")) · \(UsageFormat.count(usage.turns, "turn", "turns"))"
        if usage.hourly || usage.activeDays == 0 { return counts }
        return "\(UsageFormat.count(UInt64(usage.activeDays), "active day", "active days")) · \(counts)"
    }

    var body: some View {
        LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: Space.m, alignment: .top), count: Self.columns), spacing: Space.m) {
            card("Total tokens", usage.totalTokens, usage.rangeLabel)
                .help(usage.totalTokensExact.isEmpty ? "" : "\(usage.totalTokensExact) tokens")
                .accessibilityValue(usage.totalTokensExact.isEmpty ? usage.totalTokens : "\(usage.totalTokensExact) tokens")
            card("Cost", usage.totalCost, "Estimated")
            card("Cache hit", usage.cacheHitLabel, "Input read from cache") {
                if !usage.cacheHitLabel.isEmpty {
                    PercentBar(percent: usage.cacheHitPercent, tint: Tint.success)
                }
            }
            card(usage.hourly ? "Per active hour" : "Per active day", usage.dailyAvgTokens, activity)
        }
    }

    private func card(_ title: String, _ value: String, _ detail: String) -> some View {
        card(title, value, detail) { EmptyView() }
    }

    private func card<Extra: View>(_ title: String, _ value: String, _ detail: String, @ViewBuilder extra: () -> Extra) -> some View {
        VStack(alignment: .leading, spacing: Space.xxs) {
            Text(title).font(TypeRole.meta).foregroundStyle(.secondary)
            Text(value.isEmpty ? "—" : value).font(.title2.weight(.semibold).monospacedDigit()).lineLimit(1).minimumScaleFactor(0.7)
            extra()
            Text(detail).font(TypeRole.metaSmall).foregroundStyle(.secondary).lineLimit(2)
        }
        .padding(Space.m)
        .frame(maxWidth: .infinity, alignment: .leading)
        .cardSurface()
        .accessibilityElement(children: .combine)
    }
}

/// Tokens over the span (an hour a bar for Today, a day otherwise), stacked
/// by provider; pointing at a bar says its slot, total and cost.
struct UsageActivityChart: View {
    let usage: UsageView
    @State private var selected: String?

    private struct Segment: Identifiable {
        let id: String
        let slot: String
        let provider: String
        let tokens: Double
        let detail: String
        let total: String
    }

    private var segments: [Segment] {
        usage.series.flatMap { point -> [Segment] in
            // Empty slots draw nothing; the x scale keeps their place.
            if point.byProvider.isEmpty {
                guard point.totalTokens > 0 else { return [] }
                return [Segment(id: point.key, slot: point.key, provider: "Tokens", tokens: Double(point.totalTokens), detail: point.detailLabel, total: point.totalLabel)]
            }
            return point.byProvider.map {
                Segment(id: "\(point.key)/\($0.providerKey)", slot: point.key, provider: $0.provider, tokens: Double($0.tokens), detail: point.detailLabel, total: point.totalLabel)
            }
        }
    }

    /// Providers in the order of their chips, each in its colour.
    private var scale: (names: [String], colors: [Color]) {
        var names: [String] = []
        var colors: [Color] = []
        for point in usage.series {
            for part in point.byProvider where !names.contains(part.provider) {
                names.append(part.provider)
                colors.append(Tint.provider(part.providerKey))
            }
        }
        if usage.series.contains(where: { $0.byProvider.isEmpty && $0.totalTokens > 0 }) {
            names.append("Tokens")
            colors.append(.accentColor)
        }
        return (names, colors)
    }

    /// A label every few slots, so they don't crowd.
    private var axisSlots: [String] {
        let keys = usage.series.map(\.key)
        let step = max(1, keys.count / (usage.hourly ? 6 : 7))
        return keys.enumerated().filter { $0.offset % step == 0 }.map(\.element)
    }

    private var shown: UsageSeriesPointView? {
        selected.flatMap { key in usage.series.first { $0.key == key } }
    }

    var body: some View {
        let scale = scale
        VStack(alignment: .leading, spacing: Space.s) {
            HStack(alignment: .firstTextBaseline) {
                Text(usage.hourly ? "Today by hour" : "Daily activity").font(TypeRole.sectionHeader)
                Spacer()
                if let shown {
                    Text(shown.cost.isEmpty ? "\(shown.detailLabel): \(shown.totalLabel)" : "\(shown.detailLabel): \(shown.totalLabel) (\(shown.cost))")
                        .font(TypeRole.meta.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
            }
            Chart(segments) { segment in
                BarMark(x: .value("When", segment.slot), y: .value("Tokens", segment.tokens))
                    .foregroundStyle(by: .value("Provider", segment.provider))
                    .opacity(selected == nil || selected == segment.slot ? 1 : 0.45)
                    .accessibilityLabel(segment.detail)
                    .accessibilityValue("\(segment.provider), of \(segment.total)")
            }
            .chartForegroundStyleScale(domain: scale.names, range: scale.colors)
            .chartXScale(domain: usage.series.map(\.key))
            .chartXSelection(value: $selected)
            .chartXAxis {
                AxisMarks(values: axisSlots) { value in
                    AxisValueLabel {
                        if let key = value.as(String.self) {
                            Text(usage.series.first { $0.key == key }?.label ?? key)
                        }
                    }
                }
            }
            .chartYAxis {
                AxisMarks(position: .leading, values: .automatic(desiredCount: 4)) { value in
                    AxisGridLine()
                    AxisValueLabel {
                        if let tokens = value.as(Double.self) { Text(UsageFormat.compact(tokens)) }
                    }
                }
            }
            .chartLegend(scale.names.count > 1 ? .visible : .hidden)
            .frame(height: 200)
        }
    }
}

/// What the tokens were: input, cache reads and writes, output, reasoning.
struct UsageTokenMix: View {
    let slices: [UsageSliceView]

    var body: some View {
        let shown = slices.filter { $0.tokens > 0 }
        let total = max(1, shown.reduce(0) { $0 + $1.tokens })
        VStack(alignment: .leading, spacing: Space.m) {
            Text("Token composition").font(TypeRole.sectionHeader)
            GeometryReader { geo in
                HStack(spacing: Space.xxs) {
                    ForEach(shown, id: \.kind) { slice in
                        Capsule()
                            .fill(slice.tint)
                            .frame(width: max(2, (geo.size.width - Space.xxs * CGFloat(max(0, shown.count - 1))) * CGFloat(slice.tokens) / CGFloat(total)))
                    }
                }
            }
            .frame(height: 8)
            .accessibilityHidden(true)
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 110), spacing: Space.m, alignment: .topLeading)], alignment: .leading, spacing: Space.s) {
                ForEach(slices, id: \.kind) { slice in
                    HStack(alignment: .firstTextBaseline, spacing: Space.xs) {
                        Circle().fill(slice.tint).frame(width: Size.dot, height: Size.dot)
                        VStack(alignment: .leading, spacing: 0) {
                            Text(slice.label).font(TypeRole.meta).foregroundStyle(.secondary)
                            Text(slice.tokensLabel).font(.callout.weight(.semibold).monospacedDigit())
                            Text("\(slice.percent)%").font(TypeRole.metaSmall.monospacedDigit()).foregroundStyle(.secondary)
                        }
                    }
                    .accessibilityElement(children: .combine)
                }
            }
        }
    }
}
