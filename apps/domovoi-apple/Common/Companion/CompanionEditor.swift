import App
import SwiftUI

struct CompanionEditor: View {
    @ObservedObject var core: Core
    let environmentId: String

    var body: some View {
        if let server = core.view.connection.environments.first(where: { $0.id == environmentId }) {
            CompanionAppearanceForm(core: core, server: server)
                .id(server.id)
        } else {
            ContentUnavailableView("No saved server", systemImage: "desktopcomputer")
        }
    }
}

private struct CompanionAppearanceForm: View {
    @ObservedObject var core: Core
    let server: EnvironmentView
    @StateObject private var brain = CompanionBrain()
    @State private var preview: Preview = .asleep
    @State private var pendingLook: CompanionLook?
    @State private var resetting = false
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize

    private enum Preview: String, CaseIterable {
        case asleep = "Asleep"
        case working = "Working"
        case alert = "Needs you"
        case finished = "Finished"
        case failed = "Failed"

        var mood: CompanionMood {
            switch self {
            case .asleep: .asleep
            case .working: .working(2)
            case .alert: .alert
            case .finished: .happy
            case .failed: .wince
            }
        }
    }

    private var look: CompanionLook { pendingLook ?? server.companion.look }

    var body: some View {
        Form {
            Section {
                VStack(spacing: 12) {
                    CompanionView(look: look, brain: brain, size: 120)
                    if dynamicTypeSize >= .xxLarge {
                        previewPicker.pickerStyle(.menu)
                    } else {
                        #if os(iOS)
                        previewSegments
                        #else
                        previewPicker.pickerStyle(.segmented)
                        #endif
                    }
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, 8)
            }
            Section("Body") {
                PaletteSwatchGrid(selection: setting(\.body))
            }
            .disabled(resetting)
            Section("Eyes") {
                eyeShapes
                choices("Pupil size", setting(\.pupilSize), ["small", "medium", "large"])
                VStack(alignment: .leading, spacing: 4) {
                    Text("Pupil color")
                    PaletteSwatchGrid(selection: setting(\.pupil), includesInk: true)
                }
            }
            .disabled(resetting)
            Section("Extras") {
                Toggle("Antenna", isOn: setting(\.antenna))
                choices("Ears", setting(\.ears), ["none", "round", "pointy"])
                Toggle("Eyebrows", isOn: setting(\.eyebrows))
            }
            .disabled(resetting)
            Section("Personality") {
                choices("Energy", setting(\.energy), ["calm", "normal", "lively"])
                choices("Blink rate", setting(\.blink), ["rare", "normal", "often"])
                #if os(iOS)
                Toggle("Eyes follow touches", isOn: setting(\.follow))
                #else
                Toggle("Eyes follow the pointer", isOn: setting(\.follow))
                #endif
            }
            .disabled(resetting)
            Section {
                Button("Reset to Default") {
                    resetting = true
                    pendingLook = nil
                    core.update(.resetCompanion(id: server.id))
                }
                .disabled(resetting || !server.companion.custom)
            }
        }
        .formStyle(.grouped)
        .onChange(of: preview, initial: true) { _, value in brain.preview(value.mood) }
        .onChange(of: server.companion) { _, value in
            if pendingLook == value.look { pendingLook = nil }
            if !value.custom { resetting = false }
        }
        .task(id: pendingLook) {
            // The core folds bursts of edits into one view. A value that
            // matches the already visible look may produce no new view.
            guard pendingLook != nil else { return }
            do { try await Task.sleep(for: .seconds(0.5)) }
            catch { return }
            pendingLook = nil
        }
    }

    private var previewPicker: some View {
        Picker("Preview", selection: $preview) {
            ForEach(Preview.allCases, id: \.self) { mood in
                Text(mood.rawValue).tag(mood)
            }
        }
        .accessibilityLabel("Preview mood")
    }

    #if os(iOS)
    private var previewSegments: some View {
        HStack(spacing: 0) {
            ForEach(Preview.allCases, id: \.self) { mood in
                Button { preview = mood } label: {
                    Text(mood.rawValue)
                        .font(.caption.weight(preview == mood ? .semibold : .medium))
                        .lineLimit(1)
                        .minimumScaleFactor(0.75)
                        .frame(maxWidth: .infinity, minHeight: 44)
                        .background {
                            if preview == mood { RoundedRectangle(cornerRadius: 10).fill(.background) }
                        }
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(preview == mood ? .isSelected : [])
            }
        }
        .padding(3)
        .background(.quaternary, in: RoundedRectangle(cornerRadius: 13))
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Preview mood")
    }
    #endif

    private var eyeShapes: some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 64))], spacing: 8) {
            ForEach(["round", "oval", "pill", "sleepy"], id: \.self) { shape in
                let sample = with(\.eyeShape, setTo: shape)
                let selected = look.eyeShape == shape
                Button { setting(\.eyeShape).wrappedValue = shape } label: {
                    VStack(spacing: 2) {
                        CompanionDrawing(look: sample, pose: .pose(mood: .idle, time: 0, look: sample, seed: 0, still: true))
                            .frame(width: 34, height: 34)
                            .accessibilityHidden(true)
                        Text(shape.capitalized).font(.caption)
                    }
                    .padding(5)
                    .frame(maxWidth: .infinity)
                    .background(selected ? Color.accentColor.opacity(0.18) : .clear, in: RoundedRectangle(cornerRadius: 10))
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(selected ? .isSelected : [])
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Eye shape")
    }

    private func choices(_ title: String, _ selection: Binding<String>, _ keys: [String]) -> some View {
        Picker(title, selection: selection) {
            ForEach(keys, id: \.self) { Text($0.capitalized).tag($0) }
        }
    }

    private func with<Value>(_ key: WritableKeyPath<CompanionLook, Value>, setTo value: Value) -> CompanionLook {
        var changed = look
        changed[keyPath: key] = value
        return changed
    }

    private func setting<Value>(_ key: WritableKeyPath<CompanionLook, Value>) -> Binding<Value> {
        Binding {
            look[keyPath: key]
        } set: { value in
            let changed = with(key, setTo: value)
            pendingLook = changed
            core.update(.setCompanion(id: server.id, look: changed))
        }
    }
}
