import App
import SwiftUI

struct CompanionExpression: Equatable {
    let mood: CompanionMood
    let since: Date
}

@MainActor
final class CompanionBrain: ObservableObject {
    @Published private(set) var expression = CompanionExpression(mood: .offline, since: .now)
    let seed = Double.random(in: 0..<1000)
    let gaze = CompanionGaze()
    private(set) var touchedAt: Date?
    private var activity: CompanionActivity?
    private var idleSince: Date?
    private struct Reaction {
        let mood: CompanionMood
        let started: Date
        let until: Date

        init(_ mood: CompanionMood, at now: Date, duration: Double) {
            self.mood = mood
            started = now
            until = now.addingTimeInterval(duration)
        }
    }

    private var reaction: Reaction?
    private var wakeUntil: Date?
    private var timer: Task<Void, Never>?
    private var previewing = false

    init() { gaze.onPoke = { [weak self] in self?.poke() } }

    deinit { timer?.cancel() }

    func poke() { touchedAt = .now }

    func update(_ activity: CompanionActivity, reduceMotion: Bool) {
        let now = Date.now
        let previous = self.activity
        self.activity = activity

        if previous?.server != activity.server || activity.connection != "connected" {
            idleSince = nil
            reaction = nil
            wakeUntil = nil
        }
        if activity.connection == "connected" {
            if activity.working > 0 || activity.attention > 0 {
                idleSince = nil
            } else if idleSince == nil {
                let startingQuietly = previous?.connection != "connected" && activity.sequence == previous?.sequence
                idleSince = startingQuietly || previous == nil ? now.addingTimeInterval(-33) : now
            }
            if let previous, previous.server == activity.server, previous.sequence != activity.sequence {
                if activity.beat == "failed" {
                    reaction = Reaction(.wince, at: now, duration: 1.5)
                } else if activity.beat == "finished" && (reaction?.mood != .wince || (reaction?.until ?? .distantPast) <= now) {
                    reaction = Reaction(.happy, at: now, duration: 1.2)
                }
            }
            if previous?.server == activity.server && expression.mood == .asleep
                && (activity.working > 0 || activity.attention > 0 || reaction != nil) && !reduceMotion {
                wakeUntil = now.addingTimeInterval(0.4)
            }
        }
        if reduceMotion { wakeUntil = nil }
        advance(at: now)
    }

    func preview(_ mood: CompanionMood) {
        previewing = true
        timer?.cancel()
        expression = CompanionExpression(mood: mood, since: .now)
    }

    func pose(
        at now: Date, look: CompanionLook, expression: CompanionExpression, tracksPointer: Bool, lowPower: Bool
    ) -> CompanionPose {
        let follows = tracksPointer && look.follow && !lowPower
        let elapsed = touchedAt.map { now.timeIntervalSince($0) } ?? 1
        let poke = elapsed >= 0 && elapsed < 0.25 ? sin(elapsed * .pi / 0.25) : 0
        return .pose(
            mood: expression.mood, time: now.timeIntervalSince(expression.since), look: look, seed: seed,
            gaze: follows ? gaze.direction(at: now) : .zero, poke: poke,
            twitch: follows ? gaze.twitch(at: now) : 0
        )
    }

    private func advance(at now: Date) {
        timer?.cancel()
        guard let activity, !previewing else { return }
        var deadline: Date?
        let mood: CompanionMood
        if ["pairing", "connecting", "reconnecting"].contains(activity.connection) {
            mood = .searching
        } else if activity.connection != "connected" {
            mood = .offline
        } else {
            if let reaction, reaction.until <= now { self.reaction = nil }
            if let wakeUntil, wakeUntil <= now { self.wakeUntil = nil }
            deadline = reaction?.until
            if let wakeUntil {
                mood = .waking
                deadline = min(deadline ?? wakeUntil, wakeUntil)
            } else if reaction?.mood == .wince {
                mood = .wince
            } else if activity.attention > 0 {
                mood = .alert
            } else if activity.working > 0 {
                mood = .working(activity.working >= 4 ? 3 : activity.working >= 2 ? 2 : 1)
            } else if reaction?.mood == .happy {
                mood = .happy
            } else if let idleSince {
                let drowse = idleSince.addingTimeInterval(30)
                let sleep = idleSince.addingTimeInterval(33)
                if now < drowse {
                    mood = .idle
                    deadline = min(deadline ?? drowse, drowse)
                } else if now < sleep {
                    mood = .drowsy
                    deadline = min(deadline ?? sleep, sleep)
                } else {
                    mood = .asleep
                }
            } else {
                mood = .asleep
            }
        }
        let newReaction = reaction.flatMap { $0.mood == mood && $0.started > expression.since ? $0 : nil }
        if mood != expression.mood || newReaction != nil {
            expression = CompanionExpression(mood: mood, since: now)
        }
        if let deadline {
            timer = Task { [weak self] in
                do { try await Task.sleep(for: .seconds(max(0, deadline.timeIntervalSinceNow))) }
                catch { return }
                self?.advance(at: .now)
            }
        }
    }
}

struct CompanionWindow: ViewModifier {
    let brain: CompanionBrain
    let connection: ConnectionView
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.scenePhase) private var scenePhase

    func body(content: Content) -> some View {
        let activity = CompanionActivity(connection)
        content
            .task(id: activity) { brain.update(activity, reduceMotion: reduceMotion) }
            .onChange(of: reduceMotion) { _, value in brain.update(activity, reduceMotion: value) }
            .background {
                if scenePhase == .active { CompanionPointerMonitor(gaze: brain.gaze) }
            }
    }
}
