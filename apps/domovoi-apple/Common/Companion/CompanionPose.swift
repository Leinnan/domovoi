import App
import SwiftUI

struct CompanionPose {
    enum Lids { case open, closed, smile, cross, flat }

    var lids = Lids.open
    var openness = 1.0
    var gaze = CGPoint.zero
    var eyeScale = 1.0
    var squash = 1.0
    var lift = 0.0
    var shake = 0.0
    var brow = 0.0
    var perk = 0.0
    var antennaGlow = 0.26
    var dream: Double?
    var offline = false

    static func pose(
        mood: CompanionMood, time: Double, look: CompanionLook, seed: Double,
        gaze: CGPoint = .zero, poke: Double = 0, twitch: Double = 0, still: Bool = false
    ) -> Self {
        let t = max(0, time)
        let style = CompanionStyle(look: look)
        let pulse = sin(t * 3)
        var pose = Self()
        switch mood {
        case .offline:
            pose.lids = .flat
            pose.offline = true
            pose.antennaGlow = 0
        case .searching:
            pose.gaze.x = still ? 0.55 : sin(t * 2.2) * 0.9
            pose.gaze.y = -0.3
            pose.brow = 3
            pose.antennaGlow = (pulse + 1) / 3
        case .wince:
            pose.lids = .cross
            pose.shake = still ? 0 : sin(t * 30) * 3 * max(0, 1 - t)
            pose.brow = -3
            pose.perk = -4
            pose.antennaGlow = 0.1
        case .alert:
            pose.eyeScale = 1.18
            pose.brow = 4
            pose.perk = 5
            let bounce = min(1, t.truncatingRemainder(dividingBy: 3.8) / 0.43)
            pose.lift = still ? 0 : sin(bounce * .pi) * 5
            pose.antennaGlow = still ? 1 : pulse > 0.4 ? 1 : 0.32
        case let .working(level):
            let speed = style.energy * (0.78 + Double(level) * 0.26)
            let sweep = (t * speed).truncatingRemainder(dividingBy: 1)
            pose.gaze = CGPoint(x: still ? 0.42 : (sweep < 0.92 ? -0.82 + sweep * 1.78 : 0.82 - (sweep - 0.92) * 20.5), y: 0.34)
            pose.lift = still ? 0 : sin(t * speed * 6) * 1.7
            pose.brow = -1.7
            pose.antennaGlow = (sin(t * speed * 6) + 1) * 0.26 + 0.25
        case .happy:
            let hop = min(1, t / 1.2)
            pose.lids = .smile
            pose.lift = still ? 0 : pow(sin(hop * .pi), 2) * 12
            pose.squash = still ? 1 : 1 + sin(hop * 2 * .pi) * 0.10
            pose.perk = 3
            pose.antennaGlow = 0.85
        case .idle:
            pose.gaze = still ? .zero : CGPoint(x: sin((t + seed) * 1.1) * 0.32, y: sin((t + seed) * 0.7) * 0.18)
            pose.lift = still ? 0 : pulse * 0.85
            if !still {
                let blinkClock = t + seed
                let cycle = (blinkClock / style.blinkInterval).rounded(.down)
                let blinkAt = style.blinkInterval * (0.4 + 0.3 * sin(seed + cycle * 8.3))
                let blink = max(0, 1 - abs(blinkClock.truncatingRemainder(dividingBy: style.blinkInterval) - blinkAt) / 0.11)
                pose.openness = 1 - blink
            }
        case .drowsy:
            pose.openness = max(0, 1 - pow(min(t / 3, 1), 0.6))
            pose.gaze.y = 0.42
            pose.perk = -2
        case .asleep:
            pose.lids = .closed
            pose.squash = still ? 1 : 1 + sin(t * 1.9) * 0.03
            pose.perk = -2 + max(poke * 7, twitch)
            if still { pose.dream = 0.45 }
            else {
                let phase = t.truncatingRemainder(dividingBy: 4)
                if phase < 2.3 { pose.dream = phase / 2.3 }
            }
        case .waking:
            let progress = min(1, t / 0.4)
            pose.openness = progress < 0.55 ? min(1, progress * 5) : 1 - 0.8 * max(0, 1 - abs(progress - 0.72) / 0.17)
            pose.eyeScale = 1.15
            pose.brow = 3
            pose.lift = still ? 0 : sin(progress * .pi) * 5
        }
        if mood != .offline {
            pose.squash -= poke * 0.15
        }
        if pose.lids == .open && mood != .alert {
            pose.gaze.x = max(-1, min(1, pose.gaze.x + gaze.x))
            pose.gaze.y = max(-1, min(1, pose.gaze.y + gaze.y))
        }
        if mood == .asleep && poke > 0.4 {
            pose.lids = .open
            pose.openness = poke * 0.45
        }
        return pose
    }
}
