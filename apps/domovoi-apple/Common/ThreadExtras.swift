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
            Button(parent.title) { open(parent.threadId) }
                .buttonStyle(.plain)
                .fontWeight(.medium)
                .foregroundStyle(.tint)
                .lineLimit(1)
            Spacer(minLength: Space.s)
            if canMergeBack {
                Button { confirmMerge = true } label: { Label("Merge Back", systemImage: "arrow.uturn.left") }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .help("Send what this fork did back to the thread it came from")
            }
        }
        .font(TypeRole.meta)
        .padding(.horizontal, Space.m)
        .padding(.vertical, Space.s)
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
                    Button("Resume") { actions.send(.resumeQueue) }
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
        HStack(alignment: .top, spacing: Space.s) {
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
            Menu {
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
            } label: {
                Image(systemName: "ellipsis.circle")
                    .imageScale(.large)
                    .foregroundStyle(.secondary)
                    .frame(width: 32, height: 32)
                    .contentShape(Rectangle())
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .accessibilityLabel("Queued message actions")
        }
        .accessibilityElement(children: .contain)
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
                Button(expanded ? "Show Less" : "Show More") { expanded.toggle() }
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
        HStack(spacing: Space.s) {
            StatusDot(status: item.status)
            Image(systemName: "person.2").foregroundStyle(.secondary)
            Text(item.title).font(TypeRole.rowTitle.weight(.medium)).lineLimit(2).fixedSize(horizontal: false, vertical: true)
        }
    }

    @ViewBuilder private var buttons: some View {
        if item.canStop {
            Button { actions.send(.stopSubagent(subagentId: item.id)) } label: { Label("Stop", systemImage: "stop.circle") }
                .buttonStyle(.bordered)
                .controlSize(.small)
        }
        if !item.childThreadId.isEmpty {
            Button { open(item.childThreadId) } label: { Label("Open", systemImage: "arrow.up.right.square") }
                .buttonStyle(.bordered)
                .controlSize(.small)
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

/// How full the agent's context window is: a small ring, with the counts
/// on hover (Mac) or in its label.
struct ContextMeter: View {
    let context: ContextUsageView
    var showsLabel = false

    var body: some View {
        HStack(spacing: Space.xs) {
            ZStack {
                Circle().stroke(.quaternary, lineWidth: 3)
                if context.maxTokens > 0 {
                    Circle()
                        .trim(from: 0, to: CGFloat(context.percent) / 100)
                        .stroke(tint, style: StrokeStyle(lineWidth: 3, lineCap: .round))
                        .rotationEffect(.degrees(-90))
                }
            }
            .frame(width: 14, height: 14)
            if showsLabel || context.maxTokens == 0 {
                Text(context.maxTokens > 0 ? "\(context.percent)%" : context.label)
                    .font(TypeRole.meta.monospacedDigit())
                    .foregroundStyle(context.high ? Tint.caution : .secondary)
                    .lineLimit(1)
                    .fixedSize()
            }
        }
        .fixedSize()
        .help("Context: \(context.label)\(context.high ? ". Nearly full: the agent will compact the conversation soon." : "")")
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Context window")
        .accessibilityValue(context.maxTokens > 0 ? "\(context.percent) percent used, \(context.label)" : context.label)
    }

    private var tint: Color {
        context.high ? Tint.caution : context.percent >= 50 ? Color.accentColor : .secondary
    }
}
