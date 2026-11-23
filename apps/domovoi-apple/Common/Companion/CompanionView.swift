import App
import SwiftUI

struct CompanionView: View {
    let look: CompanionLook
    @ObservedObject var brain: CompanionBrain
    let size: CGFloat
    var tracksPointer = false
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var lowPower = ProcessInfo.processInfo.isLowPowerModeEnabled

    var body: some View {
        let expression = brain.expression
        Group {
            if reduceMotion || expression.mood == .offline {
                ZStack {
                    CompanionDrawing(look: look, pose: .pose(mood: expression.mood, time: expression.mood.stillTime, look: look, seed: brain.seed, still: true))
                        .id(expression.mood)
                        .transition(.opacity)
                }
                .animation(reduceMotion ? .easeInOut(duration: 0.25) : nil, value: expression.mood)
            } else if scenePhase != .active {
                frame(at: .now, expression: expression)
            } else {
                TimelineView(.animation(minimumInterval: lowPower ? 1 / 15 : expression.mood.frameInterval)) {
                    frame(at: $0.date, expression: expression)
                }
            }
        }
        .frame(width: size, height: size)
        .background {
            if tracksPointer { CompanionPointerAnchor(gaze: brain.gaze) }
        }
        .accessibilityHidden(true)
        .onReceive(NotificationCenter.default.publisher(for: .NSProcessInfoPowerStateDidChange)) { _ in
            lowPower = ProcessInfo.processInfo.isLowPowerModeEnabled
        }
    }

    private func frame(at now: Date, expression: CompanionExpression) -> CompanionDrawing {
        CompanionDrawing(look: look, pose: brain.pose(
            at: now, look: look, expression: expression, tracksPointer: tracksPointer, lowPower: lowPower
        ))
    }
}

struct CompanionDrawing: View {
    let look: CompanionLook
    let pose: CompanionPose
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let painter = CompanionPainter(look: look, dark: scheme == .dark)
        Canvas { context, size in
            context.withCGContext { painter.draw(in: $0, size: size, pose: pose) }
        }
    }
}
