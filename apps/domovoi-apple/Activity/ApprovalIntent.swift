import AppIntents
import Foundation

/// Allow or Decline from a Live Activity. It runs in the app's process (a
/// `LiveActivityIntent`), where the core answers the thread's pending
/// approval; compiled into the widget too, which only shows the buttons.
struct RespondToApprovalIntent: LiveActivityIntent {
    static let title: LocalizedStringResource = "Answer an Approval"
    static let isDiscoverable = false

    @Parameter(title: "Thread") var threadId: String
    @Parameter(title: "Server") var serverId: String
    /// `accept` or `decline`.
    @Parameter(title: "Decision") var decision: String

    init() {}

    init(threadId: String, serverId: String?, decision: String) {
        self.threadId = threadId
        self.serverId = serverId ?? ""
        self.decision = decision
    }

    func perform() async throws -> some IntentResult {
        await ApprovalIntentHandler.handle(thread: threadId, server: serverId, decision: decision)
        return .result()
    }
}

/// Where the intent's answer goes: the app sets `respond` once its core is
/// there; until then answers wait (the system may start the app for one).
@MainActor
enum ApprovalIntentHandler {
    static var respond: ((_ thread: String, _ server: String, _ decision: String) -> Void)? {
        didSet {
            guard let respond else { return }
            for answer in waiting { respond(answer.thread, answer.server, answer.decision) }
            waiting.removeAll()
        }
    }
    private static var waiting: [(thread: String, server: String, decision: String)] = []

    static func handle(thread: String, server: String, decision: String) {
        if let respond { respond(thread, server, decision) } else { waiting.append((thread, server, decision)) }
    }
}
