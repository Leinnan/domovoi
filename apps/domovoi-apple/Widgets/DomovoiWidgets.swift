import ActivityKit
import SwiftUI
import WidgetKit

@main
struct DomovoiWidgets: WidgetBundle {
    var body: some Widget {
        AgentLiveActivity()
        ThreadsWidget()
    }
}

// MARK: - Status look

enum StatusLook {
    static func icon(_ status: String) -> String {
        switch status {
        case "working": return "circle.dotted"
        case "approval": return "hand.raised.fill"
        case "failed": return "exclamationmark.triangle.fill"
        case "completed": return "checkmark.circle.fill"
        default: return "circle"
        }
    }

    static func color(_ status: String) -> Color {
        switch status {
        case "working": return .blue
        case "approval": return .orange
        case "failed": return .red
        case "completed": return .green
        default: return .secondary
        }
    }

    static func label(_ status: String) -> String {
        switch status {
        case "working": return "Working"
        case "approval": return "Needs you"
        case "failed": return "Failed"
        case "completed": return "Done"
        default: return "Idle"
        }
    }
}

/// `domovoi://thread?id=…&server=…`: the server too, so a tap after switching
/// servers opens the thread where it lives.
private func threadURL(_ id: String, server: String? = nil) -> URL? {
    var c = URLComponents()
    c.scheme = "domovoi"
    c.host = "thread"
    c.queryItems = [URLQueryItem(name: "id", value: id)] + (server.map { [URLQueryItem(name: "server", value: $0)] } ?? [])
    return c.url
}

// MARK: - Live Activity

struct AgentLiveActivity: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: AgentActivityAttributes.self) { context in
            LockScreenActivity(attributes: context.attributes, state: context.state)
                .padding(16)
                .activityBackgroundTint(nil)
                .widgetURL(threadURL(context.attributes.threadId, server: context.attributes.serverId))
        } dynamicIsland: { context in
            let state = context.state
            return DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    Image(systemName: StatusLook.icon(state.status)).foregroundStyle(StatusLook.color(state.status)).font(.title2)
                }
                DynamicIslandExpandedRegion(.trailing) {
                    Elapsed(state: state).font(.callout.monospacedDigit()).foregroundStyle(.secondary)
                }
                DynamicIslandExpandedRegion(.center) {
                    Text(context.attributes.title).font(.headline).lineLimit(1)
                }
                DynamicIslandExpandedRegion(.bottom) {
                    VStack(alignment: .leading, spacing: 8) {
                        Text(state.detail.isEmpty ? StatusLook.label(state.status) : state.detail)
                            .font(.callout)
                            .foregroundStyle(.secondary)
                            .lineLimit(2)
                        if state.status == "approval" { ApprovalButtons(attributes: context.attributes) }
                    }
                }
            } compactLeading: {
                Image(systemName: StatusLook.icon(state.status)).foregroundStyle(StatusLook.color(state.status))
            } compactTrailing: {
                Elapsed(state: state).monospacedDigit().frame(maxWidth: 44)
            } minimal: {
                Image(systemName: StatusLook.icon(state.status)).foregroundStyle(StatusLook.color(state.status))
            }
            .widgetURL(threadURL(context.attributes.threadId, server: context.attributes.serverId))
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

/// Time since the turn started while it runs; the outcome once it's over.
private struct Elapsed: View {
    let state: AgentActivityAttributes.ContentState

    var body: some View {
        if ["working", "approval"].contains(state.status) {
            Text(timerInterval: state.startedAt...Date.distantFuture, countsDown: false)
        } else {
            Text(StatusLook.label(state.status))
        }
    }
}

private struct LockScreenActivity: View {
    let attributes: AgentActivityAttributes
    let state: AgentActivityAttributes.ContentState

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: StatusLook.icon(state.status))
                .font(.title2)
                .foregroundStyle(StatusLook.color(state.status))
            VStack(alignment: .leading, spacing: 4) {
                Text(attributes.title).font(.headline).lineLimit(1)
                Text(state.detail.isEmpty ? StatusLook.label(state.status) : state.detail)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
                if !attributes.project.isEmpty {
                    Text(attributes.project).font(.caption).foregroundStyle(.tertiary)
                }
                if state.status == "approval" { ApprovalButtons(attributes: attributes).padding(.top, 4) }
            }
            Spacer(minLength: 0)
            Elapsed(state: state).font(.callout.monospacedDigit()).foregroundStyle(.secondary).multilineTextAlignment(.trailing).frame(maxWidth: 70)
        }
    }
}

// MARK: - Threads widget

struct ThreadsEntry: TimelineEntry {
    let date: Date
    let snapshot: WidgetSnapshot?
}

struct ThreadsProvider: TimelineProvider {
    func placeholder(in context: Context) -> ThreadsEntry {
        ThreadsEntry(date: .now, snapshot: WidgetSnapshot(server: "MacBook", threads: [
            .init(id: "1", title: "Fix the failing tests", project: "app", status: "working", unread: false),
            .init(id: "2", title: "Review the parser", project: "app", status: "approval", unread: false),
            .init(id: "3", title: "Write release notes", project: "docs", status: "completed", unread: true),
        ], savedAt: .now))
    }

    func getSnapshot(in context: Context, completion: @escaping (ThreadsEntry) -> Void) {
        completion(context.isPreview ? placeholder(in: context) : ThreadsEntry(date: .now, snapshot: WidgetSnapshot.load()))
    }

    func getTimeline(in context: Context, completion: @escaping (Timeline<ThreadsEntry>) -> Void) {
        // The app reloads the timeline whenever its list changes.
        completion(Timeline(entries: [ThreadsEntry(date: .now, snapshot: WidgetSnapshot.load())], policy: .never))
    }
}

struct ThreadsWidget: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: "threads", provider: ThreadsProvider()) { entry in
            ThreadsWidgetView(entry: entry)
                .containerBackground(.fill.tertiary, for: .widget)
        }
        .configurationDisplayName("Threads")
        .description("What your agents are doing, and what waits on you.")
        .supportedFamilies([.systemSmall, .systemMedium, .accessoryRectangular, .accessoryCircular])
    }
}

private struct ThreadsWidgetView: View {
    @Environment(\.widgetFamily) private var family
    let entry: ThreadsEntry

    private var threads: [WidgetSnapshot.Thread] { entry.snapshot?.threads ?? [] }
    /// The one thread a small widget names, and opens.
    private var top: WidgetSnapshot.Thread? { threads.first { $0.status == "approval" } ?? threads.first }
    /// When the app last wrote the list: it can be old if Domovoi hasn't run.
    @ViewBuilder private var updated: some View {
        if let saved = entry.snapshot?.savedAt {
            Text("Updated \(saved, style: .relative) ago").font(.caption2).foregroundStyle(.tertiary).lineLimit(1)
        }
    }
    private var waiting: Int { threads.filter { $0.status == "approval" }.count }
    private var working: Int { threads.filter { $0.status == "working" }.count }

    var body: some View {
        switch family {
        case .accessoryCircular:
            ZStack {
                AccessoryWidgetBackground()
                VStack(spacing: 0) {
                    Image(systemName: waiting > 0 ? "hand.raised.fill" : "circle.dotted")
                    Text("\(waiting > 0 ? waiting : working)").font(.headline)
                }
            }
        case .accessoryRectangular:
            VStack(alignment: .leading, spacing: 2) {
                Text("Domovoi").font(.headline)
                Text(waiting > 0 ? "\(waiting) need\(waiting == 1 ? "s" : "") you" : "\(working) working")
                if let top { Text(top.title).lineLimit(1).foregroundStyle(.secondary) }
            }
        case .systemSmall:
            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    Text(entry.snapshot?.server ?? "Domovoi").font(.caption.bold()).lineLimit(1)
                    Spacer()
                }
                Spacer(minLength: 0)
                counts
                if let top {
                    Text(top.title).font(.caption).lineLimit(2).foregroundStyle(.secondary)
                }
                updated
            }
            .widgetURL(top.flatMap { threadURL($0.id, server: entry.snapshot?.serverId) })
        default:
            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    Text(entry.snapshot?.server ?? "Domovoi").font(.caption.bold())
                    Spacer()
                    counts.font(.caption)
                }
                if threads.isEmpty {
                    Text("Open Domovoi to see your threads here.").font(.caption).foregroundStyle(.secondary)
                }
                ForEach(threads.prefix(4), id: \.id) { thread in
                    Link(destination: threadURL(thread.id, server: entry.snapshot?.serverId) ?? URL(string: "domovoi://")!) {
                        HStack(spacing: 6) {
                            Image(systemName: StatusLook.icon(thread.status)).foregroundStyle(StatusLook.color(thread.status)).font(.caption)
                            Text(thread.title).font(.caption).lineLimit(1)
                            Spacer(minLength: 0)
                            if thread.unread { Circle().fill(.blue).frame(width: 6, height: 6) }
                        }
                    }
                }
                Spacer(minLength: 0)
                updated
            }
        }
    }

    private var counts: some View {
        HStack(spacing: 10) {
            if waiting > 0 { Label("\(waiting)", systemImage: "hand.raised.fill").foregroundStyle(.orange) }
            Label("\(working)", systemImage: "circle.dotted").foregroundStyle(.blue)
        }
        .font(.headline)
    }
}
