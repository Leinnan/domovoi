import App
import AppKit
import QuartzCore
import SwiftUI

/// Keeping frames in a backing layer avoids relaying animation updates
/// through the toolbar's layout system.
struct ToolbarCompanion: View {
    let look: CompanionLook
    @ObservedObject var brain: CompanionBrain
    @Environment(\.scenePhase) private var phase
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.colorScheme) private var scheme
    @State private var lowPower = ProcessInfo.processInfo.isLowPowerModeEnabled

    private static let size: CGFloat = 24

    var body: some View {
        Artwork(brain: brain, state: Configuration(
            look: look, expression: brain.expression, active: phase == .active,
            dark: scheme == .dark, reduceMotion: reduceMotion, lowPower: lowPower
        ))
        .frame(width: Self.size, height: Self.size)
        .accessibilityHidden(true)
        .onReceive(NotificationCenter.default.publisher(for: .NSProcessInfoPowerStateDidChange)) { _ in
            lowPower = ProcessInfo.processInfo.isLowPowerModeEnabled
        }
    }

    private struct Configuration: Equatable {
        let look: CompanionLook
        let expression: CompanionExpression
        let active: Bool
        let dark: Bool
        let reduceMotion: Bool
        let lowPower: Bool

        var interval: Double? {
            guard active, !reduceMotion, expression.mood != .offline else { return nil }
            return lowPower ? 1 / 15 : expression.mood.frameInterval
        }
    }

    private struct Artwork: NSViewRepresentable {
        let brain: CompanionBrain
        let state: Configuration

        func makeNSView(context: Context) -> Host { Host(brain: brain) }
        func updateNSView(_ view: Host, context: Context) { view.configure(state) }
        static func dismantleNSView(_ view: Host, coordinator: ()) { view.stop() }
    }

    private final class Host: NSView {
        private let brain: CompanionBrain
        private var state: Configuration?
        private var timer: Timer?
        private static let colorSpace = CGColorSpace(name: CGColorSpace.sRGB)!

        init(brain: CompanionBrain) {
            self.brain = brain
            super.init(frame: .zero)
            wantsLayer = true
        }

        required init?(coder: NSCoder) { nil }
        override var isFlipped: Bool { true }
        override func hitTest(_ point: NSPoint) -> NSView? { nil }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            if window != nil { brain.gaze.attach(self) }
            schedule()
        }

        override func viewDidChangeBackingProperties() {
            super.viewDidChangeBackingProperties()
            render()
        }

        func configure(_ newState: Configuration) {
            guard state != newState else { return }
            let previous = state
            state = newState
            if newState.reduceMotion, let previous, previous.expression.mood != newState.expression.mood {
                let fade = CATransition()
                fade.type = .fade
                fade.duration = 0.25
                layer?.add(fade, forKey: "mood")
            }
            if previous?.interval != newState.interval { schedule() }
            else { render() }
        }

        func stop() {
            timer?.invalidate()
            timer = nil
        }

        private func schedule() {
            stop()
            guard window != nil else { return }
            render()
            guard let interval = state?.interval else { return }
            let timer = Timer(timeInterval: interval, repeats: true) { [weak self] timer in
                MainActor.assumeIsolated {
                    if let self { self.render() }
                    else { timer.invalidate() }
                }
            }
            timer.tolerance = interval * 0.1
            self.timer = timer
            RunLoop.main.add(timer, forMode: .common)
        }

        private func render() {
            guard let window, let state, let layer else { return }
            let scale = window.backingScaleFactor
            let width = Int(ToolbarCompanion.size * scale)
            guard let context = CGContext(data: nil, width: width, height: width, bitsPerComponent: 8, bytesPerRow: 0,
                                          space: Self.colorSpace, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return }
            context.translateBy(x: 0, y: CGFloat(width))
            context.scaleBy(x: scale, y: -scale)
            let look = state.look
            let pose: CompanionPose
            if state.reduceMotion || state.expression.mood == .offline {
                let mood = state.expression.mood
                pose = .pose(mood: mood, time: mood.stillTime, look: look, seed: brain.seed, still: true)
            } else {
                pose = brain.pose(at: .now, look: look, expression: state.expression, tracksPointer: true, lowPower: state.lowPower)
            }
            CompanionPainter(look: look, dark: state.dark).draw(
                in: context, size: CGSize(width: ToolbarCompanion.size, height: ToolbarCompanion.size), pose: pose
            )
            CATransaction.begin()
            CATransaction.setDisableActions(true)
            layer.contentsScale = scale
            layer.contents = context.makeImage()
            CATransaction.commit()
        }
    }
}
