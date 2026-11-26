import App
import SwiftUI
#if os(macOS)
import AppKit
#endif

/// Client-local theme override, saved on this device only.
enum AppAppearance: String, CaseIterable, Identifiable {
    case system
    case light
    case dark

    var id: String { rawValue }

    var label: String {
        switch self {
        case .system: "System"
        case .light: "Light"
        case .dark: "Dark"
        }
    }

    var symbol: String {
        switch self {
        case .system: "circle.lefthalf.filled"
        case .light: "sun.max"
        case .dark: "moon"
        }
    }

    var colorScheme: ColorScheme? {
        switch self {
        case .system: nil
        case .light: .light
        case .dark: .dark
        }
    }

    #if os(macOS)
    var nsAppearance: NSAppearance? {
        switch self {
        case .system: nil
        case .light: NSAppearance(named: .aqua)
        case .dark: NSAppearance(named: .darkAqua)
        }
    }
    #endif
}

/// Client-local transcript text scale on macOS (iOS follows system Dynamic Type).
enum TranscriptTextScale: String, CaseIterable, Identifiable {
    case small
    case `default`
    case large
    case extraLarge

    var id: String { rawValue }

    var label: String {
        switch self {
        case .small: "Small"
        case .default: "Default"
        case .large: "Large"
        case .extraLarge: "Extra Large"
        }
    }

    var dynamicTypeSize: DynamicTypeSize {
        switch self {
        case .small: .small
        case .default: .large
        case .large: .xLarge
        case .extraLarge: .xxLarge
        }
    }
}

enum AppPreferenceKeys {
    static let appearance = "appAppearance"
    static let transcriptTextScale = "transcriptTextScale"
}

/// Applies the device-local appearance override across the app and its windows.
struct AppAppearanceModifier: ViewModifier {
    @AppStorage(AppPreferenceKeys.appearance) private var rawAppearance = AppAppearance.system.rawValue

    private var appearance: AppAppearance {
        AppAppearance(rawValue: rawAppearance) ?? .system
    }

    func body(content: Content) -> some View {
        content
            .preferredColorScheme(appearance.colorScheme)
            #if os(macOS)
            .onChange(of: rawAppearance, initial: true) { _, _ in
                // Snapshot runs drive NSApp.appearance directly for light/dark sweeps.
                guard ProcessInfo.processInfo.environment["DOMOVOI_SNAPSHOT_DIR"] == nil else { return }
                NSApp.appearance = appearance.nsAppearance
            }
            #endif
    }
}

/// Applies the macOS device-local transcript text size.
struct TranscriptTextScaleModifier: ViewModifier {
    @AppStorage(AppPreferenceKeys.transcriptTextScale) private var rawScale = TranscriptTextScale.default.rawValue

    private var scale: TranscriptTextScale {
        TranscriptTextScale(rawValue: rawScale) ?? .default
    }

    func body(content: Content) -> some View {
        #if os(macOS)
        content.dynamicTypeSize(scale.dynamicTypeSize)
        #else
        content
        #endif
    }
}

/// A still companion avatar for server lists, pickers, and headers.
struct CompanionStill: View {
    let look: CompanionLook
    let size: CGFloat
    var mood: CompanionMood = .idle

    var body: some View {
        CompanionDrawing(look: look, pose: .pose(mood: mood, time: mood.stillTime, look: look, seed: 0, still: true))
            .frame(width: size, height: size)
            .accessibilityHidden(true)
    }
}
