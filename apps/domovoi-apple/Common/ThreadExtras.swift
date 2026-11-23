import App
import SwiftUI

/// "Forked from …" or "Subagent of …" above a thread's transcript, with the
/// way back to that thread (and, for a fork, to send its work back there).
struct ParentBanner: View {
    let parent: ThreadLinkView
    let canMergeBack: Bool
    let actions: TranscriptActions
    @Environment(\.openThread) private var openThread
    @State private var confirmMerge = false

    var body: some View {
        HStack(spacing: Space.s) {
            Image(systemName: parent.relation == "subagent" ? "person.2" : "arrow.triangle.branch")
                .foregroundStyle(.secondary)
            Text(parent.relation == "subagent" ? "Subagent of" : "Forked from")
                .foregroundStyle(.secondary)
            // The banner's vertical padding is the link's, so all of it taps.
            Button { open(parent.threadId) } label: {
                Text(parent.title).padding(.vertical, Space.s).contentShape(Rectangle()).tapTarget()
            }
                .buttonStyle(.plain)
                .fontWeight(.medium)
                .foregroundStyle(.tint)
                .lineLimit(1)
            Spacer(minLength: Space.s)
            if canMergeBack {
                Button { confirmMerge = true } label: { Label("Merge Back", systemImage: "arrow.uturn.left") }
                    .buttonStyle(.bordered)
                    .cardControlSize()
                    .help("Send what this fork did back to the thread it came from")
            }
        }
        .font(TypeRole.meta)
        .padding(.horizontal, Space.m)
        .frame(maxWidth: .infinity, alignment: .leading)
        .cardSurface()
        .confirmationDialog("Merge this fork back?", isPresented: $confirmMerge, titleVisibility: .visible) {
            Button("Merge Back") { actions.send(.mergeBack) }
        } message: {
            Text("“\(parent.title)” gets a summary of what was done here with its next message.")
        }
    }

    private func open(_ id: String) {
        if let openThread { openThread(id) } else { actions.send(.selectThread(threadId: id)) }
    }
}

/// Messages queued behind the turn under way: each can be sent into the
/// turn now, edited, moved or dropped.
struct QueuedSection: View {
    let queued: [QueuedMessageView]
    let held: Bool
    let actions: TranscriptActions
    @State private var editing: QueuedMessageView?
    @State private var editText = ""

    var body: some View {
        VStack(alignment: .trailing, spacing: Space.s) {
            HStack(spacing: Space.xs) {
                Image(systemName: held ? "pause.circle" : "clock")
                Text(held ? "Queue paused after the stop" : queued.count == 1 ? "Queued for after this turn" : "\(queued.count) queued for after this turn")
                if held {
                    Button { actions.send(.resumeQueue) } label: { Text("Resume").tapTarget() }
                        .buttonStyle(.borderless)
                        .fontWeight(.semibold)
                }
            }
            .font(TypeRole.meta)
            .foregroundStyle(.secondary)
            ForEach(queued, id: \.runId) { item in
                row(item)
            }
        }
        .frame(maxWidth: .infinity, alignment: .trailing)
        // A turn to come: the room above a sent message.
        .padding(.top, Space.m)
        .alert("Edit Queued Message", isPresented: Binding(get: { editing != nil }, set: { if !$0 { editing = nil } })) {
            TextField("Message", text: $editText, axis: .vertical)
            Button("Save") {
                if let editing { actions.send(.editQueued(runId: editing.runId, text: editText)) }
                editing = nil
            }
            Button("Cancel", role: .cancel) { editing = nil }
        }
    }

    private func row(_ item: QueuedMessageView) -> some View {
        HStack(spacing: 0) {
            Spacer(minLength: Space.xxl * 1.5)
            Text(item.text)
                .lineLimit(6)
                .padding(.horizontal, Space.l)
                .padding(.vertical, Space.m)
                .background(Color.accentColor.opacity(0.07), in: RoundedRectangle(cornerRadius: Radius.bubble, style: .continuous))
                .overlay(
                    RoundedRectangle(cornerRadius: Radius.bubble, style: .continuous)
                        .strokeBorder(Color.accentColor.opacity(0.35), style: StrokeStyle(lineWidth: 1, dash: [4, 3]))
                )
                .contextMenu { menuItems(item) }
                // The menu hangs off the bubble's leading edge, in the
                // margin, so queued bubbles end where sent ones do.
                .overlay(alignment: .topLeading) {
                    Menu { menuItems(item) } label: {
                        Image(systemName: "ellipsis.circle")
                            .imageScale(.large)
                            .foregroundStyle(.secondary)
                            .frame(width: Size.tap, height: Size.tap)
                            .contentShape(Rectangle())
                    }
                    .menuStyle(.borderlessButton)
                    .menuIndicator(.hidden)
                    .fixedSize()
                    .alignmentGuide(.leading) { $0[.trailing] }
                    .accessibilityLabel("Queued message actions")
                }
        }
        .accessibilityElement(children: .contain)
    }

    @ViewBuilder private func menuItems(_ item: QueuedMessageView) -> some View {
        if item.canSteer {
            Button { actions.send(.steerQueued(runId: item.runId)) } label: { Label("Send Now", systemImage: "arrow.up.circle") }
        }
        Button {
            editText = item.text
            editing = item
        } label: { Label("Edit", systemImage: "pencil") }
        if !item.first {
            Button { actions.send(.moveQueued(runId: item.runId, delta: -1)) } label: { Label("Move Up", systemImage: "arrow.up") }
        }
        if !item.last {
            Button { actions.send(.moveQueued(runId: item.runId, delta: 1)) } label: { Label("Move Down", systemImage: "arrow.down") }
        }
        Divider()
        Button(role: .destructive) { actions.send(.cancelQueued(runId: item.runId)) } label: { Label("Remove", systemImage: "trash") }
    }
}

/// A subagent the agent started: what it was asked, how it went, and its
/// own thread.
struct SubagentRow: View {
    let item: ItemView
    let actions: TranscriptActions
    @Environment(\.openThread) private var openThread
    @State private var expanded = false

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s) {
            // The buttons beside the title when they fit, under it otherwise (a phone).
            ViewThatFits(in: .horizontal) {
                HStack(spacing: Space.s) {
                    header.layoutPriority(1)
                    Spacer(minLength: Space.s)
                    buttons
                }
                VStack(alignment: .leading, spacing: Space.s) {
                    header
                    HStack(spacing: Space.s) { buttons }
                }
            }
            if !item.progress.isEmpty {
                Label(item.progress, systemImage: "ellipsis")
                    .font(TypeRole.meta)
                    .foregroundStyle(.tint)
                    .lineLimit(1)
            }
            if !item.text.isEmpty {
                Text(item.text)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(expanded ? nil : 2)
                    .selectable(limited: !expanded)
            }
            if let result = item.output, !result.isEmpty {
                Divider()
                Text(result)
                    .font(.callout)
                    .lineLimit(expanded ? nil : 4)
                    .selectable(limited: !expanded)
            }
            if item.text.count > 160 || (item.output?.count ?? 0) > 300 {
                Button { expanded.toggle() } label: { Text(expanded ? "Show Less" : "Show More").tapTarget() }
                    .buttonStyle(.borderless)
                    .font(TypeRole.meta)
            }
        }
        .padding(Space.m)
        .frame(maxWidth: .infinity, alignment: .leading)
        .cardSurface()
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Subagent \(item.title), \(statusText)")
    }

    private var header: some View {
        // The status dot trails, as on every other row.
        HStack(alignment: .firstTextBaseline, spacing: Space.s) {
            Image(systemName: "person.2").foregroundStyle(.secondary).iconColumn()
            Text(item.title).font(TypeRole.cardTitle).lineLimit(2).fixedSize(horizontal: false, vertical: true)
            StatusDot(status: item.status)
        }
    }

    @ViewBuilder private var buttons: some View {
        if item.canStop {
            Button { actions.send(.stopSubagent(subagentId: item.id)) } label: { Label("Stop", systemImage: "stop.circle") }
                .buttonStyle(.bordered)
                .cardControlSize()
        }
        if !item.childThreadId.isEmpty {
            Button { open(item.childThreadId) } label: { Label("Open", systemImage: "arrow.up.right.square") }
                .buttonStyle(.bordered)
                .cardControlSize()
        }
    }

    private var statusText: String {
        switch item.status {
        case "running", "pending": "working"
        case "waiting": "waiting"
        case "failed": "failed"
        case "cancelled", "interrupted": "stopped"
        default: "done"
        }
    }

    private func open(_ id: String) {
        if let openThread { openThread(id) } else { actions.send(.selectThread(threadId: id)) }
    }
}

/// The thread's usage at a glance: a small ring for how full the agent's
/// context window is (with where it compacts), or for the fullest provider
/// limit when there is no context yet; a dot when a limit runs low. A click
/// or tap shows both in detail.
struct UsageMeter: View {
    let usage: SessionUsageView
    var showsLabel = false
    /// Compacts now (`ContextUsageView.can_compact`).
    var compact: (() -> Void)?
    /// Opens the Usage page.
    var showAll: (() -> Void)?
    @State private var showing = false

    /// What the ring fills to: the context, else the fullest limit.
    private var ring: (percent: UInt32, tick: UInt32, tint: Color)? {
        if let context = usage.context {
            return context.maxTokens > 0 ? (context.percent, context.compactAtPercent, context.tint) : nil
        }
        return usage.fullest.map { ($0.percent, 0, $0.tint) }
    }

    private var label: String {
        if let context = usage.context { return context.maxTokens > 0 ? "\(context.percent)%" : context.label }
        return usage.fullest.map { "\($0.percent)%" } ?? ""
    }

    private var labelTint: Color {
        if let context = usage.context { return context.high ? context.tint : .secondary }
        return usage.fullest.map { $0.high ? $0.tint : .secondary } ?? .secondary
    }

    private var alertTint: Color? {
        switch usage.limitAlert {
        case "danger": Tint.danger
        case "caution": Tint.caution
        default: nil
        }
    }

    /// `Context: 48k of 200k tokens` and `Claude Weekly: 92% used, resets in 2d`.
    private var summary: [String] {
        var lines: [String] = []
        if let context = usage.context {
            lines.append("Context: \(context.label)\(context.maxTokens > 0 ? " (\(context.percent)%)" : "")")
        }
        if let fullest = usage.fullest, let provider = usage.limits?.provider {
            lines.append("\(provider) \(fullest.label): \(fullest.percent)% used\(fullest.resets.isEmpty ? "" : ", resets \(fullest.resets)")")
        } else if !usage.limitsNotice.isEmpty, let provider = usage.limits?.provider {
            lines.append("\(provider) limits: \(usage.limitsNotice)")
        }
        return lines
    }

    var body: some View {
        Button { showing.toggle() } label: {
            HStack(spacing: Space.xs) {
                ZStack {
                    Circle().stroke(.quaternary, lineWidth: 3)
                    if let ring {
                        Circle()
                            .trim(from: 0, to: CGFloat(ring.percent) / 100)
                            .stroke(ring.tint, style: StrokeStyle(lineWidth: 3, lineCap: .round))
                            .rotationEffect(.degrees(-90))
                        if ring.tick > 0, ring.tick < 100 {
                            // Where the agent compacts on its own.
                            let at = CGFloat(ring.tick) / 100
                            Circle()
                                .trim(from: at - 0.012, to: at + 0.012)
                                .stroke(.primary.opacity(0.75), lineWidth: 3)
                                .rotationEffect(.degrees(-90))
                        }
                    }
                }
                .frame(width: 14, height: 14)
                .overlay(alignment: .topTrailing) {
                    if let alertTint {
                        // A provider limit nearly used up.
                        Circle()
                            .fill(alertTint)
                            .frame(width: 6, height: 6)
                            .overlay(Circle().stroke(.background, lineWidth: 1))
                            .offset(x: 3, y: -3)
                    }
                }
                if showsLabel || (usage.context.map { $0.maxTokens == 0 } ?? false) {
                    Text(label)
                        .font(TypeRole.meta.monospacedDigit())
                        .foregroundStyle(labelTint)
                        .lineLimit(1)
                        .fixedSize()
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .fixedSize()
        .help(summary.joined(separator: "\n"))
        .popover(isPresented: $showing) {
            SessionUsageDetails(
                usage: usage,
                compact: compact.map { compact in { showing = false; compact() } },
                showAll: showAll.map { showAll in { showing = false; showAll() } }
            )
            .presentationCompactAdaptation(.popover)
        }
        .accessibilityLabel("Usage")
        .accessibilityValue(summary.joined(separator: ". "))
        .accessibilityHint("Shows the context window and the provider's limits")
    }
}

/// A subagent (or side thread) of this one waits on the user for an
/// approval or an answer: one line above the composer, opening it.
struct WaitingChildBanner: View {
    let child: ThreadLinkView
    let open: (String) -> Void

    var body: some View {
        HStack(spacing: Space.s) {
            Image(systemName: "hand.raised.fill").foregroundStyle(Tint.caution)
            Text(child.relation == "subagent" ? "Subagent waiting:" : "Thread waiting:").foregroundStyle(.secondary)
            Text(child.title.isEmpty ? "Untitled" : child.title).fontWeight(.medium).lineLimit(1)
            Spacer(minLength: Space.s)
            // The banner's vertical padding is the button's, so all of it taps.
            Button { open(child.threadId) } label: {
                Text("Open").padding(.vertical, Space.s).contentShape(Rectangle()).tapTarget()
            }
                .buttonStyle(.borderless)
                .fontWeight(.semibold)
        }
        .font(TypeRole.meta)
        .padding(.horizontal, Space.m)
        .glassChip(tint: Tint.caution.opacity(0.2), in: RoundedRectangle(cornerRadius: Radius.card, style: .continuous))
        .accessibilityElement(children: .combine)
        .accessibilityAction(named: "Open") { open(child.threadId) }
    }
}

/// Opens the latest plan and checklist without moving the transcript.
struct WorkProgressStrip: View {
    let plan: PlanProgressView?
    let tasks: TaskProgressView?
    let open: () -> Void

    var body: some View {
        Button(action: open) {
            HStack(spacing: Space.s) {
                Image(systemName: "checklist").foregroundStyle(.tint)
                Text([plan?.label, tasks?.label].compactMap { $0 }.joined(separator: " · "))
                    .font(TypeRole.meta).lineLimit(1)
                Spacer(minLength: Space.s)
                if let tasks, tasks.total > 0 {
                    PercentBar(percent: tasks.done * 100 / tasks.total).frame(width: 64)
                        .accessibilityHidden(true)
                }
                #if os(iOS)
                Image(systemName: "chevron.up").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                #else
                Image(systemName: "chevron.up").font(.caption2)
                #endif
            }
            #if os(iOS)
            // A full tap target, on the composer panel's glass, in the chips' shape.
            .padding(.horizontal, Space.l)
            .frame(minHeight: Size.tap)
            .contentShape(Capsule())
            #else
            .padding(.horizontal, Space.m)
            .padding(.vertical, Space.s)
            .contentShape(Rectangle())
            #endif
        }
        .buttonStyle(.plain)
        #if os(iOS)
        .glassPanel(in: Capsule())
        #else
        .glassChip(in: RoundedRectangle(cornerRadius: Radius.chip + 2, style: .continuous))
        #endif
        .help("Show plan and checklist")
        .accessibilityHint("Shows plan and checklist")
    }
}

struct WorkProgressPanel: View {
    @ObservedObject var core: Core
    let threadID: String
    let close: () -> Void

    private var header: ThreadHeader? {
        core.view.header.flatMap { $0.id == threadID ? $0 : nil }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text("Plan and checklist").font(.headline).accessibilityAddTraits(.isHeader)
                Spacer()
                Button("Done", action: close).keyboardShortcut(.cancelAction)
            }
            .padding(Space.l)
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: Space.l) {
                    if let plan = header?.plan {
                        Text(plan.label).font(TypeRole.sectionHeader).accessibilityAddTraits(.isHeader)
                        MarkdownBlocks(blocks: plan.blocks)
                        if plan.canImplement {
                            Button("Implement plan", systemImage: "play.fill") {
                                close()
                                core.update(.implementPlan(planId: plan.planId))
                            }
                            .buttonStyle(.borderedProminent)
                        }
                    }
                    if let tasks = header?.tasks {
                        if header?.plan != nil { Divider() }
                        Text("Checklist · \(tasks.done)/\(tasks.total)")
                            .font(TypeRole.sectionHeader).accessibilityAddTraits(.isHeader)
                        if tasks.previousRun {
                            Text("Previous run").font(TypeRole.meta).foregroundStyle(.secondary)
                        }
                        if let explanation = tasks.explanation { Text(explanation).font(.callout) }
                        TodoSteps(todos: tasks.todos)
                    }
                    if header?.plan == nil && header?.tasks == nil {
                        Text("No plan or checklist is available.").foregroundStyle(.secondary)
                    }
                }
                .frame(maxWidth: Space.column, alignment: .leading)
                .padding(Space.l)
                .frame(maxWidth: .infinity)
            }
        }
        #if os(macOS)
        .frame(minWidth: 460, idealWidth: 620, minHeight: 320, idealHeight: 520)
        #endif
    }
}

/// The last turn stopped on a usage limit: when it resets, and continuing
/// (or snoozing) the thread then.
struct UsageLimitBanner: View {
    let resets: String
    let plan: String
    let actions: TranscriptActions

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s) {
            Label(resets == "now" ? "The usage limit has reset." : "Usage limit reached. It resets \(resets).", systemImage: "gauge.with.dots.needle.100percent")
                .fontWeight(.medium)
            HStack(spacing: Space.s) {
                switch plan {
                case "resume":
                    Text("Continues when it resets.").foregroundStyle(.secondary)
                    Button("Cancel") { actions.send(.cancelLimitPlan) }.cardControlSize()
                case "snooze":
                    Text("Snoozed until it resets.").foregroundStyle(.secondary)
                    Button("Cancel") { actions.send(.cancelLimitPlan) }.cardControlSize()
                default:
                    Button("Resume at Reset") { actions.send(.planForLimitReset(snooze: false)) }
                        .buttonStyle(.borderedProminent)
                        .cardControlSize()
                        .help("Send “Continue where you left off.” once the limit resets")
                    Button("Snooze Until Reset") { actions.send(.planForLimitReset(snooze: true)) }
                        .cardControlSize()
                }
            }
        }
        .font(TypeRole.meta)
        .padding(Space.m)
        .frame(maxWidth: .infinity, alignment: .leading)
        .cardSurface()
    }
}
