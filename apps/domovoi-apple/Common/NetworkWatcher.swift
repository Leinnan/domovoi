import Foundation
import Network

/// Tells the core when the device's network changes (Wi-Fi to cellular, a
/// network coming back), so the iroh tunnel re-probes paths and a waiting
/// reconnect happens at once.
///
/// `last` and `lastInterfaces` are only touched on the monitor's serial
/// queue, and `start` runs on the main actor, hence `@unchecked Sendable`.
final class NetworkWatcher: @unchecked Sendable {
    private let monitor = NWPathMonitor()
    private var last: NWPath.Status?
    private var lastInterfaces: [NWInterface.InterfaceType] = []
    private var started = false

    /// `online` hears whether there is a network path at all (first report
    /// included); `changed` hears each change after the first.
    private var online: (@MainActor (Bool) -> Void)?

    func start(online: @escaping @MainActor (Bool) -> Void = { _ in }, _ changed: @escaping @MainActor () -> Void) {
        guard !started else { return }
        started = true
        self.online = online
        monitor.pathUpdateHandler = { [weak self] path in
            guard let self else { return }
            let interfaces = path.availableInterfaces.map(\.type)
            let wasSatisfied = self.last.map { $0 == .satisfied }
            defer {
                self.last = path.status
                self.lastInterfaces = interfaces
            }
            let isSatisfied = path.status == .satisfied
            if wasSatisfied != isSatisfied {
                Task { @MainActor in online(isSatisfied) }
            }
            // The first report is the starting state, not a change.
            guard self.last != nil, path.status != self.last || interfaces != self.lastInterfaces else { return }
            Task { @MainActor in changed() }
        }
        monitor.start(queue: queue)
    }

    private let queue = DispatchQueue(label: "com.crayen.domovoi.network")

    /// Says again whether there's a network, from the path as it is now:
    /// a change while the app was suspended may never have been reported,
    /// leaving "No network" up with the network long back.
    func refresh() {
        guard started, let online else { return }
        queue.async { [weak self] in
            guard let self else { return }
            let isSatisfied = self.monitor.currentPath.status == .satisfied
            self.last = self.monitor.currentPath.status
            Task { @MainActor in online(isSatisfied) }
        }
    }
}
