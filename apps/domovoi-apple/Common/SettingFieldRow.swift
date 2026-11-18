import App
import SwiftUI

/// One server setting: a toggle, a picker or a text field, written back to
/// the server as it changes (text on submit, and on the Mac also when the
/// field loses focus or Settings closes, so an edit is never dropped).
struct SettingFieldRow: View {
    @ObservedObject var core: Core
    let field: SettingFieldView
    @State private var text = ""
    @FocusState private var focused: Bool

    var body: some View {
        switch field.kind {
        case "bool":
            Toggle(isOn: Binding(get: { field.boolValue }, set: { core.update(.setSettingBool(key: field.key, value: $0)) })) {
                labelText
            }
        case "choice":
            Picker(selection: Binding(get: { field.textValue }, set: { core.update(.setSettingText(key: field.key, value: $0)) })) {
                ForEach(field.choices, id: \.value) { Text($0.label).tag($0.value) }
            } label: { labelText }
        default:
            #if os(iOS)
            // A form's text field shows only a placeholder on iOS: the label
            // and help go above it.
            VStack(alignment: .leading, spacing: Space.xs) {
                labelText
                TextField(field.label, text: $text)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .submitLabel(.done)
                    .focused($focused)
                    .onSubmit(save)
                    // Back without Done mustn't drop the edit.
                    .onChange(of: focused) { _, now in if !now { save() } }
                    .onDisappear(perform: save)
            }
            .onAppear { text = field.textValue }
            .onChange(of: field.textValue) { _, v in text = v }
            #else
            TextField(text: $text) { labelText }
                .autocorrectionDisabled()
                .focused($focused)
                .onSubmit(save)
                .onChange(of: focused) { _, now in if !now { save() } }
                .onDisappear(perform: save)
                .onAppear { text = field.textValue }
                .onChange(of: field.textValue) { _, v in text = v }
            #endif
        }
    }

    private func save() {
        if text != field.textValue { core.update(.setSettingText(key: field.key, value: text)) }
    }

    private var labelText: some View {
        VStack(alignment: .leading, spacing: Space.xxs) {
            Text(field.label)
            if !field.help.isEmpty { Text(field.help).font(.caption).foregroundStyle(.secondary) }
        }
    }
}
