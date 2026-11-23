import ActivityKit
import SwiftUI
import WidgetKit

/// A thread's turn on the Lock Screen, in the Dynamic Island and in the
/// Watch's Smart Stack: what the agent is doing now, how far its to-do list
/// has got, what it waits on you for, and how the turn came out.
struct AgentLiveActivity: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: AgentActivityAttributes.self) { context in
            ActivityRoot(attributes: context.attributes, state: context.state, isStale: context.isStale)
                .activityBackgroundTint(nil)
                .activitySystemActionForegroundColor(ActivityLook.tint(context.state))
                .widgetURL(threadURL(context.attributes.threadId, server: context.attributes.serverId))
        } dynamicIsland: { context in
            let (attributes, state) = (context.attributes, context.state)
            return DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    StatusMark(state: state, size: 40).padding(.leading, 2)
                }
                DynamicIslandExpandedRegion(.trailing) {
                    VStack(alignment: .trailing, spacing: 2) {
                        TimeLabel(state: state).font(.callout.monospacedDigit().weight(.semibold)).foregroundStyle(ActivityLook.tint(state))
                        if let model = attributes.model { Text(model).font(.caption2).foregroundStyle(.secondary).lineLimit(1) }
                    }
                    .padding(.trailing, 2)
                }
                DynamicIslandExpandedRegion(.center) {
                    VStack(alignment: .leading, spacing: 1) {
                        Text(attributes.title).font(.headline).lineLimit(1)
                        Text(ActivityLook.place(attributes)).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                DynamicIslandExpandedRegion(.bottom) {
                    ActivityDetail(attributes: attributes, state: state, isStale: context.isStale, compact: true)
                        .padding(.top, 4)
                }
            } compactLeading: {
                CompactMark(state: state)
            } compactTrailing: {
                CompactTrailing(state: state)
            } minimal: {
                CompactMark(state: state)
            }
            .keylineTint(ActivityLook.tint(state))
            .widgetURL(threadURL(attributes.threadId, server: attributes.serverId))
        }
        .supplementalActivityFamilies([.small])
    }
}

// MARK: - Look

/// Colours and symbols, after the app's own (`StatusStyle`, `Tint`, `WorkingRow`).
enum ActivityLook {
    static func tint(_ state: AgentActivityAttributes.ContentState) -> Color {
        state.isQuestion ? .indigo : StatusLook.color(state.status)
    }

    static func symbol(_ state: AgentActivityAttributes.ContentState) -> String {
        switch state.status {
        case "working": actionSymbol(state.actionKind)
        case "approval": state.isQuestion ? "questionmark.bubble.fill" : "hand.raised.fill"
        default: StatusLook.icon(state.status)
        }
    }

    static func actionSymbol(_ kind: String?) -> String {
        switch kind {
        case "command": "terminal"
        case "file_change": "doc.badge.gearshape"
        case "search": "magnifyingglass"
        case "tool": "wrench.and.screwdriver"
        case "subagent": "person.2"
        default: "sparkle"
        }
    }

    /// What a step of this kind is, when it does not say what it works on.
    static func actionLabel(_ kind: String?) -> String {
        switch kind {
        case "command": "Running a command"
        case "file_change": "Editing files"
        case "search": "Searching"
        case "tool": "Using a tool"
        case "subagent": "Working with a subagent"
        default: "Working"
        }
    }

    /// The turn's outcome, in a word or three.
    static func outcome(_ state: AgentActivityAttributes.ContentState) -> String {
        let took = duration(state).map { " in \($0)" } ?? ""
        switch state.status {
        case "completed": return "Finished\(took)"
        case "failed": return "Failed" + (duration(state).map { " after \($0)" } ?? "")
        case "interrupted": return "Stopped" + (duration(state).map { " after \($0)" } ?? "")
        default: return state.detail.isEmpty ? StatusLook.label(state.status) : state.detail
        }
    }

    /// How long the turn took ("4m 12s"), once it ended.
    static func duration(_ state: AgentActivityAttributes.ContentState) -> String? {
        guard let ended = state.endedAt else { return nil }
        let seconds = max(0, ended.timeIntervalSince(state.startedAt))
        return Duration.seconds(seconds.rounded()).formatted(.units(allowed: [.hours, .minutes, .seconds], width: .narrow, maximumUnitCount: 2))
    }

    /// Where the thread lives: project and branch.
    static func place(_ attributes: AgentActivityAttributes) -> String {
        [attributes.project, attributes.branch ?? ""].filter { !$0.isEmpty }.joined(separator: " · ")
    }
}

// MARK: - Parts

/// The Lock Screen's presentation, or the Watch's smaller one.
private struct ActivityRoot: View {
    @Environment(\.activityFamily) private var family
    let attributes: AgentActivityAttributes
    let state: AgentActivityAttributes.ContentState
    let isStale: Bool

    var body: some View {
        switch family {
        case .small: SmallActivity(attributes: attributes, state: state)
        default: LockScreenActivity(attributes: attributes, state: state, isStale: isStale)
        }
    }
}

private struct LockScreenActivity: View {
    let attributes: AgentActivityAttributes
    let state: AgentActivityAttributes.ContentState
    let isStale: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .center, spacing: 10) {
                StatusMark(state: state, size: 36)
                VStack(alignment: .leading, spacing: 1) {
                    Text(attributes.title).font(.headline).lineLimit(1)
                    let place = ActivityLook.place(attributes)
                    let line = [place, attributes.model ?? ""].filter { !$0.isEmpty }.joined(separator: " · ")
                    if !line.isEmpty { Text(line).font(.caption).foregroundStyle(.secondary).lineLimit(1) }
                }
                Spacer(minLength: 8)
                TimeLabel(state: state)
                    .font(.subheadline.monospacedDigit().weight(.semibold))
                    .foregroundStyle(ActivityLook.tint(state))
            }
            ActivityDetail(attributes: attributes, state: state, isStale: isStale, compact: false)
        }
        .padding(16)
    }
}

/// Under the title: what goes on now, what is asked, or how it came out.
private struct ActivityDetail: View {
    let attributes: AgentActivityAttributes
    let state: AgentActivityAttributes.ContentState
    let isStale: Bool
    /// In the Dynamic Island: fewer lines.
    let compact: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            switch state.status {
            case "working":
                ActionLine(state: state)
                if state.taskFraction != nil { TaskProgress(state: state, tint: ActivityLook.tint(state)) }
            case "approval":
                Asked(state: state, lines: compact ? 2 : 3)
                if state.canAnswerHere {
                    ApprovalButtons(attributes: attributes)
                } else {
                    AnswerInApp(attributes: attributes)
                }
            default:
                Outcome(state: state, lines: compact ? 1 : 2)
            }
            if isStale, state.isLive {
                Label("Not updated for a while", systemImage: "clock.badge.exclamationmark")
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// The step under way: its kind and what it works on.
private struct ActionLine: View {
    let state: AgentActivityAttributes.ContentState

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: ActivityLook.actionSymbol(state.actionKind))
                .font(.caption.weight(.semibold))
                .foregroundStyle(ActivityLook.tint(state))
                .frame(width: 16)
            if let action = state.action, !action.isEmpty {
                Text(action)
                    .font(state.actionKind == "command" ? .subheadline.monospaced() : .callout)
                    // A path's end is its file's name.
                    .truncationMode(state.actionKind == "file_change" ? .head : .tail)
            } else {
                Text(ActivityLook.actionLabel(state.actionKind)).font(.callout)
            }
        }
        .lineLimit(1)
        .foregroundStyle(.secondary)
    }
}

/// The to-do list's progress: a bar, the count and the task under way.
private struct TaskProgress: View {
    let state: AgentActivityAttributes.ContentState
    let tint: Color

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            ProgressView(value: state.taskFraction ?? 0).tint(tint)
            HStack(spacing: 6) {
                Text("\(state.tasksDone ?? 0)/\(state.tasksTotal ?? 0)").monospacedDigit().fontWeight(.semibold)
                if let current = state.taskCurrent, !current.isEmpty {
                    Text(current).lineLimit(1)
                }
            }
            .font(.caption)
            .foregroundStyle(.secondary)
        }
    }
}

/// What an approval or a question asks.
private struct Asked: View {
    let state: AgentActivityAttributes.ContentState
    let lines: Int

    var body: some View {
        let tint = ActivityLook.tint(state)
        let text = state.detail.isEmpty ? (state.isQuestion ? "Needs your answer" : "Needs your approval") : state.detail
        Text(text)
            // An approval's prompt is usually a command.
            .font(state.isQuestion ? .callout : .subheadline.monospaced())
            .lineLimit(lines)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 10)
            .padding(.vertical, 7)
            .background(tint.opacity(0.16), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
    }
}

/// How the turn came out: its answer's start, or why it failed.
private struct Outcome: View {
    let state: AgentActivityAttributes.ContentState
    let lines: Int

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(ActivityLook.outcome(state))
                .font(.callout.weight(.semibold))
                .foregroundStyle(ActivityLook.tint(state))
            if let text = state.status == "failed" ? state.error : state.summary, !text.isEmpty {
                Text(text).font(.callout).foregroundStyle(.secondary).lineLimit(lines)
            }
            if let done = state.tasksDone, let total = state.tasksTotal, total > 0 {
                Text("\(done) of \(total) tasks done").font(.caption).foregroundStyle(.tertiary)
            }
        }
    }
}

/// Allow and Decline right on the Lock Screen and in the Dynamic Island:
/// the app answers in the background (`RespondToApprovalIntent`).
private struct ApprovalButtons: View {
    let attributes: AgentActivityAttributes

    var body: some View {
        HStack(spacing: 8) {
            Button(intent: RespondToApprovalIntent(threadId: attributes.threadId, serverId: attributes.serverId, decision: "decline")) {
                Text("Decline").frame(maxWidth: .infinity)
            }
            .tint(.secondary)
            Button(intent: RespondToApprovalIntent(threadId: attributes.threadId, serverId: attributes.serverId, decision: "accept")) {
                Text("Allow").fontWeight(.semibold).frame(maxWidth: .infinity)
            }
            .tint(.orange)
        }
        .buttonStyle(.borderedProminent)
        .controlSize(.small)
    }
}

/// A question is answered in the app (a Live Activity takes no typing).
private struct AnswerInApp: View {
    let attributes: AgentActivityAttributes

    var body: some View {
        if let url = threadURL(attributes.threadId, server: attributes.serverId) {
            Link(destination: url) {
                Label("Answer in Domovoi", systemImage: "arrow.up.forward.app")
                    .font(.callout.weight(.semibold))
                    .foregroundStyle(.white)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 6)
                    .background(Color.indigo, in: Capsule())
            }
        }
    }
}

/// The status in a tinted circle: the step's kind while working.
private struct StatusMark: View {
    let state: AgentActivityAttributes.ContentState
    let size: CGFloat

    var body: some View {
        let tint = ActivityLook.tint(state)
        ZStack {
            Circle().fill(tint.opacity(0.2))
            if state.status == "working", let fraction = state.taskFraction {
                Circle().trim(from: 0, to: fraction).stroke(tint, style: StrokeStyle(lineWidth: 3, lineCap: .round)).rotationEffect(.degrees(-90))
            }
            Image(systemName: ActivityLook.symbol(state))
                .font(.system(size: size * 0.42, weight: .semibold))
                .foregroundStyle(tint)
        }
        .frame(width: size, height: size)
    }
}

/// The Dynamic Island's compact and minimal mark: the to-do list's progress
/// as a ring while working, the status otherwise.
private struct CompactMark: View {
    let state: AgentActivityAttributes.ContentState

    var body: some View {
        let tint = ActivityLook.tint(state)
        if state.status == "working", let fraction = state.taskFraction {
            ZStack {
                Circle().stroke(tint.opacity(0.3), lineWidth: 2.5)
                Circle().trim(from: 0, to: fraction).stroke(tint, style: StrokeStyle(lineWidth: 2.5, lineCap: .round)).rotationEffect(.degrees(-90))
                Image(systemName: ActivityLook.symbol(state)).font(.system(size: 9, weight: .bold)).foregroundStyle(tint)
            }
            .frame(width: 20, height: 20)
        } else {
            Image(systemName: ActivityLook.symbol(state)).foregroundStyle(tint)
        }
    }
}

private struct CompactTrailing: View {
    let state: AgentActivityAttributes.ContentState

    var body: some View {
        let tint = ActivityLook.tint(state)
        switch state.status {
        case "working":
            Text(timerInterval: state.startedAt...Date.distantFuture, countsDown: false)
                .monospacedDigit()
                .multilineTextAlignment(.trailing)
                .frame(maxWidth: 48)
                .foregroundStyle(tint)
        case "approval":
            Text(state.isQuestion ? "Reply" : "Allow?").font(.caption.weight(.semibold)).foregroundStyle(tint)
        default:
            Image(systemName: StatusLook.icon(state.status)).foregroundStyle(tint)
        }
    }
}

/// Time since the turn started while it runs; when it ended once over (how
/// long it took is in the outcome line under it).
private struct TimeLabel: View {
    let state: AgentActivityAttributes.ContentState

    var body: some View {
        if state.isLive {
            Text(timerInterval: state.startedAt...Date.distantFuture, countsDown: false)
                .multilineTextAlignment(.trailing)
                .frame(maxWidth: 72, alignment: .trailing)
        } else if let ended = state.endedAt {
            Text(ended, style: .time)
        } else {
            Text(StatusLook.label(state.status))
        }
    }
}

/// The Watch's Smart Stack (and other small places): two lines.
private struct SmallActivity: View {
    let attributes: AgentActivityAttributes
    let state: AgentActivityAttributes.ContentState

    var body: some View {
        HStack(spacing: 8) {
            StatusMark(state: state, size: 28)
            VStack(alignment: .leading, spacing: 1) {
                Text(attributes.title).font(.caption.weight(.semibold)).lineLimit(1)
                Group {
                    switch state.status {
                    case "working":
                        if let done = state.tasksDone, let total = state.tasksTotal, total > 0 {
                            Text("Tasks \(done)/\(total)")
                        } else {
                            Text(state.action ?? ActivityLook.actionLabel(state.actionKind))
                        }
                    case "approval": Text(state.isQuestion ? "Needs your answer" : "Needs your approval")
                    default: Text(ActivityLook.outcome(state))
                    }
                }
                .font(.caption2)
                .foregroundStyle(.secondary)
                .lineLimit(1)
            }
            Spacer(minLength: 0)
        }
        .padding(8)
    }
}

// MARK: - Previews

extension AgentActivityAttributes {
    fileprivate static let preview = AgentActivityAttributes(
        threadId: "t1", title: "Fix the failing parser tests", project: "domovoi", serverId: nil, model: "Opus 4.7", branch: "fix/parser"
    )
}

extension AgentActivityAttributes.ContentState {
    private static let started = Date.now.addingTimeInterval(-252).timeIntervalSince1970 * 1000

    fileprivate static let working = Self(status: "working", detail: "", startedAtMs: started, actionKind: "command", action: "cargo test -p domovoi-model")
    fileprivate static let tasks = Self(
        status: "working", detail: "", startedAtMs: started, actionKind: "file_change", action: "crates/domovoi-model/src/typed/live.rs",
        tasksDone: 3, tasksTotal: 7, taskCurrent: "Teach the parser nested lists"
    )
    fileprivate static let approval = Self(status: "approval", detail: "git push origin fix/parser", startedAtMs: started, request: "approval")
    fileprivate static let question = Self(status: "approval", detail: "Should the old parser stay behind a flag?", startedAtMs: started, request: "question")
    fileprivate static let finished = Self(
        status: "completed", detail: "Finished", startedAtMs: started, endedAtMs: Date.now.timeIntervalSince1970 * 1000,
        tasksDone: 7, tasksTotal: 7, summary: "Fixed the three failing tests: nested lists were closed one level early."
    )
    fileprivate static let failed = Self(
        status: "failed", detail: "Failed", startedAtMs: started, endedAtMs: Date.now.timeIntervalSince1970 * 1000, error: "The provider stopped answering (rate limited)."
    )
}

#Preview("Lock Screen", as: .content, using: AgentActivityAttributes.preview) {
    AgentLiveActivity()
} contentStates: {
    AgentActivityAttributes.ContentState.working
    AgentActivityAttributes.ContentState.tasks
    AgentActivityAttributes.ContentState.approval
    AgentActivityAttributes.ContentState.question
    AgentActivityAttributes.ContentState.finished
    AgentActivityAttributes.ContentState.failed
}

#Preview("Island expanded", as: .dynamicIsland(.expanded), using: AgentActivityAttributes.preview) {
    AgentLiveActivity()
} contentStates: {
    AgentActivityAttributes.ContentState.tasks
    AgentActivityAttributes.ContentState.approval
    AgentActivityAttributes.ContentState.finished
}

#Preview("Island compact", as: .dynamicIsland(.compact), using: AgentActivityAttributes.preview) {
    AgentLiveActivity()
} contentStates: {
    AgentActivityAttributes.ContentState.working
    AgentActivityAttributes.ContentState.tasks
    AgentActivityAttributes.ContentState.approval
    AgentActivityAttributes.ContentState.finished
}
