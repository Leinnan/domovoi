import App
import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

/// The composer for iPhone: a floating glass panel with the message field,
/// chips for plan mode, effort, permissions and model, attachments, @
/// mentions, and one button that is Send or Stop as the turn needs.
struct MobileComposer: View {
    @ObservedObject var core: Core
    /// Puts the cursor in the field when it appears (a new thread).
    var autofocus = false
    @State private var text = ""
    /// The draft goes to the core a moment after typing pauses: each update
    /// redraws every screen watching the core, the transcript included.
    @State private var draftSync: Task<Void, Never>?
    @State private var photos: [PhotosPickerItem] = []
    @State private var pickingPhotos = false
    @State private var takingPhoto = false
    @State private var importingFile = false
    /// Small pictures of what's attached, by attachment name.
    @State private var thumbnails: [String: UIImage] = [:]
    @State private var options: ComposerOption?
    /// A photo that couldn't be attached, shown under the field for a moment.
    @State private var attachError: String?
    @FocusState private var focused: Bool
    /// What was just sent: the keyboard can still commit an autocorrection
    /// to it after Send, which would put it back in the box.
    @State private var justSent: String?
    @StateObject private var dictation = Dictation()
    /// The text before dictation started: the transcript goes after it.
    @State private var dictationBase = ""
    @Namespace private var glass

    private var composer: ComposerView { core.view.composer }
    private var planning: Bool { composer.interactionMode == "plan" }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s) {
            ProviderNotice(core: core)
                .padding(.horizontal, Space.xs)
            if let mention = composer.mention, !mention.suggestions.isEmpty {
                mentions(mention)
            } else if !composer.slashCommands.isEmpty {
                slashCommands(composer.slashCommands)
            }
            panel
        }
        .padding(.horizontal, Space.s)
        .padding(.top, Space.xs)
        .padding(.bottom, Space.xs)
        .onChange(of: text) { _, value in
            if let sent = justSent, !value.isEmpty, Self.sameMessage(value, sent) {
                text = ""
                return
            }
            guard value != composer.draft else { return }
            draftSync?.cancel()
            // Right away while picking a mention or a / command (the lists
            // follow what's typed); otherwise once typing pauses.
            if composer.mention != nil || !composer.slashCommands.isEmpty || value.last == "@" || value.hasPrefix("/") || value.isEmpty {
                core.update(.setDraft(text: value))
            } else {
                draftSync = Task {
                    try? await Task.sleep(for: .milliseconds(250))
                    guard !Task.isCancelled else { return }
                    core.update(.setDraft(text: value))
                    draftSync = nil
                }
            }
        }
        .onDisappear {
            draftSync?.cancel()
            if text != composer.draft { core.update(.setDraft(text: text)) }
        }
        .onChange(of: composer.draftRevision) { _, _ in text = composer.draft }
        .onChange(of: core.view.threadId) { _, _ in text = composer.draft }
        .onChange(of: photos) { _, items in
            guard !items.isEmpty else { return }
            Task { await attach(items) }
        }
        .onAppear {
            text = composer.draft
            if autofocus {
                Task {
                    // After the push animation, or the keyboard is refused.
                    try? await Task.sleep(for: .milliseconds(450))
                    focused = true
                }
            }
        }
        .photosPicker(isPresented: $pickingPhotos, selection: $photos, maxSelectionCount: 4, matching: .images)
        .fileImporter(isPresented: $importingFile, allowedContentTypes: [.image], allowsMultipleSelection: true) { result in
            guard case .success(let urls) = result else { return }
            var failed = 0
            for url in urls {
                let scoped = url.startAccessingSecurityScopedResource()
                defer { if scoped { url.stopAccessingSecurityScopedResource() } }
                if let data = try? Data(contentsOf: url), let image = UIImage(data: data) {
                    add(image, name: url.deletingPathExtension().lastPathComponent)
                } else {
                    failed += 1
                }
            }
            if failed > 0 { attachError = failed == 1 ? "That file couldn't be attached." : "\(failed) files couldn't be attached." }
        }
        .onChange(of: composer.attachments.map(\.name)) { _, names in
            thumbnails = thumbnails.filter { names.contains($0.key) }
        }
        .fullScreenCover(isPresented: $takingPhoto) {
            CameraPicker { image in if let image { add(image) } }
                .ignoresSafeArea()
        }
        .sheet(item: $options) { focus in ComposerOptionsSheet(core: core, focus: focus) }
        .task(id: attachError) {
            guard attachError != nil else { return }
            try? await Task.sleep(for: .seconds(4))
            attachError = nil
        }
    }

    // MARK: Panel

    private var panel: some View {
        VStack(alignment: .leading, spacing: Space.s) {
            if !composer.attachments.isEmpty { attachments }
            HStack(alignment: .bottom, spacing: Space.xs) {
                TextField(dictation.listening ? "Listening…" : (planning ? "Describe what to plan…" : composer.placeholder), text: $text, axis: .vertical)
                    .lineLimit(1...8)
                    .focused($focused)
                    .accessibilityLabel("Message")
                micButton
            }
            .padding(.horizontal, Space.xs)
            .padding(.top, Space.xs)
            if let problem = attachError ?? dictation.error {
                HStack(alignment: .firstTextBaseline, spacing: Space.s) {
                    Label(problem, systemImage: "exclamationmark.triangle.fill")
                        .font(TypeRole.meta)
                        .foregroundStyle(Tint.caution)
                    // Access was refused: the way to change that.
                    if dictation.needsSettings, attachError == nil, let settings = URL(string: UIApplication.openSettingsURLString) {
                        Link("Open Settings", destination: settings).font(TypeRole.meta.weight(.semibold))
                    }
                }
                .transition(.opacity)
            }
            HStack(alignment: .center, spacing: Space.s) {
                addMenu
                ModeChips(composer: composer, open: { options = $0 }, togglePlan: togglePlan)
                actionButtons
            }
        }
        .padding(Space.m)
        .glassPanel(in: RoundedRectangle(cornerRadius: Radius.bubble + 6, style: .continuous), tint: planning ? Tint.plan.opacity(0.35) : nil)
        .overlay {
            // Plan mode shows in the panel's edge, not only its chip.
            RoundedRectangle(cornerRadius: Radius.bubble + 6, style: .continuous)
                .strokeBorder(Tint.plan.opacity(planning ? 0.55 : 0), lineWidth: 1)
                .allowsHitTesting(false)
        }
        .animation(.snappy, value: planning)
        .animation(.snappy, value: composer.canStop)
        .animation(.snappy, value: composer.sending)
        .animation(.snappy, value: attachError)
        .animation(.snappy, value: dictation.error)
    }

    /// Dictation: speak instead of typing; the words land after what's typed.
    private var micButton: some View {
        Button {
            if !dictation.listening {
                dictationBase = text
            }
            dictation.toggle { transcript in
                let base = dictationBase
                let gap = base.isEmpty || base.hasSuffix(" ") || base.hasSuffix("\n") ? "" : " "
                text = base + gap + transcript
            }
        } label: {
            Image(systemName: dictation.listening ? "waveform" : "mic")
                .font(.body)
                .foregroundStyle(dictation.listening ? Tint.danger : .secondary)
                .symbolEffect(.variableColor.iterative, isActive: dictation.listening)
                .frame(width: 36, height: 36)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(dictation.listening ? "Stop Dictation" : "Dictate")
        .onDisappear { dictation.stop() }
    }

    private var addMenu: some View {
        Menu {
            Button { pickingPhotos = true } label: { Label("Photo Library", systemImage: "photo.on.rectangle") }
            if UIImagePickerController.isSourceTypeAvailable(.camera) {
                Button { takingPhoto = true } label: { Label("Take Photo", systemImage: "camera") }
            }
            Button { importingFile = true } label: { Label("Choose from Files", systemImage: "folder") }
            if UIPasteboard.general.hasImages {
                Button(action: pasteImage) { Label("Paste Image", systemImage: "doc.on.clipboard") }
            }
        } label: {
            Image(systemName: "plus")
                .font(.body.weight(.semibold))
                .frame(width: 24, height: 24)
                .contentShape(Circle())
        }
        .buttonStyle(.glass)
        .buttonBorderShape(.circle)
        .accessibilityLabel("Attach")
    }

    /// Send, a spinner while it goes out, or Stop while a turn runs; with
    /// text typed during a turn, Send (it queues) beside a small Stop.
    @ViewBuilder private var actionButtons: some View {
        let typed = !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        GlassEffectContainer(spacing: Space.s) {
            HStack(spacing: Space.s) {
                if composer.canStop {
                    Button { core.update(.interrupt) } label: {
                        Image(systemName: "stop.fill")
                            .font(typed ? .footnote : .body)
                            // Smaller beside Send, but still a comfortable target.
                            .frame(width: typed ? 22 : 24, height: typed ? 22 : 24)
                    }
                    .buttonStyle(.glassProminent)
                    .buttonBorderShape(.circle)
                    .tint(Tint.danger)
                    .glassEffectID("stop", in: glass)
                    .keyboardShortcut(".", modifiers: .command)
                    .accessibilityLabel("Stop")
                }
                if composer.canSteer, typed {
                    // Into the turn under way, rather than after it.
                    Button(action: steer) {
                        Image(systemName: "arrow.turn.down.right").font(.body.weight(.semibold))
                            .frame(width: 24, height: 24)
                    }
                    .buttonStyle(.glass)
                    .buttonBorderShape(.circle)
                    .glassEffectID("steer", in: glass)
                    .accessibilityLabel("Send now, into the turn under way")
                }
                if !composer.canStop || typed {
                    Button(action: send) {
                        Group {
                            if composer.sending {
                                ProgressView().tint(.white)
                            } else {
                                Image(systemName: "arrow.up").font(.body.weight(.bold))
                            }
                        }
                        .frame(width: 24, height: 24)
                    }
                    .buttonStyle(.glassProminent)
                    .buttonBorderShape(.circle)
                    .tint(planning ? Tint.plan : .accentColor)
                    // Typed but not synced yet: Send syncs before it sends.
                    .disabled(!composer.canSend && !(draftSync != nil && typed))
                    .glassEffectID("send", in: glass)
                    // A hardware keyboard: Return is a new line in the field.
                    .keyboardShortcut(.return, modifiers: .command)
                    .accessibilityLabel(composer.sending ? "Sending" : "Send")
                }
            }
        }
    }

    private var attachments: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: Space.s) {
                ForEach(composer.attachments, id: \.id) { a in
                    HStack(spacing: Space.s) {
                        Group {
                            if let image = thumbnails[a.name] {
                                Image(uiImage: image).resizable().scaledToFill()
                            } else {
                                Image(systemName: "photo").foregroundStyle(.secondary)
                            }
                        }
                        .frame(width: 30, height: 30)
                        .clipShape(RoundedRectangle(cornerRadius: Radius.chip - 2, style: .continuous))
                        .accessibilityHidden(true)
                        Text(a.name).lineLimit(1)
                        if a.uploading {
                            ProgressView().controlSize(.mini)
                        } else {
                            Button { core.update(.removeAttachment(id: a.id)) } label: {
                                Image(systemName: "xmark.circle.fill")
                                    .frame(width: 32, height: 32)
                                    .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                            .foregroundStyle(.secondary)
                            .accessibilityLabel("Remove \(a.name)")
                        }
                    }
                    .font(.caption)
                    .padding(.leading, Space.xs)
                    .padding(.trailing, Space.xxs)
                    .padding(.vertical, Space.xxs)
                    .glassChip()
                }
            }
            .padding(.vertical, Space.xxs)
        }
    }

    private func mentions(_ mention: MentionView) -> some View {
        ScrollView(.horizontal, showsIndicators: false) {
            GlassEffectContainer(spacing: Space.s) {
                HStack(spacing: Space.s) {
                    ForEach(Array(mention.suggestions.enumerated()), id: \.offset) { _, entry in
                        Button { core.update(.pickMention(path: entry.path)) } label: {
                            Label(entry.name, systemImage: entry.directory ? "folder" : "doc")
                                .font(.caption)
                                .padding(.horizontal, Space.m)
                                .padding(.vertical, Space.s)
                        }
                        .buttonStyle(.plain)
                        .glassChip()
                    }
                }
                .padding(.horizontal, Space.xs)
            }
        }
    }

    /// Commands for a draft that is `/` and a word: a tap completes it.
    private func slashCommands(_ commands: [SlashCommandView]) -> some View {
        ScrollView(.horizontal, showsIndicators: false) {
            GlassEffectContainer(spacing: Space.s) {
                HStack(spacing: Space.s) {
                    ForEach(commands, id: \.name) { command in
                        Button { core.update(.pickSlashCommand(name: command.name)) } label: {
                            Text("/\(command.name)")
                                .font(.caption.monospaced())
                                .padding(.horizontal, Space.m)
                                .padding(.vertical, Space.s)
                        }
                        .buttonStyle(.plain)
                        .glassChip()
                        .accessibilityLabel("Command \(command.name)")
                        .accessibilityHint(command.description)
                    }
                }
                .padding(.horizontal, Space.xs)
            }
        }
    }

    // MARK: Actions

    private func togglePlan() {
        Platform.selectionChanged()
        core.update(.setInteractionMode(mode: planning ? "default" : "plan"))
    }

    private func steer() {
        draftSync?.cancel()
        dictation.stop()
        core.update(.setDraft(text: text))
        core.update(.steer)
        Platform.answered(false)
        justSent = text
        text = ""
        keepTypingFocus()
        Task {
            try? await Task.sleep(for: .seconds(1.5))
            justSent = nil
        }
    }

    private func send() {
        // The latest text first (its sync may still be waiting), then ask.
        draftSync?.cancel()
        draftSync = nil
        if text != composer.draft { core.update(.setDraft(text: text)) }
        guard composer.canSend else { return }
        dictation.stop()
        core.update(.send)
        Platform.answered(false)
        justSent = text
        text = ""
        keepTypingFocus()
        Task {
            try? await Task.sleep(for: .seconds(1.5))
            justSent = nil
        }
    }

    /// On a phone the keyboard goes down to show the reply; on iPad (often
    /// with a hardware keyboard) the field stays ready for the next message.
    private func keepTypingFocus() {
        if Platform.isPhone { focused = false }
    }

    /// The sent text again, give or take the last word's correction.
    static func sameMessage(_ a: String, _ b: String) -> Bool {
        let common = zip(a, b).prefix { $0 == $1 }.count
        return abs(a.count - b.count) <= 4 && common >= min(a.count, b.count) - 8
    }

    private func pasteImage() {
        guard let image = UIPasteboard.general.image else {
            attachError = "The clipboard has no image."
            return
        }
        add(image)
    }

    private func attach(_ items: [PhotosPickerItem]) async {
        var failed = 0
        for item in items {
            guard let data = try? await item.loadTransferable(type: Data.self), let image = UIImage(data: data) else {
                failed += 1
                continue
            }
            add(image)
        }
        photos = []
        if failed > 0 { attachError = failed == 1 ? "A photo couldn't be attached." : "\(failed) photos couldn't be attached." }
    }

    /// Photos go out as JPEG (HEIC is not something every model reads),
    /// scaled to at most 2048 points on the long side. Named after the file,
    /// or "Photo 2" and so on, so several can be told apart.
    private func add(_ image: UIImage, name base: String? = nil) {
        let longest = max(image.size.width, image.size.height)
        let scale = min(1, 2048 / max(longest, 1))
        let size = CGSize(width: (image.size.width * scale).rounded(), height: (image.size.height * scale).rounded())
        let format = UIGraphicsImageRendererFormat.default()
        format.scale = 1
        let resized = UIGraphicsImageRenderer(size: size, format: format).image { _ in image.draw(in: CGRect(origin: .zero, size: size)) }
        guard let jpeg = resized.jpegData(compressionQuality: 0.85) else {
            attachError = "That image couldn't be attached."
            return
        }
        let dataUrl = "data:image/jpeg;base64,\(jpeg.base64EncodedString())"
        let taken = Set(composer.attachments.map(\.name))
        var number = composer.attachments.count + 1
        var name = "\(base ?? "Photo \(number)").jpg"
        while taken.contains(name) {
            number += 1
            name = "\(base.map { "\($0) \(number)" } ?? "Photo \(number)").jpg"
        }
        thumbnails[name] = resized.preparingThumbnail(of: CGSize(width: 90, height: 90)) ?? resized
        core.update(.addAttachment(name: name, mimeType: "image/jpeg", sizeBytes: UInt64(jpeg.count), dataUrl: dataUrl))
    }
}

/// Plan mode, effort, permissions and model as chips. Plan toggles in
/// place; the others open the options sheet at their section.
struct ModeChips: View {
    let composer: ComposerView
    let open: (ComposerOption) -> Void
    let togglePlan: () -> Void
    @Environment(\.dynamicTypeSize) private var typeSize
    /// Chips hidden past each edge: that edge fades, the other stays crisp.
    @State private var hiddenLeading = false
    @State private var hiddenTrailing = false

    private var planning: Bool { composer.interactionMode == "plan" }
    /// Large text: icons only, so the row still fits.
    private var compact: Bool { typeSize.isAccessibilitySize }

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            GlassEffectContainer(spacing: Space.xs) {
                HStack(spacing: Space.xs) {
                    if composer.showsInteractionMode || planning {
                        chip(selected: planning, tint: Tint.plan, action: togglePlan) {
                            Image(systemName: "list.bullet.clipboard")
                            if !compact { Text("Plan") }
                        }
                        .accessibilityLabel("Plan first")
                        .accessibilityValue(planning ? "On" : "Off")
                        .accessibilityAddTraits(planning ? .isSelected : [])
                    }

                    if !composer.efforts.isEmpty {
                        let index = composer.efforts.firstIndex { $0.value == composer.selectedEffort } ?? -1
                        chip(action: { open(.effort) }) {
                            EffortMeter(levels: composer.efforts.count, selected: index)
                            if !compact, index >= 0 { Text(composer.efforts[index].label) }
                        }
                        .accessibilityLabel("Effort")
                        .accessibilityValue(index >= 0 ? composer.efforts[index].label : "Default")
                    }

                    let mode = composer.runtimeMode
                    let label = composer.runtimeModes.first { $0.value == mode }?.label ?? mode
                    chip(selected: mode == "full-access", tint: Tint.caution, action: { open(.permissions) }) {
                        Image(systemName: ModeInfo.permissionIcon(mode))
                        // Only the mode that needs noticing spells itself out.
                        if !compact, mode == "full-access" { Text(ModeInfo.permissionShort(mode, label: label)) }
                    }
                    .accessibilityLabel("Permissions")
                    .accessibilityValue(label)

                    chip(action: { open(.model) }) {
                        Image(systemName: "cpu")
                        if !compact { Text(composer.selectedModel?.label ?? "Model").lineLimit(1) }
                    }
                    .accessibilityLabel("Model")
                    .accessibilityValue(composer.selectedModel?.label ?? "None")
                }
                .padding(.vertical, Space.xxs)
                .padding(.trailing, Space.l)
            }
        }
        // Clipped to the row (scrolled chips never draw beside the +
        // button), fading only at an edge with more chips past it.
        .onScrollGeometryChange(for: [Bool].self) { geo in
            let offset = geo.contentOffset.x + geo.contentInsets.leading
            return [offset > 1, offset + geo.containerSize.width < geo.contentSize.width - 1]
        } action: { _, edges in
            hiddenLeading = edges[0]
            hiddenTrailing = edges[1]
        }
        .mask {
            LinearGradient(stops: [
                .init(color: hiddenLeading ? .clear : .black, location: 0),
                .init(color: .black, location: hiddenLeading ? 0.08 : 0),
                .init(color: .black, location: hiddenTrailing ? 0.9 : 1),
                .init(color: hiddenTrailing ? .clear : .black, location: 1),
            ], startPoint: .leading, endPoint: .trailing)
        }
    }

    private func chip(selected: Bool = false, tint: Color? = nil, action: @escaping () -> Void, @ViewBuilder content: () -> some View) -> some View {
        Button(action: action) {
            HStack(spacing: Space.xs) { content() }
                .font(.footnote.weight(.medium))
                .foregroundStyle(selected ? AnyShapeStyle(.white) : AnyShapeStyle(.primary))
                .padding(.horizontal, Space.m)
                .frame(minHeight: 34)
                .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .glassChip(tint: tint, selected: selected)
    }
}

/// The camera, for a photo to attach.
struct CameraPicker: UIViewControllerRepresentable {
    let done: (UIImage?) -> Void

    func makeUIViewController(context: Context) -> UIImagePickerController {
        let picker = UIImagePickerController()
        picker.sourceType = .camera
        picker.delegate = context.coordinator
        return picker
    }

    func updateUIViewController(_ controller: UIImagePickerController, context: Context) {}

    func makeCoordinator() -> Coordinator { Coordinator(done: done) }

    final class Coordinator: NSObject, UIImagePickerControllerDelegate, UINavigationControllerDelegate {
        let done: (UIImage?) -> Void
        init(done: @escaping (UIImage?) -> Void) { self.done = done }

        func imagePickerController(_ picker: UIImagePickerController, didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]) {
            done(info[.originalImage] as? UIImage)
            picker.dismiss(animated: true)
        }

        func imagePickerControllerDidCancel(_ picker: UIImagePickerController) {
            done(nil)
            picker.dismiss(animated: true)
        }
    }
}
