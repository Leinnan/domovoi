import App
import SwiftUI

struct CompanionStyle {
    let look: CompanionLook

    static let ink = Color(red: 0.06, green: 0.08, blue: 0.15)

    var eye: CGSize {
        switch look.eyeShape {
        case "oval": CGSize(width: 17, height: 23)
        case "pill": CGSize(width: 13, height: 24)
        case "sleepy": CGSize(width: 22, height: 12)
        default: CGSize(width: 20, height: 20)
        }
    }

    var pupilScale: Double {
        switch look.pupilSize {
        case "small": 0.46
        case "large": 0.79
        default: 0.62
        }
    }

    var energy: Double {
        switch look.energy {
        case "calm": 0.7
        case "lively": 1.3
        default: 1
        }
    }

    var blinkInterval: Double {
        switch look.blink {
        case "rare": 8
        case "often": 2.5
        default: 4.5
        }
    }
}
