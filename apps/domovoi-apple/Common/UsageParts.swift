import App
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
            Text(context.label).font(.callout).foregroundStyle(.secondary)
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
            if !context.breakdown.isEmpty || !context.cost.isEmpty {
                Divider()
                VStack(spacing: Space.xs) {
                    ForEach(context.breakdown, id: \.label) { line in
                        LabeledContent(line.label) { Text(line.value).monospacedDigit() }
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
        return resetsAt.isEmpty ? "Resets \(resets)" : "Resets \(resets) · \(ServerTime.local(resetsAt))"
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
                    HStack {
                        Text(provider.provider).font(TypeRole.sectionHeader)
                        Spacer()
                        if !provider.externalUrl.isEmpty, let url = URL(string: provider.externalUrl) {
                            // Borderless: the section is one list row, and a link in it took the row's tap.
                            Link(provider.externalLabel.isEmpty ? "Usage on the web" : provider.externalLabel, destination: url)
                                .font(.callout)
                                .buttonStyle(.borderless)
                        }
                    }
                    if provider.windows.isEmpty {
                        Text("No usage windows reported.").font(TypeRole.meta).foregroundStyle(.secondary)
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
