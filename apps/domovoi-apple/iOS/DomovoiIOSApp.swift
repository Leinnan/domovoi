import App
import SwiftUI
import UserNotifications

@main
struct DomovoiIOSApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @StateObject private var core = Core()
    @Environment(\.scenePhase) private var scenePhase
    @State private var network = NetworkWatcher()

    var body: some Scene {
        WindowGroup {
            RootView(core: core)
                .environment(\.core, core)
                .modifier(SystemSurfacesSync(core: core))
                .onOpenURL { url in
                    guard url.scheme == "domovoi" else { return }
                    // `domovoi://thread?id=…` from a widget or a Live Activity.
                    if url.host == "thread" {
                        let items = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems ?? []
                        if let id = items.first(where: { $0.name == "id" })?.value {
                            core.openNotification(thread: id, server: items.first { $0.name == "server" }?.value)
                        }
                        return
                    }
                    // `domovoi://connect?ticket=…&code=…` from the Camera app's
                    // scan or a shared link: the core asks before connecting.
                    core.update(.openLink(link: url.absoluteString))
                }
                .linkRequestDialog(core: core)
                .task {
                    core.launch()
                    network.start(online: { core.update(.networkStatus(online: $0)) }) { core.update(.networkChanged) }
                    delegate.attach(core)
                }
        }
        // A hardware keyboard on iPad: the Thread menu (hold ⌘ to see it).
        .commands {
            CommandMenu("Thread") {
                // The palette sheet belongs to the app proper (MainStack).
                Button("Command Palette") { core.update(.openPalette) }
                    .keyboardShortcut("k")
                    .disabled(!["connected", "reconnecting"].contains(core.view.connection.state))
                Button("Find in Thread") {
                    if core.view.thread?.find != nil { FindFocus.shared.revision += 1 } else { core.update(.findInThread(query: "")) }
                }
                .keyboardShortcut("f")
                .disabled(core.view.thread == nil)
                Button("Find Next") { core.update(.findNext(forward: true)) }
                    .keyboardShortcut("g")
                    .disabled((core.view.thread?.find?.total ?? 0) == 0)
                Button("Find Previous") { core.update(.findNext(forward: false)) }
                    .keyboardShortcut("g", modifiers: [.command, .shift])
                    .disabled((core.view.thread?.find?.total ?? 0) == 0)
            }
        }
        .onChange(of: scenePhase) { _, phase in
            switch phase {
            case .background:
                QuickActions.update(from: core)
                core.enterBackground()
            case .active:
                network.refresh()
                core.enterForeground()
                SystemSurfaces.shared.openSharedDraft(core)
                SystemSurfaces.shared.sync(core)
                // Opening the app answers the badge.
                UNUserNotificationCenter.current().setBadgeCount(0)
            default: break
            }
        }
    }
}

/// Receives the APNs device token and notification taps. It is the
/// notification center's delegate from launch (Apple requires that, or a
/// tap that launches the app is lost); taps wait until the core is there.
@MainActor
final class AppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    private var core: Core?
    private var pendingToken: String?
    private var pendingTaps: [(thread: String, server: String?)] = []
    private var pendingResponses: [(thread: String, server: String?, request: String, decision: String)] = []
    private var pendingAnswers: [(thread: String, server: String?, request: String, question: String, text: String)] = []
    /// A home-screen quick action that launched the app, for when the core is there.
    var pendingShortcut: UIApplicationShortcutItem?

    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        let center = UNUserNotificationCenter.current()
        center.delegate = self
        // Approvals can be answered from the notification. Allow asks for the
        // device to be unlocked first; neither opens the app.
        let allow = UNNotificationAction(identifier: Self.allowAction, title: "Allow", options: [.authenticationRequired])
        let session = UNNotificationAction(identifier: Self.allowSessionAction, title: "Allow for Session", options: [.authenticationRequired])
        let decline = UNNotificationAction(identifier: Self.declineAction, title: "Decline", options: [.destructive])
        // A question with one typed answer: replied to from the notification.
        let answer = UNTextInputNotificationAction(identifier: Self.answerAction, title: "Answer", options: [.authenticationRequired], textInputButtonTitle: "Send", textInputPlaceholder: "Your answer")
        center.setNotificationCategories([
            UNNotificationCategory(identifier: "APPROVAL", actions: [allow, session, decline], intentIdentifiers: []),
            UNNotificationCategory(identifier: "QUESTION", actions: [answer], intentIdentifiers: []),
        ])
        return true
    }

    nonisolated static let allowAction = "ALLOW"
    nonisolated static let declineAction = "DECLINE"
    nonisolated static let allowSessionAction = "ALLOW_SESSION"
    nonisolated static let answerAction = "ANSWER"

    func application(_ application: UIApplication, configurationForConnecting connectingSceneSession: UISceneSession, options: UIScene.ConnectionOptions) -> UISceneConfiguration {
        if let item = options.shortcutItem { pendingShortcut = item }
        let config = UISceneConfiguration(name: nil, sessionRole: connectingSceneSession.role)
        config.delegateClass = SceneDelegate.self
        return config
    }

    /// A quick action: a recent thread, or a new one.
    func perform(_ item: UIApplicationShortcutItem) {
        guard let core else {
            pendingShortcut = item
            return
        }
        QuickActions.perform(item, core: core)
    }

    /// Debug builds talk to APNs' sandbox, release builds to production: what
    /// decides is the provisioning profile the app was signed with, not the
    /// build configuration. Simulators have no profile and use the sandbox.
    nonisolated static let apsEnvironment: String = {
        guard let url = Bundle.main.url(forResource: "embedded", withExtension: "mobileprovision"),
              let data = try? Data(contentsOf: url),
              let text = String(data: data, encoding: .isoLatin1),
              let range = text.range(of: "<key>aps-environment</key>")
        else { return "sandbox" }
        let rest = text[range.upperBound...]
        guard let open = rest.range(of: "<string>"), let close = rest.range(of: "</string>"), open.upperBound <= close.lowerBound else { return "sandbox" }
        return rest[open.upperBound..<close.lowerBound] == "production" ? "production" : "sandbox"
    }()

    func attach(_ core: Core) {
        self.core = core
        ApprovalIntentHandler.respond = { [weak core] thread, server, decision in
            core?.respondToThread(thread: thread, server: server.isEmpty ? nil : server, decision: decision)
        }
        if let token = pendingToken {
            core.update(.pushToken(token: token, environment: Self.apsEnvironment))
            pendingToken = nil
        }
        for tap in pendingTaps {
            core.openNotification(thread: tap.thread, server: tap.server)
        }
        pendingTaps.removeAll()
        for r in pendingResponses {
            core.respondFromNotification(thread: r.thread, server: r.server, request: r.request, decision: r.decision)
        }
        pendingResponses.removeAll()
        for a in pendingAnswers {
            core.answerFromNotification(thread: a.thread, server: a.server, request: a.request, question: a.question, text: a.text)
        }
        pendingAnswers.removeAll()
        if let item = pendingShortcut {
            pendingShortcut = nil
            QuickActions.perform(item, core: core)
        }
    }

    /// Registers for pushes when notifications are already allowed (no
    /// prompt); the first ask waits for a reason (the first thread started).
    nonisolated static func registerIfAllowed() {
        UNUserNotificationCenter.current().getNotificationSettings { settings in
            guard settings.authorizationStatus == .authorized || settings.authorizationStatus == .provisional else { return }
            DispatchQueue.main.async { UIApplication.shared.registerForRemoteNotifications() }
        }
    }

    /// Asks for permission (once; later calls just register), then registers for pushes.
    nonisolated static func requestPush() {
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound, .badge]) { granted, _ in
            guard granted else { return }
            DispatchQueue.main.async { UIApplication.shared.registerForRemoteNotifications() }
        }
    }

    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        let hex = deviceToken.map { String(format: "%02x", $0) }.joined()
        if let core { core.update(.pushToken(token: hex, environment: Self.apsEnvironment)) } else { pendingToken = hex }
    }

    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: Error) {
        // Unsigned simulator builds have no aps-environment entitlement.
        print("push registration failed: \(error.localizedDescription)")
    }

    // The completion-handler forms: with the async ones UIKit finishes the
    // response off the main thread and asserts.
    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse, withCompletionHandler completionHandler: @escaping @Sendable () -> Void) {
        let info = response.notification.request.content.userInfo
        let thread = info["threadId"] as? String
        // Pushes name the server (`environmentId`); local notifications don't.
        let server = info["environmentId"] as? String
        let request = info["requestId"] as? String
        let question = info["questionId"] as? String
        let typed = (response as? UNTextInputNotificationResponse)?.userText
        let action = response.actionIdentifier
        DispatchQueue.main.async {
            MainActor.assumeIsolated {
                guard let thread else { return }
                if let request, let question, let typed, action == Self.answerAction {
                    if let core = self.core {
                        core.answerFromNotification(thread: thread, server: server, request: request, question: question, text: typed)
                    } else {
                        self.pendingAnswers.append((thread, server, request, question, typed))
                    }
                    return
                }
                if let request, [Self.allowAction, Self.allowSessionAction, Self.declineAction].contains(action) {
                    let decision = action == Self.allowAction ? "accept" : action == Self.allowSessionAction ? "acceptForSession" : "decline"
                    if let core = self.core {
                        core.respondFromNotification(thread: thread, server: server, request: request, decision: decision)
                    } else {
                        self.pendingResponses.append((thread, server, request, decision))
                    }
                    return
                }
                if let core = self.core { core.openNotification(thread: thread, server: server) } else { self.pendingTaps.append((thread, server)) }
            }
            // The answer goes out under a background task of its own.
            completionHandler()
        }
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification, withCompletionHandler completionHandler: @escaping @Sendable (UNNotificationPresentationOptions) -> Void) {
        let thread = notification.request.content.userInfo["threadId"] as? String
        DispatchQueue.main.async {
            MainActor.assumeIsolated { completionHandler(self.core?.presentation(for: thread) ?? [.banner, .sound]) }
        }
    }
}

/// Receives quick actions while the app runs.
final class SceneDelegate: NSObject, UIWindowSceneDelegate {
    func windowScene(_ windowScene: UIWindowScene, performActionFor shortcutItem: UIApplicationShortcutItem, completionHandler: @escaping (Bool) -> Void) {
        MainActor.assumeIsolated {
            (UIApplication.shared.delegate as? AppDelegate)?.perform(shortcutItem)
        }
        completionHandler(true)
    }
}

/// Home-screen quick actions: a new thread and the three most recent.
@MainActor
enum QuickActions {
    static func update(from core: Core) {
        let sidebar = core.view.sidebar
        let recent = sidebar.surfaceThreads.prefix(3)
        var items = [UIApplicationShortcutItem(type: "new", localizedTitle: "New Thread", localizedSubtitle: nil, icon: UIApplicationShortcutIcon(systemImageName: "square.and.pencil"), userInfo: nil)]
        for thread in recent {
            let project = thread.projectTitle.isEmpty ? nil : thread.projectTitle
            items.append(UIApplicationShortcutItem(
                type: "thread", localizedTitle: thread.title, localizedSubtitle: project,
                icon: UIApplicationShortcutIcon(systemImageName: "bubble.left.and.text.bubble.right"),
                userInfo: ["threadId": thread.id as NSString, "server": (SystemSurfaces.serverId(core) ?? "") as NSString]
            ))
        }
        UIApplication.shared.shortcutItems = items
    }

    static func perform(_ item: UIApplicationShortcutItem, core: Core) {
        switch item.type {
        case "thread":
            guard let thread = item.userInfo?["threadId"] as? String else { return }
            // Like a notification: on its server, once that server's list is
            // there (switching first if it's another one).
            let server = (item.userInfo?["server"] as? String).flatMap { $0.isEmpty ? nil : $0 }
            core.openNotification(thread: thread, server: server)
        default:
            core.update(.newThread(projectId: core.view.sidebar.selectedProjectId))
            IOSSnapshots.navigate.send([.newThread])
        }
    }
}
