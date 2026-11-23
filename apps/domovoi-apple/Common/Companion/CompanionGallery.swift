#if DEBUG
import App
import SwiftUI
#if os(macOS)
import AppKit
#else
import UIKit
#endif

/// Fixed poses for comparing the drawing across palettes, appearances and motion settings.
struct CompanionGallery: View {
    let reduceMotion: Bool

    private static let looks = [
        CompanionLook(body: "violet", eyeShape: "round", pupil: "ink", pupilSize: "medium", antenna: true, ears: "none", eyebrows: true, energy: "normal", blink: "normal", follow: false),
        CompanionLook(body: "teal", eyeShape: "pill", pupil: "blue", pupilSize: "small", antenna: false, ears: "round", eyebrows: true, energy: "calm", blink: "rare", follow: false),
        CompanionLook(body: "rose", eyeShape: "sleepy", pupil: "ink", pupilSize: "large", antenna: true, ears: "pointy", eyebrows: false, energy: "lively", blink: "often", follow: false)
    ]

    private static let rows: [(name: String, mood: CompanionMood, time: Double)] = [
        ("Asleep", .asleep, 1.2),
        ("Drowsy", .drowsy, 1.1),
        ("Idle", .idle, 1.45),
        ("Waking", .waking, 0.12),
        ("Working", .working(2), 0.53),
        ("Needs you", .alert, 0.23),
        ("Finished", .happy, 0.58),
        ("Failed", .wince, 0.31),
        ("Connecting", .searching, 0.68),
        ("Offline", .offline, 0)
    ]

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("Companion poses").font(.title2.bold())
            Text(reduceMotion ? "Reduce Motion" : "Animation frames")
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .padding(.bottom, 16)
            HStack {
                Spacer().frame(width: 98)
                ForEach(["Violet", "Teal", "Rose"], id: \.self) { name in
                    Text(name).font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity)
                }
            }
            ForEach(Self.rows.indices, id: \.self) { index in
                let row = Self.rows[index]
                Divider()
                HStack {
                    Text(row.name).font(.callout.weight(.medium)).frame(width: 98, alignment: .leading)
                    ForEach(Self.looks, id: \.body) { look in
                        CompanionDrawing(
                            look: look,
                            pose: .pose(mood: row.mood, time: reduceMotion ? row.mood.stillTime : row.time,
                                        look: look, seed: 27, still: reduceMotion)
                        )
                        .frame(width: 62, height: 62)
                        .frame(maxWidth: .infinity)
                    }
                }
            }
        }
        .padding(22)
        .frame(width: 390)
        .background(.background)
    }

    @MainActor
    static func capture(in directory: URL) throws {
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        for (appearance, scheme) in [("light", ColorScheme.light), ("dark", .dark)] {
            for (motion, reduced) in [("motion", false), ("reduced", true)] {
                let renderer = ImageRenderer(content: Self(reduceMotion: reduced).environment(\.colorScheme, scheme))
                renderer.scale = 2
                guard let image = renderer.cgImage else { throw CocoaError(.fileWriteUnknown) }
                #if os(macOS)
                let png = NSBitmapImageRep(cgImage: image).representation(using: .png, properties: [:])
                #else
                let png = UIImage(cgImage: image).pngData()
                #endif
                guard let png else { throw CocoaError(.fileWriteUnknown) }
                try png.write(to: directory.appendingPathComponent("companion-\(appearance)-\(motion).png"))
            }
        }
    }
}
#endif
