import ActivityKit
import App
import SwiftUI
import WidgetKit

/// Keeps what iOS shows outside the app in step with the thread list: Live
/// Activities for turns under way (the server pushes their updates while the
/// app sleeps) and the widgets' snapshot.
@MainActor
final class SystemSurfaces {
    static let shared = SystemSurfaces()
    /// Activities whose push tokens are being watched.
    private var watching: Set<String> = []
    /// Activities about to be ended here (by id).
    private var ending: Set<String> = []
    private var lastSnapshot: [WidgetSnapshot.Thread] = []
    private var lastServer = ""
    private var lastProjects: [WidgetSnapshot.Project] = []

    /// At most this many Live Activities at once (the newest turns).
    private static let maxActivities = 2

    func sync(_ core: Core) {
        let sidebar = core.view.sidebar
        let threads = sidebar.pickableRows.map { ($0.projectTitle, $0) }
        syncActivities(core, threads: threads)
        writeSnapshot(core, threads: threads)
    }

    // MARK: Live Activities

    private func syncActivities(_ core: Core, threads: [(String, ThreadRowView)]) {
        guard ActivityAuthorizationInfo().areActivitiesEnabled else { return }
        let live = threads.filter { ["working", "approval"].contains($0.1.status) }
        let byId = Dictionary(threads.map { ($0.1.id, $0) }, uniquingKeysWith: { a, _ in a })
        // Threads in a collapsed section are not listed but still exist.
        let known = Set(threads.map(\.1.id))
        // Only a loaded list can say a thread is gone (not one still coming).
        let listLoaded = core.view.connection.state == "connected" && !core.view.sidebar.projects.isEmpty
        let current = Activity<AgentActivityAttributes>.activities

        for activity in current where activity.activityState == .active {
            let id = activity.attributes.threadId
            guard let (_, row) = byId[id] else {
                // Archived, deleted, or on another server: it would say
                // "Working" until iOS gave up on it.
                if listLoaded, !known.contains(id) {
                    var state = activity.content.state
                    state.status = "idle"
                    state.detail = "No longer in the list"
                    let gone = ActivityContent(state: state, staleDate: nil)
                    nonisolated(unsafe) let handle = activity
                    Task { await handle.end(gone, dismissalPolicy: .immediate) }
                }
                continue
            }
            var state = activity.content.state
            if ["working", "approval"].contains(row.status) {
                // A new turn: an end waited for is not wanted any more.
                ending.remove(activity.id)
                let request = row.status == "approval" ? (row.question ? "question" : "approval") : nil
                guard state.status != row.status || state.request != request else { continue }
                // The server's pushes say what the turn does; here only its
                // status moves, so a push arriving later is not undone.
                let fresh = AgentActivityAttributes.ContentState.starting(status: row.status, question: row.question, startedAtMs: state.startedAtMs)
                state.status = fresh.status
                state.detail = fresh.detail
                state.request = fresh.request
                if row.workingSinceMs > 0 { state.startedAtMs = Double(row.workingSinceMs) }
                // ActivityKit's handle isn't Sendable; it is only used here.
                let updated = ActivityContent(state: state, staleDate: Self.staleDate())
                nonisolated(unsafe) let handle = activity
                Task { await handle.update(updated) }
            } else {
                // The turn is over: the activity says how, then leaves.
                state.status = row.status
                state.detail = switch row.status {
                case "failed": "Failed"
                case "interrupted": "Stopped"
                default: "Finished"
                }
                state.request = nil
                state.endedAtMs = state.endedAtMs ?? Date.now.timeIntervalSince1970 * 1000
                let ended = ActivityContent(state: state, staleDate: nil)
                nonisolated(unsafe) let handle = activity
                // A server that pushes ends the activity itself, with how the
                // turn came out (its answer, its error): that end is waited
                // for a while, this one only stands in if it never comes.
                let wait: Duration = core.view.settings.remote.pushConfigured ? .seconds(30) : .zero
                guard ending.insert(activity.id).inserted else { continue }
                Task { [weak self] in
                    try? await Task.sleep(for: wait)
                    guard let self, self.ending.remove(handle.id) != nil, handle.activityState == .active else { return }
                    await handle.end(ended, dismissalPolicy: .after(.now.addingTimeInterval(15 * 60)))
                }
            }
        }

        // New turns get an activity (only possible while the app is in front),
        // unless the server is known to have no way to update it later.
        guard UIApplication.shared.applicationState == .active else { return }
        let remote = core.view.settings.remote
        if remote.available, !remote.pushConfigured {
            for activity in current where !watching.contains(activity.id) { watch(activity, core: core) }
            return
        }
        let running = Set(current.filter { $0.activityState == .active }.map(\.attributes.threadId))
        var count = running.count
        for (project, row) in live where !running.contains(row.id) {
            guard count < Self.maxActivities else { break }
            count += 1
            let attributes = AgentActivityAttributes(
                threadId: row.id, title: row.title, project: project, serverId: Self.serverId(core),
                model: row.modelLabel.isEmpty ? nil : row.modelLabel, branch: row.branch
            )
            let started = row.workingSinceMs > 0 ? Double(row.workingSinceMs) : Date.now.timeIntervalSince1970 * 1000
            let state = AgentActivityAttributes.ContentState.starting(status: row.status, question: row.question, startedAtMs: started)
            do {
                let activity = try Activity.request(attributes: attributes, content: ActivityContent(state: state, staleDate: Self.staleDate()), pushType: .token)
                watch(activity, core: core)
            } catch {
                print("live activity: \(error.localizedDescription)")
            }
        }
        // Activities from an earlier launch keep sending their tokens.
        for activity in current where !watching.contains(activity.id) { watch(activity, core: core) }
    }

    /// When an activity nothing has updated says it may be out of date (the
    /// server's pushes move it on as they come).
    private static func staleDate() -> Date { .now.addingTimeInterval(30 * 60) }

    private func watch(_ activity: Activity<AgentActivityAttributes>, core: Core) {
        guard watching.insert(activity.id).inserted else { return }
        let thread = activity.attributes.threadId
        Task { [weak core] in
            for await token in activity.pushTokenUpdates {
                let hex = token.map { String(format: "%02x", $0) }.joined()
                core?.update(.activityToken(threadId: thread, token: hex, environment: AppDelegate.apsEnvironment))
            }
        }
    }

    // MARK: Widgets

    /// The server in use, by its own id (what pushes and deep links name).
    static func serverId(_ core: Core) -> String? {
        let c = core.view.connection
        let id = c.environments.first { $0.id == c.activeEnvironmentId }?.serverId ?? ""
        return id.isEmpty ? nil : id
    }

    private func writeSnapshot(_ core: Core, threads: [(String, ThreadRowView)]) {
        // What needs the user first, so a small widget shows it.
        func rank(_ row: ThreadRowView) -> Int {
            switch row.status {
            case "approval": 0
            case "working": 1
            default: row.unread ? 2 : 3
            }
        }
        let rows = threads
            .filter { !$0.1.archived }
            .enumerated()
            .sorted { (rank($0.element.1), $0.offset) < (rank($1.element.1), $1.offset) }
            .map(\.element)
            .prefix(8)
            .map { WidgetSnapshot.Thread(id: $0.1.id, title: $0.1.title, project: $0.0, status: $0.1.status, unread: $0.1.unread) }
        let server = core.view.connection.environmentLabel
        let projects = core.view.sidebar.projects.filter { !$0.isScratch }.map { WidgetSnapshot.Project(id: $0.id, title: $0.title) }
        guard Array(rows) != lastSnapshot || server != lastServer || projects != lastProjects else { return }
        lastSnapshot = Array(rows)
        lastServer = server
        lastProjects = projects
        WidgetSnapshot(server: server, threads: Array(rows), savedAt: .now, projects: projects, serverId: Self.serverId(core)).save()
        WidgetCenter.shared.reloadAllTimelines()
    }

    // MARK: Share sheet

    /// Text and images shared from another app become a new thread's draft.
    func openSharedDraft(_ core: Core) {
        // Once a server is known: the draft waits in the inbox until then.
        guard !core.view.connection.activeEnvironmentId.isEmpty, !core.view.sidebar.projects.isEmpty,
              let draft = SharedDraft.take() else { return }
        let projects = core.view.sidebar.projects
        let picked = draft.projectId.flatMap { id in projects.first { $0.id == id }?.id }
        core.update(.newThread(projectId: picked ?? core.view.sidebar.selectedProjectId ?? projects.first?.id))
        core.update(.setDraft(text: draft.text))
        for image in draft.images {
            let dataUrl = "data:image/jpeg;base64,\(image.data.base64EncodedString())"
            core.update(.addAttachment(name: image.name, mimeType: "image/jpeg", sizeBytes: UInt64(image.data.count), dataUrl: dataUrl))
        }
        IOSSnapshots.navigate.send([.newThread])
    }
}

/// Runs `SystemSurfaces.sync` when the thread list's statuses change.
struct SystemSurfacesSync: ViewModifier {
    @ObservedObject var core: Core

    private var signature: [String] {
        [core.view.connection.environmentLabel] + core.view.sidebar.pickableRows.map { "\($0.id)|\($0.status)|\($0.question)|\($0.unread)|\($0.title)" }
    }

    func body(content: Content) -> some View {
        content
            .onChange(of: signature) { _, _ in SystemSurfaces.shared.sync(core) }
            // A draft shared before the server's list was there opens now.
            .onChange(of: core.view.sidebar.projects.isEmpty) { _, empty in
                if !empty { SystemSurfaces.shared.openSharedDraft(core) }
            }
            .onAppear { SystemSurfaces.shared.sync(core) }
    }
}

extension Core {
    /// Pull to refresh: sends `event`, then waits until `busy` is false (10
    /// seconds at most), so the spinner stays until the list has loaded
    /// instead of snapping back at once.
    func refresh(_ event: Event, busy: @escaping @MainActor () -> Bool = { false }) async {
        update(event)
        try? await Task.sleep(for: .milliseconds(350))
        let deadline = Date().addingTimeInterval(10)
        while busy(), Date() < deadline {
            try? await Task.sleep(for: .milliseconds(150))
        }
    }
}
