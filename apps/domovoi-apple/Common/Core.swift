@preconcurrency import App
import Combine
import Foundation
import Observation
import os
import Shared
@preconcurrency import UserNotifications
#if os(macOS)
import AppKit
#else
import UIKit
#endif

/// Bridges the Rust core: forwards events, runs effects (HTTP, WebSocket,
/// the iroh tunnel, secure storage, notifications) and publishes the view
/// model. The core makes every decision; this class only performs I/O and
/// the timers the core asks for.
///
/// Requests carry a bearer token the core keeps; no cookies are used, so one
/// loopback address can front different remote servers over time.
@MainActor
final class Core: NSObject, ObservableObject, UNUserNotificationCenterDelegate {
    /// The view model, one observed property per part: a view redraws only
    /// when the part it reads changes (a streaming reply redraws the thread,
    /// not the thread list behind it).
    let view: LiveView

    /// The Rust core, on its own queue: nothing of it runs on the main thread.
    private nonisolated let engine: CoreEngine
    /// Debug aid: `DOMOVOI_TRACE_WS=<substring>` prints matching socket frames.
    private let trace = ProcessInfo.processInfo.environment["DOMOVOI_TRACE_WS"]
    private let session: URLSession = {
        let config = URLSessionConfiguration.default
        config.httpShouldSetCookies = false
        config.httpCookieAcceptPolicy = .never
        config.requestCachePolicy = .reloadIgnoringLocalCacheData
        return URLSession(configuration: config)
    }()
    private let store: SecureStore
    private let tunnel = TunnelWorker()
    /// The open (or opening) socket. Its frames are read off the main thread,
    /// where `current` says whether a socket is still this one.
    private var socket: URLSessionWebSocketTask? {
        get { current.task }
        set { current.task = newValue }
    }
    private nonisolated let current = CurrentSocket()
    /// The socket whose handshake is still under way.
    private var opening: URLSessionWebSocketTask?
    private var clock: Timer?
    private var reconnectTask: Task<Void, Never>?
    /// The retry `reconnectTask` waits for.
    private var reconnectGen: UInt64 = 0
    /// When the scheduled retry happens (the banner counts down to it).
    @Published private(set) var retryAt: Date?
    #if os(iOS)
    /// Time iOS gives a backgrounded app; the socket stays open for it.
    private var backgroundTask: UIBackgroundTaskIdentifier = .invalid
    private var backgroundGrace: Task<Void, Never>?
    #endif
    private var tunnelPoll: Task<Void, Never>?
    /// The core's request for the running tunnel; a start that finishes
    /// after a stop (or after a newer start) is not this one.
    private var tunnelRequest: UInt32?
    private var keepAlive: Timer?
    #if os(macOS)
    /// Reads the descriptor of the server on this Mac every few seconds, for
    /// the core's `watch` (the one running is the latest asked for).
    private var localWatch: Task<Void, Never>?
    #endif
    private var launched = false
    private var dismissedToast: UInt32?
    #if os(macOS)
    enum KeychainState {
        case notChecked, checking, unavailable, ready
    }
    @Published private(set) var keychainState: KeychainState
    private var pendingPairingLink: Event?
    #endif
    /// A notification was tapped: the screen should show this thread even if
    /// it is already the selected one.
    let openedThread = PassthroughSubject<String, Never>()

    override init() {
        let store = SecureStore()
        self.store = store
        #if os(macOS)
        keychainState = store.needsKeychainPreparation ? .notChecked : .ready
        #endif
        engine = CoreEngine(store: store)
        // The core's first view carries every part.
        view = LiveView(engine.first())
        super.init()
        engine.attach(
            perform: { [weak self] request in self?.handle(request.value) },
            show: { [weak self] update in
                self?.view.apply(update.value)
                self?.scheduleReconnectIfNeeded()
            },
            relaunch: { [weak self] in self?.relaunch() })
        #if os(macOS)
        notifications?.delegate = self
        #endif
        tick()
        // Every few seconds: unanswered sends time out on it. The core only
        // renders when something changed (or every 20 s, for relative times).
        let clock = Timer(timeInterval: 5, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.tick() }
        }
        // `.common`: keeps firing while the user scrolls.
        RunLoop.main.add(clock, forMode: .common)
        self.clock = clock
        let center = NotificationCenter.default
        #if os(macOS)
        let active = NSApplication.didBecomeActiveNotification
        let inactive = NSApplication.didResignActiveNotification
        #else
        let active = UIApplication.didBecomeActiveNotification
        let inactive = UIApplication.willResignActiveNotification
        #endif
        center.addObserver(forName: active, object: nil, queue: .main) { [weak self] _ in
            Task { @MainActor in self?.update(.appFocus(focused: true)) }
        }
        center.addObserver(forName: inactive, object: nil, queue: .main) { [weak self] _ in
            Task { @MainActor in self?.update(.appFocus(focused: false)) }
        }
        // The user moved or changed the clock's zone: Usage buckets by local day and hour.
        center.addObserver(forName: .NSSystemTimeZoneDidChange, object: nil, queue: .main) { [weak self] _ in
            Task { @MainActor in self?.sendTimeZone() }
        }
    }

    /// The local zone, for Usage's "Today" and its hours.
    private func sendTimeZone() {
        NSTimeZone.resetSystemTimeZone()
        let zone = TimeZone.current
        update(.setTimeZone(iana: zone.identifier, utcOffsetSecs: Int32(zone.secondsFromGMT())))
    }

    /// Restores saved environments and reconnects to the last one (once:
    /// a window that opens again must not start the connection over).
    func launch() {
        guard !launched else { return }
        #if os(macOS)
        guard keychainState != .checking else { return }
        if keychainState != .ready {
            keychainState = .checking
            Task { [weak self, store] in
                let ready = await Task.detached(priority: .userInitiated) { store.prepareKeychain() }.value
                guard let self else { return }
                self.keychainState = ready ? .ready : .unavailable
                if ready { self.launch() }
            }
            return
        }
        launched = true
        sendTimeZone()
        update(.launch(surface: "desktop", deviceLabel: Host.current().localizedName ?? "Mac", deviceOs: "macOS"))
        if let event = pendingPairingLink {
            pendingPairingLink = nil
            update(event)
        }
        #else
        launched = true
        sendTimeZone()
        update(.launch(surface: "mobile", deviceLabel: UIDevice.current.name, deviceOs: "iOS"))
        #endif
    }

    /// The core was replaced after a panic (see `CoreFfi::take_reset`): the
    /// new one starts as at launch, and reconnects to the saved server.
    private func relaunch() {
        #if os(macOS)
        // The replaced core's request is gone with it: the one launched here asks again.
        localWatch?.cancel()
        localWatch = nil
        #endif
        launched = false
        launch()
        #if os(macOS)
        update(.appFocus(focused: NSApplication.shared.isActive))
        #else
        update(.appFocus(focused: UIApplication.shared.applicationState == .active))
        #endif
    }

    /// Dismisses the toast showing, once: a viewer over the window shows
    /// the same toast, and both time out together.
    func dismissToast(_ id: UInt32) {
        guard view.toast?.id == id, dismissedToast != id else { return }
        dismissedToast = id
        update(.dismissToast)
    }

    private func tick() {
        update(.tick(nowMs: Int64(Date().timeIntervalSince1970 * 1000)))
    }

    /// Hands the event to the core. It is taken in order, on the core's
    /// queue; what it changes shows in `view` a moment later.
    func update(_ event: Event) {
        #if os(macOS)
        guard keychainState == .ready else {
            // A link that opened the app waits for the keychain (`launch`
            // sends it once the keychain is ready).
            switch event {
            case .connectRemote, .openLink: pendingPairingLink = event
            default: break
            }
            return
        }
        #endif
        // swiftlint:disable:next force_try
        engine.send(try! event.bincodeSerialize())
    }

    /// `update`, with `then` run here once the core has taken the event and
    /// the effects it asked for have been carried out.
    func update(_ event: Event, then: @escaping @MainActor @Sendable () -> Void) {
        #if os(macOS)
        guard keychainState == .ready else { return }
        #endif
        // swiftlint:disable:next force_try
        engine.send(try! event.bincodeSerialize(), then: then)
    }

    private func resolve(_ id: UInt32, _ data: [UInt8]) {
        engine.resolve(id, data)
    }

    /// Carries out an effect that belongs on the main actor.
    private func handle(_ request: Request) {
        switch request.effect {
        case .render, .store:
            // The engine's own: a render is read on its queue, the store is
            // answered there without a detour through the main thread.
            break
        case .http(let http):
            Task {
                let result = await perform(http)
                // swiftlint:disable:next force_try
                resolve(request.id, try! result.bincodeSerialize())
            }
        case .socket(let op):
            switch op {
            case .open(let url):
                open(url, requestId: request.id)
            case .send(let text):
                if let trace, text.contains(trace) { print(">> \(text.prefix(300))") }
                socket?.send(.string(text)) { _ in }
            case .close:
                socket?.cancel(with: .normalClosure, reason: nil)
                socket = nil
                opening = nil
                keepAlive?.invalidate()
                keepAlive = nil
            }
        case .notify(let note):
            post(note)
            // swiftlint:disable:next force_try
            resolve(request.id, try! NotifyOutput.posted.bincodeSerialize())
        case .tunnel(let op):
            runTunnel(op, requestId: request.id)
        case .local(let op):
            runLocal(op, requestId: request.id)
        }
    }

    // MARK: Reconnect

    /// Times the retry the core scheduled. Each retry has its own number: a
    /// new one replaces the timer (an attempt that failed at once must wait
    /// its own backoff), and the core ignores a timer for an older one.
    private func scheduleReconnectIfNeeded() {
        let connection = view.connection
        let delay = connection.reconnectInSeconds
        guard connection.state == "reconnecting", delay > 0, !connection.offline else {
            // Connected, trying now, offline, failed or given up.
            reconnectTask?.cancel()
            reconnectTask = nil
            if retryAt != nil { retryAt = nil }
            return
        }
        guard reconnectTask == nil || reconnectGen != connection.retryGen else { return }
        reconnectTask?.cancel()
        let gen = connection.retryGen
        reconnectGen = gen
        retryAt = Date().addingTimeInterval(Double(delay))
        reconnectTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(Int(delay)))
            guard let self, !Task.isCancelled else { return }
            self.reconnectTask = nil
            self.update(.retryTimer(retryGen: gen))
        }
    }

    // MARK: Background

    #if os(iOS)
    /// Leaving the app for a moment (a glance at another app, a lock) keeps
    /// the connection: the socket stays open for the time iOS allows, and is
    /// closed only when that runs out.
    func enterBackground() {
        guard backgroundTask == .invalid else { return }
        backgroundTask = UIApplication.shared.beginBackgroundTask(withName: "Keep the connection") { [weak self] in
            // Out of time: the task must end before this handler returns, or
            // iOS stops the app. The core is told to suspend all the same.
            MainActor.assumeIsolated { self?.endGrace(suspend: true, expired: true) }
        }
        // Within the time iOS actually gives (less when launched in the
        // background), with room for the core to save and close.
        let remaining = UIApplication.shared.backgroundTimeRemaining
        let grace = remaining.isFinite ? max(1, min(25, remaining - 5)) : 25
        backgroundGrace = Task { [weak self] in
            try? await Task.sleep(for: .seconds(grace))
            guard let self, !Task.isCancelled else { return }
            self.endGrace(suspend: true)
        }
        if backgroundTask == .invalid { endGrace(suspend: true) }
    }

    func enterForeground() {
        if backgroundTask != .invalid {
            // Back within the grace period: the connection never stopped.
            endGrace(suspend: false)
            update(.networkChanged)
            return
        }
        // iOS takes a suspended app's listening port: check the tunnel's
        // before the core reuses it.
        Task { [weak self] in
            guard let self else { return }
            if let request = self.tunnelRequest, let status = await self.tunnel.status() {
                self.emitTunnel(.status(connected: status.connected, path: status.path ?? "", rttMs: UInt32(min(max(status.rttMs ?? 0, 0), 60_000)), error: status.error ?? "", listening: status.listening ?? true), request)
            }
            self.update(.resume)
        }
    }

    private func endGrace(suspend: Bool, expired: Bool = false) {
        backgroundGrace?.cancel()
        backgroundGrace = nil
        let task = backgroundTask
        backgroundTask = .invalid
        if expired {
            if task != .invalid { UIApplication.shared.endBackgroundTask(task) }
            update(.suspend)
            return
        }
        let end: @MainActor @Sendable () -> Void = {
            if task != .invalid { UIApplication.shared.endBackgroundTask(task) }
        }
        // Suspending saves drafts and closes the socket: iOS may stop the app
        // as soon as the task ends, so it ends once the core has done both.
        if suspend { update(.suspend, then: end) } else { end() }
    }
    #endif

    // MARK: Tunnel

    private func runTunnel(_ op: TunnelOperation, requestId: UInt32) {
        switch op {
        case .start(let ticket):
            tunnelPoll?.cancel()
            tunnelPoll = nil
            tunnelRequest = requestId
            guard let key = store.deviceKey() else {
                emitTunnel(.failed(reason: "This device's key could not be read from the Keychain. Unlock the device and try again."), requestId)
                return
            }
            tunnel.start(key: key, ticket: ticket) { [weak self] result in
                Task { @MainActor in
                    // Stopped or replaced while starting: the worker's queue
                    // stops it again; nothing to report.
                    guard let self, self.tunnelRequest == requestId else { return }
                    switch result {
                    case .success(let port):
                        self.emitTunnel(.started(port: port), requestId)
                        self.pollTunnel(requestId)
                    case .failure(let error):
                        self.emitTunnel(.failed(reason: error.message), requestId)
                    }
                }
            }
        case .networkChanged:
            tunnel.networkChanged()
        case .stop:
            tunnelPoll?.cancel()
            tunnelPoll = nil
            tunnelRequest = nil
            tunnel.stop()
        }
    }

    private func emitTunnel(_ output: TunnelOutput, _ requestId: UInt32) {
        // swiftlint:disable:next force_try
        resolve(requestId, try! output.bincodeSerialize())
    }

    /// Streams the bridge's path and round-trip time while it runs.
    private func pollTunnel(_ requestId: UInt32) {
        tunnelPoll?.cancel()
        tunnelPoll = Task { [weak self] in
            while !Task.isCancelled {
                guard let self, self.tunnelRequest == requestId else { return }
                let status = await self.tunnel.status()
                guard !Task.isCancelled else { return }
                if let status {
                    self.emitTunnel(.status(connected: status.connected, path: status.path ?? "", rttMs: UInt32(min(max(status.rttMs ?? 0, 0), 60_000)), error: status.error ?? "", listening: status.listening ?? true), requestId)
                }
                try? await Task.sleep(for: .seconds(3))
            }
        }
    }

    // MARK: Local server

    /// The server on this machine: its descriptor file, watched for the
    /// core, and a sign-in request written beside it. Only a Mac has one.
    private func runLocal(_ op: LocalOperation, requestId: UInt32) {
        #if os(macOS)
        switch op {
        case .watch:
            localWatch?.cancel()
            localWatch = Task { [weak self] in
                // What the core was last told; `nil` before the first answer.
                var reported: LocalOutput?
                while !Task.isCancelled {
                    // The file is read off the main thread.
                    let text = await Task.detached(priority: .utility) { localServerDescriptor() }.value
                    // Cancelled while reading: a newer watch (or a new core) took over.
                    guard !Task.isCancelled else { return }
                    let output = LocalOutput.descriptor(text: text.isEmpty ? nil : text)
                    if output != reported {
                        reported = output
                        self?.emitLocal(output, requestId)
                    }
                    try? await Task.sleep(for: .seconds(5))
                }
            }
        case .request:
            Task { [weak self] in
                // `<id> <secret>`, or nothing when no request could be written.
                let made = await Task.detached(priority: .userInitiated) { localServerRequest() }.value
                let parts = made.split(separator: " ", maxSplits: 1, omittingEmptySubsequences: false)
                let output: LocalOutput = parts.count == 2 && !parts[0].isEmpty && !parts[1].isEmpty
                    ? .request(id: String(parts[0]), secret: String(parts[1]))
                    : .unavailable
                self?.emitLocal(output, requestId)
            }
        }
        #else
        // No server runs beside the app here.
        switch op {
        case .watch: emitLocal(.descriptor(text: nil), requestId)
        case .request: emitLocal(.unavailable, requestId)
        }
        #endif
    }

    private func emitLocal(_ output: LocalOutput, _ requestId: UInt32) {
        // swiftlint:disable:next force_try
        resolve(requestId, try! output.bincodeSerialize())
    }

    // MARK: Notifications

    /// Notifications need an app bundle (the CLI smoke tool has none).
    private var notifications: UNUserNotificationCenter? {
        Bundle.main.bundleIdentifier == nil ? nil : UNUserNotificationCenter.current()
    }

    private func post(_ note: NotifyOperation) {
        #if os(iOS)
        // In the foreground, a tap of the phone says the same.
        if UIApplication.shared.applicationState == .active {
            UINotificationFeedbackGenerator().notificationOccurred(note.body == "Failed" ? .error : note.body == "Finished" ? .success : .warning)
        }
        #endif
        guard let center = notifications else { return }
        let content = UNMutableNotificationContent()
        content.title = note.title
        content.body = note.body
        // Like a push: the server, and for an approval its request and the
        // Allow / Decline buttons.
        var info: [String: String] = ["threadId": note.threadId]
        if !note.environmentId.isEmpty { info["environmentId"] = note.environmentId }
        if let request = note.requestId {
            info["requestId"] = request
            content.categoryIdentifier = "APPROVAL"
        }
        content.userInfo = info
        content.sound = .default
        let request = UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil)
        // Asked once, the first time there is something to say; after that
        // only posted (or not, if the answer was no).
        center.getNotificationSettings { settings in
            switch settings.authorizationStatus {
            case .notDetermined:
                center.requestAuthorization(options: [.alert, .sound, .badge]) { granted, _ in
                    if granted { center.add(request) }
                }
            case .denied: break
            default: center.add(request)
            }
        }
    }

    /// A notification was opened: `server` names the server for pushes;
    /// local notifications come from the current one.
    #if os(iOS)
    /// Allow or Decline from an approval's notification: the core queues the
    /// answer and reconnects; a background task keeps the app up long enough
    /// to send it.
    func respondFromNotification(thread: String, server: String?, request: String, decision: String) {
        update(.respondFromNotification(environmentId: server ?? "", threadId: thread, requestId: request, decision: decision))
        if UIApplication.shared.applicationState != .active { enterBackground() }
    }

    /// Allow / Decline from a Live Activity: the thread's pending approval.
    func respondToThread(thread: String, server: String?, decision: String) {
        update(.respondToThread(environmentId: server ?? "", threadId: thread, decision: decision))
        if UIApplication.shared.applicationState != .active { enterBackground() }
    }

    /// A question answered by typing in its notification.
    func answerFromNotification(thread: String, server: String?, request: String, question: String, text: String) {
        update(.answerFromNotification(environmentId: server ?? "", threadId: thread, requestId: request, questionId: question, text: text))
        if UIApplication.shared.applicationState != .active { enterBackground() }
    }
    #endif

    func openNotification(thread: String, server: String?) {
        #if os(macOS)
        NSApp.activate(ignoringOtherApps: true)
        #endif
        // The core holds a tap that arrives before saved servers are loaded.
        update(.openNotification(environmentId: server ?? "", threadId: thread))
        openedThread.send(thread)
    }

    // The completion-handler forms: with the async ones UIKit finishes the
    // response off the main thread and asserts (iOS). On iOS the app
    // delegate receives these (it must be the delegate before launch ends)
    // and hands taps to `openNotification`.
    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse, withCompletionHandler completionHandler: @escaping @Sendable () -> Void) {
        let info = response.notification.request.content.userInfo
        let thread = info["threadId"] as? String
        let server = info["environmentId"] as? String
        DispatchQueue.main.async {
            MainActor.assumeIsolated {
                if let thread { self.openNotification(thread: thread, server: server) }
            }
            completionHandler()
        }
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification, withCompletionHandler completionHandler: @escaping @Sendable (UNNotificationPresentationOptions) -> Void) {
        let thread = notification.request.content.userInfo["threadId"] as? String
        DispatchQueue.main.async {
            MainActor.assumeIsolated { completionHandler(self.presentation(for: thread)) }
        }
    }

    /// In front: no banner for the thread on screen (its own card and the
    /// haptic say it), a banner for any other.
    func presentation(for thread: String?) -> UNNotificationPresentationOptions {
        if let thread, thread == view.thread?.id { return [] }
        return [.banner, .sound]
    }

    // MARK: HTTP

    private func perform(_ request: HttpRequest) async -> HttpResult {
        guard let url = URL(string: request.url) else { return .err(.url("Invalid URL")) }
        var urlRequest = URLRequest(url: url)
        urlRequest.httpMethod = request.method
        urlRequest.timeoutInterval = 30
        for header in request.headers {
            urlRequest.addValue(header.value, forHTTPHeaderField: header.name)
        }
        if !request.body.isEmpty {
            urlRequest.httpBody = Data(request.body)
            if urlRequest.value(forHTTPHeaderField: "Content-Type") == nil {
                urlRequest.setValue("application/json", forHTTPHeaderField: "Content-Type")
            }
        }
        do {
            let (data, response) = try await session.data(for: urlRequest)
            guard let http = response as? HTTPURLResponse else { return .err(.io("Not an HTTP response")) }
            let headers = (http.allHeaderFields as? [String: String] ?? [:]).map { HttpHeader(name: $0.key, value: $0.value) }
            return .ok(HttpResponse(status: UInt16(http.statusCode), headers: headers, body: [UInt8](data)))
        } catch {
            return .err(.io(error.localizedDescription))
        }
    }

    // MARK: WebSocket

    private func emit(_ event: SocketEvent, _ requestId: UInt32) {
        // swiftlint:disable:next force_try
        resolve(requestId, try! event.bincodeSerialize())
    }

    private func open(_ urlString: String, requestId: UInt32) {
        guard let url = URL(string: urlString) else {
            emit(.closed(reason: "Invalid server URL"), requestId)
            return
        }
        socket?.cancel(with: .goingAway, reason: nil)
        keepAlive?.invalidate()
        let task = session.webSocketTask(with: url)
        task.maximumMessageSize = 64 * 1024 * 1024
        socket = task
        opening = task
        task.resume()
        // URLSessionWebSocketTask has no open callback; a ping answered means the
        // handshake (including the ticket check) succeeded.
        task.sendPing { [weak self] error in
            Task { @MainActor in
                guard let self, self.socket === task, self.opening === task else { return }
                self.opening = nil
                if let error {
                    self.socket = nil
                    self.emit(.closed(reason: "Connection failed: \(error.localizedDescription)"), requestId)
                    return
                }
                self.emit(.opened, requestId)
                self.listen(task, requestId: requestId)
                self.startKeepAlive(task, requestId: requestId)
            }
        }
        // A handshake that never completes (a server that stopped answering).
        Task { [weak self] in
            try? await Task.sleep(for: .seconds(20))
            guard let self, self.socket === task, self.opening === task else { return }
            self.opening = nil
            self.socket = nil
            task.cancel(with: .goingAway, reason: nil)
            self.emit(.closed(reason: "The server did not answer."), requestId)
        }
    }

    /// Pings an open socket now and then: a connection that died without a
    /// word (Wi-Fi gone, a NAT that forgot it) is noticed in seconds, not
    /// when TCP gives up.
    private func startKeepAlive(_ task: URLSessionWebSocketTask, requestId: UInt32) {
        keepAlive?.invalidate()
        let timer = Timer(timeInterval: 20, repeats: true) { [weak self] timer in
            nonisolated(unsafe) let timer = timer
            MainActor.assumeIsolated {
                guard let self, self.socket === task else {
                    timer.invalidate()
                    return
                }
                let pong = Task { @MainActor [weak self] in
                    try? await Task.sleep(for: .seconds(10))
                    guard let self, self.socket === task, !Task.isCancelled else { return }
                    self.dropSocket(task, requestId, reason: "The connection stopped answering.")
                }
                task.sendPing { [weak self] error in
                    pong.cancel()
                    // A failed ping is a dead connection, not an answer.
                    guard let error else { return }
                    Task { @MainActor in
                        guard let self, self.socket === task else { return }
                        self.dropSocket(task, requestId, reason: "The connection dropped: \(error.localizedDescription)")
                    }
                }
            }
        }
        // `.common`: keeps pinging while the user scrolls.
        RunLoop.main.add(timer, forMode: .common)
        keepAlive = timer
    }

    private func dropSocket(_ task: URLSessionWebSocketTask, _ requestId: UInt32, reason: String) {
        socket = nil
        keepAlive?.invalidate()
        keepAlive = nil
        task.cancel(with: .goingAway, reason: nil)
        emit(.closed(reason: reason), requestId)
    }

    /// Reads the socket's frames into the core. A frame goes from
    /// URLSession's queue straight to the core's, never through the main
    /// thread; the next one is read once the core has applied it, so a
    /// server that sends faster than the core applies waits in the socket.
    private func listen(_ task: URLSessionWebSocketTask, requestId: UInt32) {
        Self.read(task, requestId, engine, current, trace) { [weak self] reason in
            guard let self, self.socket === task else { return }
            self.socket = nil
            self.emit(.closed(reason: reason), requestId)
        }
    }

    private nonisolated static func read(
        _ task: URLSessionWebSocketTask, _ requestId: UInt32, _ engine: CoreEngine, _ current: CurrentSocket, _ trace: String?,
        failed: @escaping @MainActor @Sendable (String) -> Void
    ) {
        task.receive { result in
            guard current.task === task else { return }
            let next: @Sendable () -> Void = { read(task, requestId, engine, current, trace, failed: failed) }
            let message: @Sendable (String) -> Void = { text in
                if let trace, text.contains(trace) { print("<< \(text.prefix(1000))") }
                // swiftlint:disable:next force_try
                engine.resolve(requestId, try! SocketEvent.message(text: text).bincodeSerialize(), then: next)
            }
            switch result {
            case .success(.string(let text)): message(text)
            case .success(.data(let data)): message(String(decoding: data, as: UTF8.self))
            case .success: next()
            case .failure(let error):
                let reason = error.localizedDescription
                DispatchQueue.main.async { MainActor.assumeIsolated { failed(reason) } }
            }
        }
    }
}

/// The socket in use, readable from the queue its frames arrive on.
final class CurrentSocket: @unchecked Sendable {
    private let lock = NSLock()
    private var held: URLSessionWebSocketTask?

    var task: URLSessionWebSocketTask? {
        get {
            lock.lock()
            defer { lock.unlock() }
            return held
        }
        set {
            lock.lock()
            held = newValue
            lock.unlock()
        }
    }
}

/// The Rust core on a queue of its own. Every call into it (an event, an
/// effect's answer, reading the view) runs there, one at a time and in the
/// order sent, so the main thread never runs or waits for the core: a long
/// thread's frame is megabytes of JSON to parse and a view to build.
///
/// The core's effects are handed on from here: the store is answered on this
/// queue, a render reads the view here, and the rest go to the main actor in
/// the order the core asked for them.
final class CoreEngine: @unchecked Sendable {
    private let queue = DispatchQueue(label: "domovoi.core", qos: .userInteractive)
    /// Offline copies (`cache:` keys, megabytes) are written here, so the
    /// core's queue doesn't wait on the disk. Reads and deletes of them go
    /// through it too, after any write still pending.
    private let cacheQueue = DispatchQueue(label: "domovoi.cache", qos: .utility)
    private let ffi = CoreFfi()
    private let store: SecureStore
    // Only touched on `queue`.
    private var page = ThreadPageState()
    private var viewScheduled = false
    private var perform: (@MainActor @Sendable (Unchecked<Request>) -> Void)?
    private var show: (@MainActor @Sendable (Unchecked<ViewUpdate>) -> Void)?
    private var relaunch: (@MainActor @Sendable () -> Void)?
    // Under `lock`: the changes the main actor has not shown yet.
    private let lock = NSLock()
    private var waiting: ViewUpdate?

    init(store: SecureStore) {
        self.store = store
    }

    /// The core's first view (every part), read before any event.
    func first() -> ViewUpdate {
        queue.sync { read() }
    }

    /// Who carries out the main actor's effects and shows the view. Set once,
    /// before the first event.
    func attach(
        perform: @escaping @MainActor @Sendable (Unchecked<Request>) -> Void,
        show: @escaping @MainActor @Sendable (Unchecked<ViewUpdate>) -> Void,
        relaunch: @escaping @MainActor @Sendable () -> Void
    ) {
        queue.sync {
            self.perform = perform
            self.show = show
            self.relaunch = relaunch
        }
    }

    /// An event for the core. `then` runs on the main actor after the
    /// effects the event asked of it.
    func send(_ event: [UInt8], then: (@MainActor @Sendable () -> Void)? = nil) {
        queue.async {
            let effects = self.ffi.update(data: Data(event))
            self.checkReset()
            self.process(effects)
            if let then {
                DispatchQueue.main.async { MainActor.assumeIsolated { then() } }
            }
        }
    }

    /// An effect's answer, from any thread. `then` runs on the core's queue
    /// once the answer has been applied.
    func resolve(_ id: UInt32, _ output: [UInt8], then: (@Sendable () -> Void)? = nil) {
        queue.async {
            let effects = self.ffi.resolve(id: id, data: Data(output))
            self.checkReset()
            self.process(effects)
            then?()
        }
    }

    private func process(_ effects: Data) {
        dispatchPrecondition(condition: .onQueue(queue))
        // swiftlint:disable:next force_try
        let requests = try! Requests.bincodeDeserialize(input: [UInt8](effects)).value
        for request in requests {
            switch request.effect {
            case .render:
                scheduleView()
            case .store(let op):
                let value: String?
                switch op {
                case .load(let key) where key.hasPrefix("cache:"):
                    value = cacheQueue.sync { store.load(key) }
                case .load(let key): value = store.load(key)
                case .save(let key, let v) where key.hasPrefix("cache:"):
                    cacheQueue.async { [store] in
                        let start = Perf.now()
                        store.save(key, v)
                        Perf.log("cache save", since: start, detail: "\(v.utf8.count) bytes")
                    }
                    value = nil
                case .save(let key, let v):
                    store.save(key, v)
                    value = nil
                case .delete(let key) where key.hasPrefix("cache:"):
                    // After the saves queued before it: a forgotten server's
                    // copy must not be written back once it is deleted.
                    cacheQueue.async { [store] in _ = store.delete(key) }
                    value = nil
                case .delete(let key):
                    store.delete(key)
                    value = nil
                }
                // swiftlint:disable:next force_try
                let effects = ffi.resolve(id: request.id, data: Data(try! StoreOutput.value(value: value).bincodeSerialize()))
                checkReset()
                process(effects)
            default:
                // One at a time, so the main actor carries them out in the
                // order the core asked, whatever was answered here meanwhile.
                guard let perform else { continue }
                let request = Unchecked(request)
                DispatchQueue.main.async { MainActor.assumeIsolated { perform(request) } }
            }
        }
    }

    /// After a panic the core was replaced: what was built from its deltas
    /// goes (the next delta is the new core's whole view), and the main actor
    /// starts it again.
    private func checkReset() {
        dispatchPrecondition(condition: .onQueue(queue))
        guard ffi.takeReset() else { return }
        page = ThreadPageState()
        scheduleView()
        if let relaunch {
            DispatchQueue.main.async { MainActor.assumeIsolated { relaunch() } }
        }
    }

    /// A burst of renders (a streaming reply, a batch of replies) is read
    /// once: after what is already queued, and at most every 8 ms.
    private func scheduleView() {
        guard !viewScheduled else { return }
        viewScheduled = true
        queue.asyncAfter(deadline: .now() + .milliseconds(8)) { self.deliverView() }
    }

    private func deliverView() {
        // Cleared before the view is read: a render asked for while it is
        // being built gets a read of its own.
        viewScheduled = false
        let update = read()
        lock.lock()
        let posted = waiting != nil
        waiting = waiting.map { $0.followed(by: update) } ?? update
        lock.unlock()
        // A main thread that is behind shows the changes it missed at once.
        guard !posted, let show else { return }
        DispatchQueue.main.async {
            MainActor.assumeIsolated {
                self.lock.lock()
                let update = self.waiting
                self.waiting = nil
                self.lock.unlock()
                if let update { show(Unchecked(update)) }
            }
        }
    }

    /// Reads the core's delta (what changed since the read before), decodes
    /// it and applies the thread page's patch. Every delta is applied, in
    /// order: each one is against the one before.
    private func read() -> ViewUpdate {
        let state = Perf.signposter.beginInterval("read view")
        let start = Perf.now()
        let bytes = ffi.view()
        let built = Perf.now()
        // A panic while building the view: `bytes` is the new core's whole view.
        checkReset()
        // swiftlint:disable:next force_try
        let delta: ViewDelta = try! .bincodeDeserialize(input: [UInt8](bytes))
        let page: ThreadPageView?? = delta.thread.map { self.page.apply($0) }
        let update = ViewUpdate(delta, thread: page)
        Perf.signposter.endInterval("read view", state, "\(bytes.count) bytes")
        Perf.log("read view", since: start, detail: "\(bytes.count) bytes, core \(Perf.ms(start, built)) ms")
        return update
    }
}

/// The core's view model, split so SwiftUI observes each part on its own.
/// `apply` assigns only the parts that changed.
@MainActor
@Observable
final class LiveView {
    private(set) var connection: ConnectionView
    private(set) var toast: ToastView?
    private(set) var sidebar: SidebarView
    private(set) var route: String
    private(set) var thread: ThreadPageView?
    /// The open thread's id, apart from the thread: views that only need
    /// which thread is open don't redraw on every streamed token.
    private(set) var threadId: String?
    /// Retry is off (the provider can't run), apart from the composer:
    /// the transcript doesn't redraw on every keystroke.
    private(set) var retryBlocked: Bool
    /// Counts changes to the thread's rows: the transcript rebuilds its
    /// lines when this moves, instead of comparing every row each pass.
    private(set) var rowsVersion = 0
    /// The thread's rows as the transcript lays them out (steps of a turn
    /// folded into groups).
    private(set) var lines: [TranscriptLine]
    private(set) var composer: ComposerView
    private(set) var panel: PanelView
    private(set) var palette: PaletteView?
    private(set) var settings: SettingsView
    private(set) var issues: IssuesView
    private(set) var pullRequests: PullRequestsView
    private(set) var usage: UsageView
    private(set) var agentSessions: AgentSessionsView
    private(set) var folderBrowser: FolderBrowserView
    /// Mermaid diagrams the server draws (`DiagramPreview`).
    private(set) var diagrams: [DiagramView]

    // Narrow parts, each assigned only when it changes: views that need only
    // these don't redraw on every streamed token (the thread) or keystroke
    // (the composer).
    /// The open thread's title, toolbar and menu state, without its rows.
    private(set) var header: ThreadHeader?
    /// The open thread's approvals and questions waiting for an answer.
    private(set) var pendingRequests: [RequestView] = []
    /// The open thread's secret requests waiting for an answer.
    private(set) var secretRequests: [SecretRequestView] = []
    /// What the menu bar's composer items need.
    private(set) var composerMenu = ComposerMenu()
    private(set) var panelOpen = false
    private(set) var attentionCount = 0

    /// From the core's first view, which carries every part.
    init(_ first: ViewUpdate) {
        guard let connection = first.connection, let toast = first.toast, let sidebar = first.sidebar,
              let route = first.route, let thread = first.thread, let composer = first.composer,
              let panel = first.panel, let palette = first.palette, let settings = first.settings,
              let issues = first.issues, let pullRequests = first.pullRequests, let usage = first.usage,
              let agentSessions = first.agentSessions, let folderBrowser = first.folderBrowser
        else { preconditionFailure("the core's first view must carry every part") }
        self.connection = connection
        self.toast = toast
        self.sidebar = sidebar
        self.route = route
        self.thread = thread
        threadId = thread?.id
        lines = first.lines ?? TranscriptLine.lines(thread?.items ?? [])
        self.composer = composer
        retryBlocked = !composer.blockedReason.isEmpty
        self.panel = panel
        self.palette = palette
        self.settings = settings
        self.issues = issues
        self.pullRequests = pullRequests
        self.usage = usage
        self.agentSessions = agentSessions
        self.folderBrowser = folderBrowser
        diagrams = first.diagrams ?? []
        narrowThread(thread)
        narrowComposer(composer)
        panelOpen = panel.open
        attentionCount = Int(sidebar.threadsNeedingAttention)
    }

    private func narrowThread(_ thread: ThreadPageView?) {
        let header = thread.map(ThreadHeader.init)
        if self.header != header { self.header = header }
        let requests = thread?.pendingRequests ?? []
        if pendingRequests != requests { pendingRequests = requests }
        let secrets = thread?.secretRequests ?? []
        if secretRequests != secrets { secretRequests = secrets }
    }

    private func narrowComposer(_ composer: ComposerView) {
        let menu = ComposerMenu(composer)
        if composerMenu != menu { composerMenu = menu }
    }

    /// Assigns the parts that changed (worked out on the render queue).
    func apply(_ update: ViewUpdate) {
        let state = Perf.signposter.beginInterval("apply view")
        let start = Perf.now()
        defer {
            Perf.signposter.endInterval("apply view", state)
            Perf.log("apply view", since: start)
        }
        if let v = update.connection { connection = v }
        if let v = update.toast { toast = v }
        if let v = update.sidebar {
            sidebar = v
            let attention = Int(v.threadsNeedingAttention)
            if attentionCount != attention { attentionCount = attention }
        }
        if let v = update.route { route = v }
        if let v = update.thread {
            thread = v
            if threadId != v?.id { threadId = v?.id }
            if update.rowsChanged { rowsVersion &+= 1 }
            narrowThread(v)
        }
        if let v = update.lines { lines = v }
        if let v = update.composer {
            composer = v
            let blocked = !v.blockedReason.isEmpty
            if retryBlocked != blocked { retryBlocked = blocked }
            narrowComposer(v)
        }
        if let v = update.panel {
            panel = v
            if panelOpen != v.open { panelOpen = v.open }
        }
        if let v = update.palette { palette = v }
        if let v = update.settings { settings = v }
        if let v = update.issues { issues = v }
        if let v = update.pullRequests { pullRequests = v }
        if let v = update.usage { usage = v }
        if let v = update.agentSessions { agentSessions = v }
        if let v = update.folderBrowser { folderBrowser = v }
        if let v = update.diagrams { diagrams = v }
    }
}

/// The open thread without its rows: what titles, toolbars and menus show.
struct ThreadHeader: Equatable {
    let id: String
    let title: String
    let projectTitle: String
    let branch: String?
    let modelLabel: String
    let cwd: String
    let worktreePath: String?
    /// The context window and the provider's limits, for the header meter.
    let usage: SessionUsageView?
    let canFork: Bool
    let canMergeBack: Bool
    let parentTitle: String?
    let pinned: Bool
    let settled: Bool
    let archived: Bool
    let hasItems: Bool
    let working: Bool
    /// Matches of the open search; `nil` when the find bar is closed.
    let findTotal: UInt32?
    let plan: PlanProgressView?
    let tasks: TaskProgressView?
    /// Subagents and side threads waiting on the user.
    let waitingChildren: [ThreadLinkView]
    let artifactCount: UInt32

    init(_ thread: ThreadPageView) {
        id = thread.id
        title = thread.title
        projectTitle = thread.projectTitle
        branch = thread.branch
        modelLabel = thread.modelLabel
        cwd = thread.cwd
        worktreePath = thread.worktreePath
        usage = thread.usage
        canFork = thread.canFork
        canMergeBack = thread.canMergeBack
        parentTitle = thread.parent?.title
        pinned = thread.pinned
        settled = thread.settled
        archived = thread.archived
        hasItems = !thread.items.isEmpty
        working = thread.working
        findTotal = thread.find?.total
        plan = thread.plan
        tasks = thread.tasks
        waitingChildren = thread.waitingChildren
        artifactCount = thread.artifactCount
    }

    /// "Project · branch · model".
    var subtitle: String {
        [projectTitle, branch ?? "", modelLabel].filter { !$0.isEmpty }.joined(separator: " · ")
    }
}

/// What the menu bar's composer items need, apart from the draft.
struct ComposerMenu: Equatable {
    var interactionMode = ""
    var canRaiseEffort = false
    var canLowerEffort = false
    var hasModels = false
    var runtimeModes: [ChoiceView] = []
    var runtimeMode = ""
    var canStop = false

    init() {}

    init(_ composer: ComposerView) {
        interactionMode = composer.interactionMode
        // As `ModeInfo.effort` steps: from none chosen, either end is a step.
        let efforts = composer.efforts
        let current = efforts.firstIndex { $0.value == composer.selectedEffort }
        canRaiseEffort = !efforts.isEmpty && current.map { $0 + 1 < efforts.count } ?? !efforts.isEmpty
        canLowerEffort = !efforts.isEmpty && current.map { $0 > 0 } ?? !efforts.isEmpty
        hasModels = !(composer.modelGroups.isEmpty && composer.favoriteModels.isEmpty && composer.modelQuery.isEmpty)
        runtimeModes = composer.runtimeModes
        runtimeMode = composer.runtimeMode
        canStop = composer.canStop
    }
}

/// A value handed between the main actor and the render queue, which use
/// it one at a time.
struct Unchecked<T>: @unchecked Sendable {
    let value: T
    init(_ value: T) { self.value = value }
}

/// The parts of the view model that changed since the last frame; `nil`
/// keeps a part as it is.
struct ViewUpdate {
    var connection: ConnectionView?
    var toast: ToastView??
    var sidebar: SidebarView?
    var route: String?
    var thread: ThreadPageView??
    var composer: ComposerView?
    var panel: PanelView?
    var palette: PaletteView??
    var settings: SettingsView?
    var issues: IssuesView?
    var pullRequests: PullRequestsView?
    var usage: UsageView?
    var agentSessions: AgentSessionsView?
    var folderBrowser: FolderBrowserView?
    var diagrams: [DiagramView]?
    /// The thread's rows changed (not only its header).
    var rowsChanged = false
    /// The transcript's lines, when the rows changed: built with the page,
    /// off the main thread.
    var lines: [TranscriptLine]?

    /// This update with a later one on top: each part is the later one's
    /// where it changed again.
    func followed(by later: ViewUpdate) -> ViewUpdate {
        var out = self
        if let v = later.connection { out.connection = v }
        if let v = later.toast { out.toast = v }
        if let v = later.sidebar { out.sidebar = v }
        if let v = later.route { out.route = v }
        if let v = later.thread { out.thread = v }
        if let v = later.composer { out.composer = v }
        if let v = later.panel { out.panel = v }
        if let v = later.palette { out.palette = v }
        if let v = later.settings { out.settings = v }
        if let v = later.issues { out.issues = v }
        if let v = later.pullRequests { out.pullRequests = v }
        if let v = later.usage { out.usage = v }
        if let v = later.agentSessions { out.agentSessions = v }
        if let v = later.folderBrowser { out.folderBrowser = v }
        if let v = later.diagrams { out.diagrams = v }
        if let v = later.lines { out.lines = v }
        out.rowsChanged = rowsChanged || later.rowsChanged
        return out
    }

    /// The core's delta, with the thread page it brings the transcript to
    /// (`nil`: the page did not change).
    init(_ delta: ViewDelta, thread: ThreadPageView??) {
        connection = delta.connection
        toast = delta.toast.map(\.toast)
        sidebar = delta.sidebar
        route = delta.route
        self.thread = thread
        switch delta.thread {
        case .closed: rowsChanged = true
        case .page(let patch): rowsChanged = patch.order != nil || !patch.rows.isEmpty
        case nil: rowsChanged = false
        }
        if rowsChanged, let page = thread { lines = TranscriptLine.lines(page?.items ?? []) }
        composer = delta.composer
        panel = delta.panel
        palette = delta.palette.map(\.palette)
        settings = delta.settings
        issues = delta.issues
        pullRequests = delta.pullRequests
        usage = delta.usage
        agentSessions = delta.agentSessions
        folderBrowser = delta.folderBrowser
        diagrams = delta.diagrams
    }
}

/// The thread page as the core's deltas have built it: the header and rows
/// last sent, and where each row is.
struct ThreadPageState {
    private(set) var page: ThreadPageView?
    private var index: [String: Int] = [:]

    /// Applies a change and returns the page it leads to.
    mutating func apply(_ change: ThreadChange) -> ThreadPageView? {
        switch change {
        case .closed:
            page = nil
            index = [:]
        case .page(let patch):
            // The core sends a new page with its header; without one there is
            // nothing to patch, and the page stays as it is.
            guard var next = patch.header ?? page else {
                print("domovoi: a thread patch came without a page to patch; kept the page")
                return page
            }
            var items = page?.items ?? []
            if let order = patch.order {
                var byId = Dictionary(items.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
                for row in patch.rows { byId[row.id] = row }
                // Each id once, as the core keeps them: the transcript's lazy
                // stack draws rows with the same id on top of each other.
                var seen = Set<String>()
                items = order.compactMap { id in seen.insert(id).inserted ? byId[id] : nil }
                index = Dictionary(items.enumerated().map { ($1.id, $0) }, uniquingKeysWith: { first, _ in first })
            } else {
                for row in patch.rows {
                    if let i = index[row.id] { items[i] = row }
                }
            }
            next.items = items
            page = next
        }
        return page
    }
}

/// A line of the transcript: a row on its own, or a turn's steps folded
/// under one summary.
enum TranscriptLine: Identifiable {
    case item(ItemView)
    case group(id: String, summary: String, items: [ItemView])

    var id: String {
        switch self {
        case .item(let item): return item.id
        case .group(let id, _, _): return "group-\(id)"
        }
    }

    static func lines(_ items: [ItemView]) -> [TranscriptLine] {
        var out: [TranscriptLine] = []
        var i = 0
        while i < items.count {
            let group = items[i].group
            if group.isEmpty {
                out.append(.item(items[i]))
                i += 1
                continue
            }
            var members: [ItemView] = []
            while i < items.count, items[i].group == group {
                members.append(items[i])
                i += 1
            }
            out.append(.group(id: group, summary: members.first?.groupSummary ?? "", items: members))
        }
        return out
    }
}

extension SidebarView {
    /// Every thread row the list holds, whichever its grouping (a collapsed
    /// section holds none).
    var listedRows: [ThreadRowView] { sections.flatMap(\.rows) + projects.flatMap { $0.threads + $0.earlier } + snoozed }

    /// Threads to pick one from (smoke checks, snapshots): those listed,
    /// then the ones most worth a glance.
    var pickableRows: [ThreadRowView] {
        let listed = listedRows
        let ids = Set(listed.map(\.id))
        return listed + surfaceThreads.filter { !ids.contains($0.id) }
    }
}

/// Timing for the paths that can make the app feel slow: intervals in
/// Instruments (os_signpost, subsystem com.crayen.domovoi, category Perf),
/// and in debug builds a console line for any step over a frame.
enum Perf {
    static let signposter = OSSignposter(subsystem: "com.crayen.domovoi", category: "Perf")
    #if DEBUG
    private static let logger = Logger(subsystem: "com.crayen.domovoi", category: "Perf")
    #endif

    static func now() -> UInt64 { DispatchTime.now().uptimeNanoseconds }

    static func ms(_ from: UInt64, _ to: UInt64) -> String {
        String(format: "%.1f", Double(to &- from) / 1_000_000)
    }

    /// Logs `what` when it took longer than a frame (16 ms), debug builds only.
    static func log(_ what: String, since start: UInt64, detail: String = "") {
        #if DEBUG
        let end = now()
        guard end &- start > 16_000_000 else { return }
        logger.notice("slow \(what, privacy: .public): \(ms(start, end), privacy: .public) ms \(detail, privacy: .public)")
        #endif
    }
}
