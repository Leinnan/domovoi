import App
import SwiftUI

/// A thread row's marks: a plan waiting to be carried out, its own
/// worktree, and its pull request (`#12`) in the colour of its state.
struct ThreadRowBadges: View {
    let thread: ThreadRowView

    var body: some View {
        if thread.inWorktree {
            Image(systemName: "square.stack.3d.up")
                .help("In its own worktree")
        }
        if thread.planReady {
            Image(systemName: "list.bullet.clipboard")
                .foregroundStyle(Tint.plan)
                .help("Plan ready")
        }
        if let issue = thread.issueBadge {
            Text(issue).badgeCapsule(tint: .secondary)
        }
        if !thread.pullRequest.isEmpty {
            HStack(spacing: Space.xxs) {
                Image(systemName: PullRequestState.icon(thread.pullRequestState))
                Text(thread.pullRequest).monospacedDigit()
            }
            .foregroundStyle(PullRequestState.tint(thread.pullRequestState))
            .badgeCapsule(tint: PullRequestState.tint(thread.pullRequestState))
            .fixedSize()
            .help("Pull request \(thread.pullRequest)\(thread.pullRequestState.isEmpty ? "" : ", \(thread.pullRequestState)")")
        }
    }
}

/// How a pull request's state reads, as GitHub colours it.
enum PullRequestState {
    static func tint(_ state: String) -> Color {
        switch state {
        case "open": Tint.success
        case "merged": .purple
        case "closed": Tint.danger
        default: .secondary
        }
    }

    static func icon(_ state: String) -> String {
        switch state {
        case "merged": "arrow.triangle.merge"
        case "closed": "xmark.circle"
        default: "arrow.triangle.pull"
        }
    }
}

extension ThreadRowView {
    /// A run in plan mode ("Planning 6m").
    var isPlanning: Bool { self.planMode && status == "working" }

    /// The colour of the row's status or age: plan, caution, danger, or quiet.
    var statusTint: Color {
        if statusLabel.isEmpty { return Color.secondary.opacity(0.7) }
        if isPlanning { return Tint.plan }
        return status == "failed" ? Tint.danger : status == "approval" ? Tint.caution : .secondary
    }

    /// The row's marks for VoiceOver ("pull request #12 merged").
    var spokenBadges: String {
        [planReady && statusLabel != "Plan ready" ? "Plan ready" : "",
         inWorktree ? "in its own worktree" : "",
         pullRequest.isEmpty ? "" : "pull request \(pullRequest)\(pullRequestState.isEmpty ? "" : " \(pullRequestState)")"]
            .filter { !$0.isEmpty }.joined(separator: ", ")
    }
}

extension View {
    /// A run in plan mode: its status ("Planning 6m") as a plan-coloured pill.
    @ViewBuilder
    func planningPill(_ thread: ThreadRowView) -> some View {
        if thread.isPlanning { badgeCapsule(tint: Tint.plan) } else { self }
    }
}
