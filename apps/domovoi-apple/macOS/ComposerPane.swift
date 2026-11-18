import App
import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// What the menu bar asks of the composer: focus it, open one of its
/// popovers, confirm Full access. The composer on screen carries it out.
@MainActor
@Observable
final class ComposerCommands {
    static let shared = ComposerCommands()

    /// Bumped to put the cursor in the message field.
    var focusToken = 0
    /// The chip whose popover is open.
    var open: ComposerOption?
    /// Asking before the agent gets full access.
    var confirmFullAccess = false

    func focus() { focusToken &+= 1 }

    /// Full access asks first; the others switch at once.
    func setPermissions(_ mode: String, core: Core) {
        if mode == "full-access" {
            open = nil
            // After the popover has gone, so the dialog has a window.
            DispatchQueue.main.async { self.confirmFullAccess = true }
        } else {
            core.update(.setRuntimeMode(mode: mode))
        }
    }

    func togglePlan(core: Core) {
        core.update(.setInteractionMode(mode: core.view.composer.interactionMode == "plan" ? "default" : "plan"))
    }

    func stepEffort(_ delta: Int, core: Core) {
        if let value = ModeInfo.effort(core.view.composer, step: delta) { core.update(.selectEffort(value: value)) }
    }
}

/// The Mac composer: a floating glass panel in the reading column, with
/// the message field, chips for plan mode, effort, permissions and model
/// (each opening a popover), attachments, @ mentions, and one button that
/// is Send or Stop as the turn needs.
struct ComposerPane: View {
    @ObservedObject var core: Core
    @State private var text = ""
    /// Small pictures of the attached images, by attachment name.
    @State private var thumbnails: [String: NSImage] = [:]
    /// An image that couldn't be attached, shown under the field for a moment.
    @State private var attachError: String?
    @State private var dropTargeted = false
    private let commands = ComposerCommands.shared

    private var composer: ComposerView { core.view.composer }
    private var planning: Bool { composer.interactionMode == "plan" }
    private var typed: Bool { !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s) {
            if let mention = composer.mention, !mention.suggestions.isEmpty {
                mentions(mention)
                    .transition(.opacity.combined(with: .move(edge: .bottom)))
            } else if !composer.slashCommands.isEmpty {
                slashCommands(composer.slashCommands)
                    .transition(.opacity.combined(with: .move(edge: .bottom)))
            }
            panel
        }
        .padding(.horizontal, Space.gutter)
        .padding(.top, Space.xs)
        .padding(.bottom, Space.m)
        .frame(maxWidth: Space.column)
        .frame(maxWidth: .infinity)
        .animation(.snappy, value: composer.mention?.suggestions.isEmpty ?? true)
        .animation(.snappy, value: composer.slashCommands.isEmpty)
        .onChange(of: text) { _, value in
            if value != composer.draft { core.update(.setDraft(text: value)) }
        }
        .onChange(of: composer.draftRevision) { _, _ in reload() }
        .onChange(of: core.view.threadId) { _, _ in
            reload()
            commands.focus()
        }
        .background { ProjectWatcher(core: core, changed: reload) }
        .onChange(of: composer.attachments.map(\.name)) { _, names in
            thumbnails = thumbnails.filter { names.contains($0.key) }
        }
        .onAppear { reload() }
        .task(id: attachError) {
            guard attachError != nil else { return }
            try? await Task.sleep(for: .seconds(4))
            attachError = nil
        }
        .confirmationDialog(ModeInfo.fullAccessTitle, isPresented: Binding(
            get: { commands.confirmFullAccess },
            set: { commands.confirmFullAccess = $0 }
        )) {
            Button("Allow Full Access", role: .destructive) { core.update(.setRuntimeMode(mode: "full-access")) }
        } message: {
            Text(ModeInfo.fullAccessMessage)
        }
    }

    // MARK: Panel

    private var panel: some View {
        let shape = RoundedRectangle(cornerRadius: Radius.panel, style: .continuous)
        return VStack(alignment: .leading, spacing: Space.s) {
            ProviderNotice(core: core)
            if !composer.attachments.isEmpty { attachments }
            // Sizes itself to its text; starts flush with the + button.
            ComposerTextView(
                text: $text,
                placeholder: planning ? "Describe what to plan…" : composer.placeholder,
                focusToken: commands.focusToken,
                handleKey: handleKey,
                submit: send,
                takeImages: takeImages
            )
            if let attachError {
                Label(attachError, systemImage: "exclamationmark.triangle.fill")
                    .font(TypeRole.meta)
                    .foregroundStyle(Tint.caution)
                    .transition(.opacity)
            }
            HStack(alignment: .center, spacing: Space.s) {
                attachButton
                // A narrow window scrolls the chips instead of squeezing them.
                ScrollView(.horizontal, showsIndicators: false) {
                    chips.padding(.vertical, Space.xxs)
                }
                .scrollBounceBehavior(.basedOnSize)
                .fixedSize(horizontal: false, vertical: true)
                actionButtons
            }
        }
        // The chips' own vertical padding (room for their glass) comes off
        // the bottom, so the + button sits as far from the bottom as the left.
        .padding([.horizontal, .top], Space.m)
        .padding(.bottom, Space.m - Space.xxs)
        .glassPanel(in: shape, tint: planning ? Tint.plan.opacity(0.3) : nil)
        .overlay {
            // Plan mode shows in the panel's edge, not only its chip; a drag
            // over it, in the accent colour.
            shape
                .strokeBorder(dropTargeted ? Color.accentColor : Tint.plan.opacity(planning ? 0.55 : 0), lineWidth: dropTargeted ? 2 : 1)
                .allowsHitTesting(false)
        }
        .onDrop(of: [.fileURL, .image], isTargeted: $dropTargeted, perform: drop)
        .animation(.snappy, value: planning)
        .animation(.snappy, value: composer.canStop)
        .animation(.snappy, value: composer.sending)
        .animation(.snappy, value: attachError)
        .animation(.snappy, value: dropTargeted)
    }

    private var attachButton: some View {
        Button(action: pickImages) {
            Image(systemName: "plus")
                .font(.callout.weight(.semibold))
                .frame(width: 28, height: 28)
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .glassChip(in: Circle())
        .help("Attach images (or paste or drop them)")
        .accessibilityLabel("Attach images")
        .contextMenu {
            Button("Choose Images…", action: pickImages)
            Button("Paste Image") { _ = takeImages(.general) }
                .disabled(!ImagePasteboard.hasImages(.general))
        }
    }

    // MARK: Chips

    private var chips: some View {
        // One snapshot for the whole row: a chip's label is drawn later, in
        // the chip's own body, and must not read a newer view (another
        // model's shorter effort list) through `core.view`.
        let composer = composer
        let planning = composer.interactionMode == "plan"
        return GlassGroup(spacing: Space.xs) {
            HStack(spacing: Space.xs) {
                if composer.isNewThread { ProjectChip(core: core) }

                if composer.showsInteractionMode || planning {
                    ComposerChip(selected: planning, tint: Tint.plan, help: "Plan first, then act (⇧⇥)", action: { commands.togglePlan(core: core) }) {
                        Image(systemName: "list.bullet.clipboard")
                        Text("Plan")
                    }
                    .accessibilityLabel("Plan first")
                    .accessibilityValue(planning ? "On" : "Off")
                    .accessibilityAddTraits(planning ? .isSelected : [])
                }

                if !composer.efforts.isEmpty {
                    // Values only: the label closure runs after this body.
                    let levels = composer.efforts.count
                    let index = composer.efforts.firstIndex { $0.value == composer.selectedEffort } ?? -1
                    let label = index >= 0 ? composer.efforts[index].label : nil
                    ComposerChip(help: "Reasoning effort (⌘[ and ⌘])", action: { toggle(.effort) }) {
                        EffortMeter(levels: levels, selected: index)
                        Text(label ?? "Effort")
                    }
                    .popover(isPresented: shown(.effort), arrowEdge: .top) { OptionPopover(core: core, option: .effort) }
                    .accessibilityLabel("Effort")
                    .accessibilityValue(label ?? "Default")
                }

                let mode = composer.runtimeMode
                let label = composer.runtimeModes.first { $0.value == mode }?.label ?? mode
                ComposerChip(selected: mode == "full-access", tint: Tint.caution, help: "Permissions: \(label)", action: { toggle(.permissions) }) {
                    Image(systemName: ModeInfo.permissionIcon(mode))
                    Text(ModeInfo.permissionShort(mode, label: label))
                }
                .popover(isPresented: shown(.permissions), arrowEdge: .top) { OptionPopover(core: core, option: .permissions) }
                .accessibilityLabel("Permissions")
                .accessibilityValue(label)

                ComposerChip(help: composer.selectedModel.map { "Model: \($0.label) (⌘⇧M)" } ?? "Model (⌘⇧M)", action: { toggle(.model) }) {
                    Image(systemName: "cpu")
                    Text(composer.selectedModel?.label ?? "Choose a model").lineLimit(1).fixedSize()
                }
                .popover(isPresented: shown(.model), arrowEdge: .top) { OptionPopover(core: core, option: .model) }
                .accessibilityLabel("Model")
                .accessibilityValue(composer.selectedModel?.label ?? "None")

                if composer.isNewThread {
                    ComposerChip(help: "Where the thread works", action: { toggle(.workspace) }) {
                        if composer.workspaceMode == "worktree" {
                            Image(systemName: "arrow.triangle.branch")
                            Text("Worktree · \(composer.baseBranch ?? "HEAD")").lineLimit(1).fixedSize()
                        } else {
                            Image(systemName: "folder")
                            Text("Current checkout").lineLimit(1).fixedSize()
                        }
                    }
                    .popover(isPresented: shown(.workspace), arrowEdge: .top) { OptionPopover(core: core, option: .workspace) }
                }
            }
        }
        .fixedSize(horizontal: false, vertical: true)
    }

    private func toggle(_ option: ComposerOption) {
        commands.open = commands.open == option ? nil : option
    }

    private func shown(_ option: ComposerOption) -> Binding<Bool> {
        Binding(
            get: { commands.open == option },
            set: { if !$0, commands.open == option { commands.open = nil } }
        )
    }

    /// Send, a spinner while it goes out, or Stop while a turn runs; with
    /// text typed during a turn, Send (it queues) beside a small Stop.
    @ViewBuilder private var actionButtons: some View {
        HStack(spacing: Space.s) {
            if composer.canStop {
                Button { core.update(.interrupt) } label: {
                    Image(systemName: "stop.fill")
                        .font(typed ? .caption2 : .callout)
                        .foregroundStyle(.white)
                        .frame(width: typed ? 22 : 28, height: typed ? 22 : 28)
                        .background(Circle().fill(Tint.danger))
                        .contentShape(Circle())
                }
                .buttonStyle(.plain)
                .help("Stop (⌘.)")
                .accessibilityLabel("Stop")
                .transition(.scale.combined(with: .opacity))
            }
            if composer.canSteer, typed {
                // Into the turn under way, rather than after it.
                Button(action: steer) {
                    Label("Send Now", systemImage: "arrow.turn.down.right")
                        .font(TypeRole.meta.weight(.semibold))
                        .padding(.horizontal, Space.s)
                        .frame(height: 24)
                        .background(Capsule().strokeBorder(Color.accentColor.opacity(0.6)))
                        .contentShape(Capsule())
                }
                .buttonStyle(.plain)
                .foregroundStyle(.tint)
                .keyboardShortcut(.return, modifiers: .command)
                .help("Send into the turn under way (⌘↩)")
                .transition(.scale.combined(with: .opacity))
            }
            if !composer.canStop || typed {
                Button(action: send) {
                    Group {
                        if composer.sending {
                            ProgressView().controlSize(.small).tint(.white)
                        } else {
                            Image(systemName: "arrow.up").font(.callout.weight(.bold))
                        }
                    }
                    .foregroundStyle(.white)
                    .frame(width: 28, height: 28)
                    .background(Circle().fill(planning ? Tint.plan : Color.accentColor))
                    .contentShape(Circle())
                    .opacity(composer.canSend || composer.sending ? 1 : 0.35)
                }
                .buttonStyle(.plain)
                .disabled(!composer.canSend)
                .help(composer.canStop ? "Send after this turn (↩)" : "Send (↩)")
                .accessibilityLabel(composer.sending ? "Sending" : "Send")
                .transition(.scale.combined(with: .opacity))
            }
        }
    }

    // MARK: Attachments

    private var attachments: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: Space.s) {
                ForEach(composer.attachments, id: \.id) { a in
                    HStack(spacing: Space.s) {
                        Group {
                            if let image = thumbnails[a.name] {
                                Image(nsImage: image).resizable().scaledToFill()
                            } else {
                                Image(systemName: "photo").foregroundStyle(.secondary)
                            }
                        }
                        .frame(width: 28, height: 28)
                        .clipShape(RoundedRectangle(cornerRadius: Radius.chip - 2, style: .continuous))
                        VStack(alignment: .leading, spacing: 0) {
                            Text(a.name).lineLimit(1)
                            Text(a.sizeLabel).foregroundStyle(.secondary)
                        }
                        if a.uploading {
                            ProgressView().controlSize(.mini)
                        } else {
                            Button { core.update(.removeAttachment(id: a.id)) } label: { Image(systemName: "xmark.circle.fill") }
                                .buttonStyle(.plain)
                                .foregroundStyle(.secondary)
                                .help("Remove")
                                .accessibilityLabel("Remove \(a.name)")
                        }
                    }
                    .font(.caption)
                    .padding(Space.xs)
                    .padding(.trailing, Space.xs)
                    .cardSurface(Radius.chip + 2)
                }
            }
        }
    }

    // MARK: Mentions

    private func mentions(_ mention: MentionView) -> some View {
        VStack(alignment: .leading, spacing: Space.xxs) {
            ForEach(Array(mention.suggestions.enumerated()), id: \.offset) { index, entry in
                Button { core.update(.pickMention(path: entry.path)) } label: {
                    HStack(spacing: Space.s) {
                        Image(systemName: entry.directory ? "folder" : "doc")
                            .foregroundStyle(.secondary)
                            .frame(width: 16)
                        Text(entry.name)
                        Text(entry.path).foregroundStyle(.secondary).lineLimit(1).truncationMode(.head)
                        Spacer(minLength: 0)
                    }
                    .padding(.horizontal, Space.s)
                    .padding(.vertical, Space.tight)
                    .contentShape(Rectangle())
                    .hoverHighlight(Radius.chip, active: index == Int(mention.highlighted))
                }
                .buttonStyle(.plain)
            }
            Text("↑↓ choose · ↩ or ⇥ insert · esc close")
                .font(.caption2)
                .foregroundStyle(.tertiary)
                .padding(.horizontal, Space.s)
                .padding(.top, Space.xxs)
        }
        .font(.callout)
        .padding(Space.xs)
        .glassPanel(in: RoundedRectangle(cornerRadius: Radius.card, style: .continuous))
    }

    /// Commands for a draft that is `/` and a word; ⇥ or ↩ takes the first.
    private func slashCommands(_ list: [SlashCommandView]) -> some View {
        VStack(alignment: .leading, spacing: Space.xxs) {
            ForEach(Array(list.enumerated()), id: \.offset) { index, command in
                Button { core.update(.pickSlashCommand(name: command.name)) } label: {
                    HStack(spacing: Space.s) {
                        Text("/\(command.name)").font(.callout.monospaced())
                        if !command.description.isEmpty {
                            Text(command.description).foregroundStyle(.secondary).lineLimit(1)
                        }
                        Spacer(minLength: 0)
                    }
                    .padding(.horizontal, Space.s)
                    .padding(.vertical, Space.tight)
                    .contentShape(Rectangle())
                    .hoverHighlight(Radius.chip, active: index == 0)
                }
                .buttonStyle(.plain)
            }
            Text("⇥ or ↩ insert the first · keep typing to narrow")
                .font(.caption2)
                .foregroundStyle(.tertiary)
                .padding(.horizontal, Space.s)
                .padding(.top, Space.xxs)
        }
        .font(.callout)
        .padding(Space.xs)
        .glassPanel(in: RoundedRectangle(cornerRadius: Radius.card, style: .continuous))
    }

    // MARK: Actions

    private func reload() {
        text = composer.draft
    }

    private func steer() {
        core.update(.setDraft(text: text))
        core.update(.steer)
    }

    private func send() {
        guard composer.canSend else { return }
        core.update(.setDraft(text: text))
        core.update(.send)
    }

    private func handleKey(_ key: ComposerKey) -> Bool {
        if composer.mention == nil, let first = composer.slashCommands.first, key == .tab || key == .returnKey {
            core.update(.pickSlashCommand(name: first.name))
            return true
        }
        if let mention = composer.mention, !mention.suggestions.isEmpty {
            switch key {
            case .up:
                core.update(.moveMention(delta: -1))
                return true
            case .down:
                core.update(.moveMention(delta: 1))
                return true
            case .tab, .returnKey:
                core.update(.setDraft(text: text))
                core.update(.pickMention(path: ""))
                return true
            case .escape:
                core.update(.dismissMention)
                return true
            case .backtab:
                break
            }
        }
        switch key {
        case .backtab:
            guard composer.showsInteractionMode || planning else { return false }
            commands.togglePlan(core: core)
            return true
        case .escape:
            guard commands.open != nil else { return false }
            commands.open = nil
            return true
        default:
            return false
        }
    }

    private func pickImages() {
        let panel = NSOpenPanel()
        panel.allowedContentTypes = [.image]
        panel.allowsMultipleSelection = true
        panel.message = "Choose images to attach"
        if panel.runModal() == .OK {
            for url in panel.urls {
                Task { await attach({ try? Data(contentsOf: url) }, name: url.lastPathComponent) }
            }
        }
    }

    /// Images pasted into or dropped on the field.
    private func takeImages(_ board: NSPasteboard) -> Bool {
        let images = ImagePasteboard.read(board)
        for image in images {
            Task { await attach(image.data, name: image.name) }
        }
        return !images.isEmpty
    }

    private func drop(_ providers: [NSItemProvider]) -> Bool {
        let usable = providers.filter {
            $0.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier) || $0.hasItemConformingToTypeIdentifier(UTType.image.identifier)
        }
        guard !usable.isEmpty else { return false }
        for provider in usable {
            if provider.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier) {
                _ = provider.loadObject(ofClass: URL.self) { url, _ in
                    guard let url else { return }
                    Task { @MainActor in await attach({ try? Data(contentsOf: url) }, name: url.lastPathComponent) }
                }
            } else {
                let name = provider.suggestedName ?? "image"
                _ = provider.loadDataRepresentation(for: .image) { data, _ in
                    Task { @MainActor in await attach({ data }, name: name) }
                }
            }
        }
        return true
    }

    /// Reads and scales the image away from the main thread, then adds it.
    private func attach(_ load: @escaping @Sendable () -> Data?, name: String) async {
        let prepared = await Task.detached(priority: .userInitiated) {
            load().flatMap { ImageAttachment.prepare($0, name: name) }
        }.value
        guard let prepared else {
            attachError = "“\(name)” isn't an image that can be attached."
            return
        }
        let unique = uniqueName(prepared.name)
        if let thumbnail = prepared.thumbnail {
            thumbnails[unique] = NSImage(cgImage: thumbnail, size: .zero)
        }
        let dataUrl = "data:\(prepared.mimeType);base64,\(prepared.data.base64EncodedString())"
        core.update(.addAttachment(name: unique, mimeType: prepared.mimeType, sizeBytes: UInt64(prepared.data.count), dataUrl: dataUrl))
    }

    /// The name, numbered when an attachment already has it (thumbnails
    /// are kept by name).
    private func uniqueName(_ name: String) -> String {
        let taken = Set(composer.attachments.map(\.name)).union(thumbnails.keys)
        guard taken.contains(name) else { return name }
        let stem = (name as NSString).deletingPathExtension
        let ext = (name as NSString).pathExtension
        var n = 2
        while taken.contains(ext.isEmpty ? "\(stem) \(n)" : "\(stem) \(n).\(ext)") { n += 1 }
        return ext.isEmpty ? "\(stem) \(n)" : "\(stem) \(n).\(ext)"
    }
}

/// Reloads the draft when the new thread's project changes. A view of its
/// own, so the composer doesn't redraw on every sidebar change.
private struct ProjectWatcher: View {
    @ObservedObject var core: Core
    let changed: () -> Void

    var body: some View {
        Color.clear.onChange(of: core.view.sidebar.selectedProjectId) { _, _ in changed() }
    }
}

/// A chip in the composer's row.
struct ComposerChip<Content: View>: View {
    var selected = false
    var tint: Color?
    let help: String
    let action: () -> Void
    @ViewBuilder let content: () -> Content

    var body: some View {
        Button(action: action) {
            HStack(spacing: Space.tight) { content() }
                .font(.callout.weight(.medium))
                .foregroundStyle(selected ? AnyShapeStyle(.white) : AnyShapeStyle(.primary))
                .padding(.horizontal, Space.s + Space.xxs)
                .frame(minHeight: 28)
                .contentShape(Capsule())
                .hoverHighlight(14)
        }
        .buttonStyle(.plain)
        .modifier(ChipSurface(selected: selected, tint: tint))
        .help(help)
    }
}

/// Glass for a chip; a solid fill in its tint when on, which reads at a
/// glance where tinted glass stays faint.
private struct ChipSurface: ViewModifier {
    let selected: Bool
    let tint: Color?

    func body(content: Content) -> some View {
        if selected {
            content.background(Capsule().fill((tint ?? .accentColor).gradient))
        } else {
            content.glassChip()
        }
    }
}

/// The project a new thread starts in, when there is more than one.
private struct ProjectChip: View {
    @ObservedObject var core: Core
    @State private var open = false

    private var sidebar: SidebarView { core.view.sidebar }

    var body: some View {
        let projects = sidebar.projects.filter { !$0.isScratch }
        if projects.count > 1 {
            let current = projects.first { $0.id == sidebar.selectedProjectId }
            ComposerChip(help: "Project", action: { open.toggle() }) {
                Text(current?.monogram ?? "?")
                    .font(.system(size: 9, weight: .bold))
                    .frame(width: 16, height: 16)
                    .background(Color.accentColor.opacity(0.25), in: RoundedRectangle(cornerRadius: Space.xs))
                Text(current?.title ?? "Project").lineLimit(1)
            }
            .frame(maxWidth: 200)
            .popover(isPresented: $open, arrowEdge: .top) {
                VStack(alignment: .leading, spacing: Space.xxs) {
                    OptionHeader("Project", "folder").padding(.horizontal, Space.s).padding(.bottom, Space.xs)
                    ForEach(projects, id: \.id) { project in
                        Button {
                            core.update(.newThread(projectId: project.id))
                            open = false
                        } label: {
                            HStack(spacing: Space.s) {
                                Text(project.title).lineLimit(1)
                                Spacer()
                                if project.id == sidebar.selectedProjectId { Image(systemName: "checkmark").foregroundStyle(.tint) }
                            }
                            .padding(.horizontal, Space.s)
                            .padding(.vertical, Space.tight)
                            .contentShape(Rectangle())
                            .hoverHighlight(Radius.chip)
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(Space.s)
                .frame(width: 260)
            }
        }
    }
}

/// A chip's popover: the shared option views, sized for a pointer.
private struct OptionPopover: View {
    @ObservedObject var core: Core
    let option: ComposerOption
    private let commands = ComposerCommands.shared

    private var composer: ComposerView { core.view.composer }

    @ViewBuilder var body: some View {
        let list = ScrollView {
            LazyVStack(alignment: .leading, spacing: Space.m) {
                // Its own view: reading the thread here would redraw the whole
                // popover (hundreds of model rows) on every streamed token.
                if option != .workspace { WorkingNote(core: core) }
                switch option {
                case .effort:
                    EffortPicker(composer: composer) { core.update(.selectEffort(value: $0)) }
                case .permissions:
                    PermissionCards(composer: composer) { commands.setPermissions($0, core: core) }
                case .model:
                    ModelList(composer: composer, update: { core.update($0) }, select: { model in
                        core.update(.selectModel(instanceId: model.instanceId, model: model.model))
                        commands.open = nil
                    }, focusSearch: true)
                case .plan where composer.showsInteractionMode || composer.interactionMode == "plan":
                    PlanCard(on: composer.interactionMode == "plan") { commands.togglePlan(core: core) }
                case .plan:
                    EmptyView()
                case .workspace:
                    workspace
                }
            }
            .padding(Space.l)
        }
        .scrollBounceBehavior(.basedOnSize)
        .frame(width: 360)
        // Sized to its content, up to 480, except the model list: measuring
        // that for its height would build every row of a long list.
        if option == .model {
            list.frame(height: 480)
        } else {
            list.frame(maxHeight: 480).fixedSize(horizontal: false, vertical: true)
        }
    }

    private var workspace: some View {
        VStack(alignment: .leading, spacing: Space.s) {
            OptionHeader("Workspace", "folder")
            row("Current checkout", detail: "Works in the project's folder as it is.", icon: "folder", chosen: composer.workspaceMode != "worktree") {
                core.update(.setWorkspaceMode(mode: "local"))
            }
            row("New worktree", detail: "A separate checkout on its own branch, so this one stays untouched.", icon: "arrow.triangle.branch", chosen: composer.workspaceMode == "worktree") {
                core.update(.setWorkspaceMode(mode: "worktree"))
            }
            if composer.workspaceMode == "worktree", !composer.branches.isEmpty {
                OptionHeader("Start from", "arrow.triangle.branch").padding(.top, Space.s)
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(composer.branches, id: \.self) { branch in
                        Button { core.update(.setBaseBranch(branch: branch)) } label: {
                            HStack {
                                Text(branch).font(TypeRole.mono).lineLimit(1)
                                Spacer()
                                if branch == composer.baseBranch { Image(systemName: "checkmark").foregroundStyle(.tint) }
                            }
                            .padding(.horizontal, Space.m)
                            .padding(.vertical, Space.s)
                            .contentShape(Rectangle())
                            .hoverHighlight(Radius.chip)
                        }
                        .buttonStyle(.plain)
                    }
                }
                .cardSurface()
            }
        }
    }

    private func row(_ title: String, detail: String, icon: String, chosen: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: Space.m) {
                Image(systemName: icon).foregroundStyle(chosen ? Color.accentColor : .secondary).frame(width: 28)
                VStack(alignment: .leading, spacing: Space.xxs) {
                    Text(title).font(.body.weight(chosen ? .semibold : .regular))
                    Text(detail).font(TypeRole.meta).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: Space.s)
                if chosen { Image(systemName: "checkmark").foregroundStyle(.tint).fontWeight(.semibold) }
            }
            .padding(Space.m)
            .contentShape(Rectangle())
            .cardSurface(tint: chosen ? .accentColor : nil)
        }
        .buttonStyle(.plain)
    }
}
