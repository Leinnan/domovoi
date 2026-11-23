import SwiftUI
#if os(iOS)
import UIKit
typealias CompanionPointerView = UIView
private typealias PointerWindow = UIWindow
#else
import AppKit
typealias CompanionPointerView = NSView
private typealias PointerWindow = NSWindow
#endif

@MainActor
final class CompanionGaze {
    private weak var anchor: CompanionPointerView?
    private var point: CGPoint?
    private var lastInput = Date.distantPast
    private var lastNearTouch = Date.distantPast

    func attach(_ anchor: CompanionPointerView) { self.anchor = anchor }

    fileprivate func record(_ point: CGPoint, at window: PointerWindow, pressed: Bool) {
        guard let anchor, anchor.window === window, !anchor.bounds.isEmpty else { return }
        #if os(iOS)
        let local = anchor.convert(point, from: window)
        #else
        let local = anchor.convert(point, from: nil)
        #endif
        let bounds = anchor.bounds
        self.point = CGPoint(x: local.x - bounds.midX, y: local.y - bounds.midY)
        lastInput = .now
        if pressed, bounds.insetBy(dx: -18, dy: -18).contains(local) {
            lastNearTouch = lastInput
            if bounds.contains(local) { onPoke?() }
        }
    }

    var onPoke: (() -> Void)?

    func direction(at now: Date) -> CGPoint {
        guard let point else { return .zero }
        let strength = min(1, max(0, 1 - max(0, now.timeIntervalSince(lastInput) - 0.15) / 1.5))
        let length = max(24, hypot(point.x, point.y))
        return CGPoint(x: point.x * strength / length, y: point.y * strength / length)
    }

    func twitch(at now: Date) -> Double {
        let elapsed = now.timeIntervalSince(lastNearTouch)
        guard elapsed >= 0, elapsed < 0.25 else { return 0 }
        return sin(elapsed * .pi / 0.25) * 4
    }
}

#if os(iOS)
struct CompanionPointerAnchor: UIViewRepresentable {
    let gaze: CompanionGaze
    func makeUIView(context: Context) -> UIView {
        let view = UIView()
        view.isUserInteractionEnabled = false
        return view
    }
    func updateUIView(_ view: UIView, context: Context) { gaze.attach(view) }
}

struct CompanionPointerMonitor: UIViewRepresentable {
    let gaze: CompanionGaze

    func makeUIView(context: Context) -> Host { Host(gaze: gaze) }
    func updateUIView(_ view: Host, context: Context) {}
    static func dismantleUIView(_ view: Host, coordinator: ()) { view.detach() }

    final class Host: UIView {
        private let observer: TouchObserver
        private weak var attachedWindow: UIWindow?

        init(gaze: CompanionGaze) {
            observer = TouchObserver(gaze: gaze)
            super.init(frame: .zero)
            isUserInteractionEnabled = false
        }

        required init?(coder: NSCoder) { nil }

        override func didMoveToWindow() {
            super.didMoveToWindow()
            guard attachedWindow !== window else { return }
            detach()
            window?.addGestureRecognizer(observer)
            attachedWindow = window
        }

        func detach() {
            attachedWindow?.removeGestureRecognizer(observer)
            attachedWindow = nil
        }
    }

    private final class TouchObserver: UIGestureRecognizer, UIGestureRecognizerDelegate {
        private let gaze: CompanionGaze

        init(gaze: CompanionGaze) {
            self.gaze = gaze
            super.init(target: nil, action: nil)
            delegate = self
            cancelsTouchesInView = false
            delaysTouchesBegan = false
            delaysTouchesEnded = false
        }

        override func canPrevent(_ preventedGestureRecognizer: UIGestureRecognizer) -> Bool { false }
        override func canBePrevented(by preventingGestureRecognizer: UIGestureRecognizer) -> Bool { false }
        func gestureRecognizer(_ gestureRecognizer: UIGestureRecognizer, shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer) -> Bool { true }

        override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent) { record(touches, pressed: true) }
        override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent) { record(touches, pressed: false) }
        override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent) {
            if let view, event.touches(for: view)?.allSatisfy({ $0.phase == .ended || $0.phase == .cancelled }) ?? true {
                state = .failed
            }
        }
        override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent) { state = .failed }

        private func record(_ touches: Set<UITouch>, pressed: Bool) {
            guard let window = view as? UIWindow, let touch = touches.min(by: { $0.timestamp < $1.timestamp }) else { return }
            gaze.record(touch.location(in: window), at: window, pressed: pressed)
        }
    }
}
#else
struct CompanionPointerAnchor: NSViewRepresentable {
    let gaze: CompanionGaze
    func makeNSView(context: Context) -> NSView { Anchor() }
    func updateNSView(_ view: NSView, context: Context) { gaze.attach(view) }

    private final class Anchor: NSView {
        override var isFlipped: Bool { true }
        override func hitTest(_ point: NSPoint) -> NSView? { nil }
    }
}

struct CompanionPointerMonitor: NSViewRepresentable {
    let gaze: CompanionGaze
    func makeNSView(context: Context) -> Host { Host(gaze: gaze) }
    func updateNSView(_ view: Host, context: Context) {}
    static func dismantleNSView(_ view: Host, coordinator: ()) { view.detach() }

    final class Host: NSView {
        private let gaze: CompanionGaze
        private var monitor: Any?
        private weak var attachedWindow: NSWindow?
        private var acceptedMouseMoves = false

        init(gaze: CompanionGaze) {
            self.gaze = gaze
            super.init(frame: .zero)
        }

        required init?(coder: NSCoder) { nil }
        override func hitTest(_ point: NSPoint) -> NSView? { nil }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            guard attachedWindow !== window else { return }
            detach()
            guard let window else { return }
            attachedWindow = window
            acceptedMouseMoves = window.acceptsMouseMovedEvents
            window.acceptsMouseMovedEvents = true
            monitor = NSEvent.addLocalMonitorForEvents(matching: [.mouseMoved, .leftMouseDown]) { [weak self] event in
                MainActor.assumeIsolated {
                    if let self, let window = self.window, event.window === window {
                        self.gaze.record(event.locationInWindow, at: window, pressed: event.type == .leftMouseDown)
                    }
                }
                return event
            }
        }

        func detach() {
            if let monitor { NSEvent.removeMonitor(monitor) }
            monitor = nil
            attachedWindow?.acceptsMouseMovedEvents = acceptedMouseMoves
            attachedWindow = nil
        }
    }
}
#endif
