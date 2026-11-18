import Foundation
#if canImport(ActivityKit)
import ActivityKit
#endif

/// What the iPhone app and its extensions (widgets, Live Activities, the
/// share sheet) share: the Live Activity's shape, and files in the App Group.
enum AppGroup {
    static let id = "group.com.crayen.domovoi"

    static var container: URL? {
        FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: id)
    }
}

#if canImport(ActivityKit)
/// A thread's turn on the Lock Screen and in the Dynamic Island.
struct AgentActivityAttributes: ActivityAttributes {
    /// Keys match the server's `liveactivity` pushes (`push::activity_payload`).
    struct ContentState: Codable, Hashable {
        /// `working` | `approval` | `completed` | `failed` | ...
        var status: String
        /// What an approval asks, or how the turn ended.
        var detail: String
        /// When the turn started (Unix ms).
        var startedAtMs: Double

        var startedAt: Date { Date(timeIntervalSince1970: startedAtMs / 1000) }
    }

    var threadId: String
    var title: String
    var project: String
    /// The server's own id, so a tap opens the thread on the right server.
    var serverId: String?
}
#endif

/// The threads the widgets show, written by the app as its list changes.
struct WidgetSnapshot: Codable {
    struct Thread: Codable, Hashable {
        var id: String
        var title: String
        var project: String
        /// `working` | `approval` | `completed` | `failed` | `idle` ...
        var status: String
        var unread: Bool
    }

    /// A project the share sheet can send a draft to.
    struct Project: Codable, Hashable {
        var id: String
        var title: String
    }

    var server: String
    var threads: [Thread]
    var savedAt: Date
    /// Missing in snapshots written before projects were kept.
    var projects: [Project]?
    /// The server's own id, for deep links (missing in older snapshots).
    var serverId: String?

    private static var file: URL? { AppGroup.container?.appendingPathComponent("widget-snapshot.json") }

    static func load() -> WidgetSnapshot? {
        guard let file, let data = try? Data(contentsOf: file) else { return nil }
        return try? JSONDecoder().decode(WidgetSnapshot.self, from: data)
    }

    func save() {
        guard let file = Self.file, let data = try? JSONEncoder().encode(self) else { return }
        try? data.write(to: file, options: .atomic)
    }
}

/// Text and images shared to Domovoi from another app, waiting for the app to
/// turn them into a new thread's draft.
struct SharedDraft: Codable {
    var text: String
    /// Image files beside the draft, in its folder.
    var images: [String]
    var createdAt: Date
    /// The project picked in the share sheet (none: the app's selected one).
    var projectId: String?

    private static var inbox: URL? { AppGroup.container?.appendingPathComponent("inbox", isDirectory: true) }

    /// Saves a draft with its images (JPEG data).
    static func save(text: String, images: [Data], projectId: String? = nil) throws {
        guard let inbox else { throw CocoaError(.fileNoSuchFile) }
        let folder = inbox.appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        var names: [String] = []
        for (i, data) in images.enumerated() {
            let name = "image-\(i + 1).jpg"
            try data.write(to: folder.appendingPathComponent(name))
            names.append(name)
        }
        let draft = SharedDraft(text: text, images: names, createdAt: .now, projectId: projectId)
        try JSONEncoder().encode(draft).write(to: folder.appendingPathComponent("draft.json"))
    }

    /// The oldest waiting draft with its images' data, removed from the inbox.
    static func take() -> (text: String, images: [(name: String, data: Data)], projectId: String?)? {
        guard let inbox,
              let folders = try? FileManager.default.contentsOfDirectory(at: inbox, includingPropertiesForKeys: [.creationDateKey]) else { return nil }
        let sorted = folders.sorted { a, b in
            let da = (try? a.resourceValues(forKeys: [.creationDateKey]).creationDate) ?? .distantPast
            let db = (try? b.resourceValues(forKeys: [.creationDateKey]).creationDate) ?? .distantPast
            return da < db
        }
        for folder in sorted {
            defer { try? FileManager.default.removeItem(at: folder) }
            guard let data = try? Data(contentsOf: folder.appendingPathComponent("draft.json")),
                  let draft = try? JSONDecoder().decode(SharedDraft.self, from: data) else { continue }
            let images = draft.images.compactMap { name in (try? Data(contentsOf: folder.appendingPathComponent(name))).map { (name, $0) } }
            return (draft.text, images, draft.projectId)
        }
        return nil
    }
}
