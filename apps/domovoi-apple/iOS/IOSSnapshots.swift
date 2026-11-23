import App
import Combine
import SwiftUI

/// Debug aid for the simulator: with `DOMOVOI_WALK=1` in the launch environment
/// (`SIMCTL_CHILD_DOMOVOI_WALK=1 xcrun simctl launch …`), once connected the app
/// steps through its screens every few seconds and prints `walk: <screen>`
/// before each, so screenshots can be taken from outside.
@MainActor
enum IOSSnapshots {
    static let navigate = PassthroughSubject<[Screen], Never>()
    /// The Scheduled Tasks screen opens its editor with this draft.
    static let openScheduledEditor = PassthroughSubject<ScheduledTaskEdit, Never>()

    static func run(_ core: Core) async {
        let env = ProcessInfo.processInfo.environment
        guard env["DOMOVOI_WALK"] == "1" else { return }
        let pause = Double(env["DOMOVOI_WALK_PAUSE"] ?? "") ?? 4
        if let server = env["DOMOVOI_SERVER"], let code = env["DOMOVOI_PAIRING"] {
            await settle(0.5)
            core.update(.connect(serverUrl: server, pairingCode: code))
        }
        #if DEBUG
        if let screen = env["DOMOVOI_WALK_COMPANION"] {
            let folder = URL.documentsDirectory.appending(path: "companion")
            do { try CompanionGallery.capture(in: folder) }
            catch { print("companion gallery: \(error)") }
            await wait(20) { core.view.connection.state == "connected" }
            let connection = core.view.connection
            guard !connection.activeEnvironmentId.isEmpty else { return }
            if screen == "editor" {
                navigate.send([.settings, .companion(connection.activeEnvironmentId)])
            }
            step("companion-\(screen)")
            return
        }
        #endif
        await wait(60) { core.view.connection.state == "connected" && !core.view.sidebar.projects.isEmpty }
        if env["DOMOVOI_WALK_SIDEBAR_ONLY"] == "1" {
            core.update(.setSidebarGrouping(grouping: env["DOMOVOI_WALK_GROUPING"] ?? "status"))
            for section in core.view.sidebar.sections where !section.expanded { core.update(.toggleSection(section: section.kind)) }
            step("threads")
            return
        }
        step("threads")
        await settle(pause)
        let threads = core.view.sidebar.pickableRows
        guard let thread = env["DOMOVOI_THREAD"].flatMap({ id in threads.first { $0.id == id } }) ?? threads.first else { return }
        core.update(.selectThread(threadId: thread.id))
        navigate.send([.thread(thread.id)])
        step("thread")
        await settle(pause)
        for screen in [Screen.diff, .terminal, .files, .git, .pullRequests, .usage, .settings, .serverSettings, .providers, .scheduledTasks, .access, .maintenance] {
            navigate.send([.thread(thread.id), screen])
            step("\(screen)")
            if screen == .terminal {
                await settle(1.5)
                core.update(.terminalPaste(text: "echo domovoi-ios-$((40+2)) && pwd"))
                core.update(.terminalKey(key: "enter", text: "", ctrl: false, alt: false))
            }
            await settle(pause)
            if screen == .scheduledTasks {
                // A one-thread task: bound to the walk's thread, then left to its first run.
                var draft = ScheduledTaskEditor.new(projectId: thread.projectId)
                draft.name = "Morning check"
                draft.prompt = "Summarize what changed overnight."
                draft.threadMode = "continue"
                draft.threadId = thread.id
                openScheduledEditor.send(draft)
                step("scheduled-editor-thread")
                await settle(pause)
                draft.threadId = ""
                draft.threadMode = "fresh_session"
                openScheduledEditor.send(draft)
                step("scheduled-editor-first-run")
                await settle(pause)
            }
        }
        core.update(.newThread(projectId: thread.projectId))
        navigate.send([.newThread])
        step("new-thread")
        await settle(pause)
        navigate.send([])
        step("done")
    }

    private static func step(_ name: String) {
        print("walk: \(name)")
        fflush(stdout)
    }

    private static func wait(_ seconds: Double, _ condition: () -> Bool) async {
        let deadline = Date().addingTimeInterval(seconds)
        while Date() < deadline, !condition() {
            try? await Task.sleep(for: .milliseconds(100))
        }
    }

    private static func settle(_ seconds: Double) async {
        try? await Task.sleep(for: .seconds(seconds))
    }
}
