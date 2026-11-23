import App
import SwiftUI

// The composer's choices (plan mode, effort, permissions, model) as views
// both apps share: the iPhone's options sheet and the Mac's chip popovers
// are built from these, so they read and behave the same.

/// How the composer's choices read: icons, short labels and a line on what
/// each permission mode lets the agent do.
enum ModeInfo {
    static func permissionIcon(_ mode: String) -> String {
        switch mode {
        case "approval-required": return "lock.fill"
        case "auto-accept-edits": return "pencil.and.list.clipboard"
        case "full-access": return "lock.open.fill"
        default: return "lock"
        }
    }

    static func permissionShort(_ mode: String, label: String) -> String {
        switch mode {
        case "approval-required": return "Ask"
        case "auto-accept-edits": return "Auto-edit"
        case "full-access": return "Full access"
        default: return label
        }
    }

    static func permissionDetail(_ mode: String) -> String {
        switch mode {
        case "approval-required": return "Asks before it runs commands or changes files."
        case "auto-accept-edits": return "Edits files on its own; asks before commands."
        case "full-access": return "Runs commands and edits files without asking."
        default: return ""
        }
    }

    static func permissionTint(_ mode: String) -> Color? {
        mode == "full-access" ? Tint.caution : nil
    }

    static let fullAccessTitle = "Give the agent full access?"
    static let fullAccessMessage = "It will run commands and change files on the server without asking first."

    /// The effort one step up (`delta` 1) or down (-1) from the chosen one,
    /// or nil at either end.
    static func effort(_ composer: ComposerView, step delta: Int) -> String? {
        guard !composer.efforts.isEmpty else { return nil }
        let current = composer.efforts.firstIndex { $0.value == composer.selectedEffort } ?? -1
        let next = current < 0 ? (delta > 0 ? 0 : composer.efforts.count - 1) : current + delta
        guard composer.efforts.indices.contains(next) else { return nil }
        return composer.efforts[next].value
    }
}

/// "Goes after the turn under way" while one runs. A view of its own: the
/// options around it don't redraw on every streamed token.
struct WorkingNote: View {
    @ObservedObject var core: Core

    var body: some View {
        if core.view.thread?.working == true { NextMessageNote() }
    }
}

/// Sections of the options; a chip opens its own.
enum ComposerOption: String, Identifiable {
    case model, effort, permissions, plan, workspace
    var id: String { rawValue }
}

/// A row of bars, filled up to the chosen effort.
struct EffortMeter: View {
    let levels: Int
    let selected: Int
    /// Grows with the text size, so it stays readable beside large labels.
    @ScaledMetric(relativeTo: .footnote) private var height: CGFloat = 12
    @ScaledMetric(relativeTo: .footnote) private var barWidth: CGFloat = 3

    var body: some View {
        HStack(alignment: .bottom, spacing: Space.xxs) {
            ForEach(0..<max(levels, 1), id: \.self) { i in
                Capsule()
                    .fill(i <= selected ? AnyShapeStyle(.tint) : AnyShapeStyle(.quaternary))
                    .frame(width: barWidth, height: height * CGFloat(i + 1) / CGFloat(max(levels, 1)) + 2)
            }
        }
        .accessibilityHidden(true)
    }
}

/// A section title in the options.
struct OptionHeader: View {
    let title: String
    let icon: String

    init(_ title: String, _ icon: String) {
        self.title = title
        self.icon = icon
    }

    var body: some View {
        Label(title, systemImage: icon)
            .font(TypeRole.sectionHeader)
            .foregroundStyle(.secondary)
    }
}

/// "Changes apply from your next message", while a turn runs.
struct NextMessageNote: View {
    var body: some View {
        Label("Changes apply from your next message.", systemImage: "clock.arrow.circlepath")
            .font(TypeRole.meta)
            .foregroundStyle(.secondary)
    }
}

/// Plan mode as one card that toggles.
struct PlanCard: View {
    let on: Bool
    let toggle: () -> Void

    var body: some View {
        Button {
            Platform.selectionChanged()
            toggle()
        } label: {
            HStack(spacing: Space.m) {
                Image(systemName: "list.bullet.clipboard")
                    .foregroundStyle(on ? Tint.plan : .secondary)
                    .iconColumn(Size.listIcon)
                VStack(alignment: .leading, spacing: Space.xxs) {
                    Text("Plan first").font(.body.weight(.medium))
                    Text("The agent proposes a plan and waits for your go-ahead before changing anything.")
                        .font(TypeRole.meta)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.leading)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: Space.s)
                // A switch, so its empty circle shows too.
                Image(systemName: on ? "checkmark.circle.fill" : "circle")
                    .foregroundStyle(on ? Tint.plan : Color.secondary)
            }
            .padding(Space.m)
            .contentShape(Rectangle())
            .cardSurface(tint: on ? Tint.plan : nil)
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(on ? .isSelected : [])
    }
}

/// Effort levels as bars side by side, filled up to the chosen one.
struct EffortPicker: View {
    let composer: ComposerView
    let select: (String) -> Void

    private var effortIndex: Int { composer.efforts.firstIndex { $0.value == composer.selectedEffort } ?? -1 }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s) {
            OptionHeader(composer.effortLabel.isEmpty ? "Effort" : composer.effortLabel, "gauge.with.dots.needle.67percent")
            HStack(spacing: Space.xs) {
                ForEach(Array(composer.efforts.enumerated()), id: \.element.value) { index, effort in
                    let filled = index <= effortIndex
                    let chosen = index == effortIndex
                    Button {
                        Platform.selectionChanged()
                        select(effort.value)
                    } label: {
                        VStack(spacing: Space.xs) {
                            Capsule()
                                .fill(filled ? AnyShapeStyle(.tint) : AnyShapeStyle(.quaternary))
                                .frame(height: 6)
                            Text(effort.label)
                                .font(.caption.weight(chosen ? .semibold : .regular))
                                .foregroundStyle(chosen ? .primary : .secondary)
                                .lineLimit(1)
                                .minimumScaleFactor(0.8)
                        }
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, Space.xs)
                        .tapTarget()
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel(effort.label)
                    .accessibilityAddTraits(chosen ? .isSelected : [])
                }
            }
            .animation(.snappy, value: effortIndex)
            Text(composer.effortLabel == "Effort" || composer.effortLabel.isEmpty
                 ? "More effort thinks longer: better on hard problems, slower and costlier."
                 : "A higher level thinks longer: better on hard problems, slower and costlier.")
                .font(TypeRole.meta)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }
}

/// The permission modes as cards with what each lets the agent do. The
/// caller decides how Full access is confirmed.
struct PermissionCards: View {
    let composer: ComposerView
    let select: (String) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s) {
            OptionHeader("Permissions", "lock.shield")
            ForEach(composer.runtimeModes, id: \.value) { mode in
                let chosen = mode.value == composer.runtimeMode
                let tint = ModeInfo.permissionTint(mode.value)
                Button {
                    guard !chosen else { return }
                    Platform.selectionChanged()
                    select(mode.value)
                } label: {
                    HStack(spacing: Space.m) {
                        Image(systemName: ModeInfo.permissionIcon(mode.value))
                            .foregroundStyle(tint ?? (chosen ? Color.accentColor : .secondary))
                            .iconColumn(Size.listIcon)
                        VStack(alignment: .leading, spacing: Space.xxs) {
                            Text(mode.label).font(.body.weight(chosen ? .semibold : .regular))
                            let detail = ModeInfo.permissionDetail(mode.value)
                            if !detail.isEmpty {
                                Text(detail)
                                    .font(TypeRole.meta)
                                    .foregroundStyle(.secondary)
                                    .multilineTextAlignment(.leading)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                        }
                        Spacer(minLength: Space.s)
                        if chosen { Image(systemName: "checkmark.circle.fill").foregroundStyle(.tint) }
                    }
                    .padding(Space.m)
                    .contentShape(Rectangle())
                    .cardSurface(tint: chosen ? (tint ?? .accentColor) : nil)
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(chosen ? .isSelected : [])
            }
        }
    }
}

/// Models: favorites first, then by provider (with a provider's state when it
/// isn't ready), with a search once the list is long. The star marks a
/// favorite (on every client); the context menu also hides a model.
struct ModelList: View {
    let composer: ComposerView
    let update: (Event) -> Void
    let select: (ModelOption) -> Void
    /// Puts the cursor in the search field when the list appears (a popover).
    var focusSearch = false
    @FocusState private var searchFocused: Bool
    /// The search as typed. Kept here, not bound to the core's copy: a view
    /// that arrives after a keystroke must not put an older query back.
    @State private var search = ""
    /// The query last sent to the core.
    @State private var sent = ""

    private var searching: Bool { !composer.modelQuery.isEmpty }
    private var showsSearch: Bool { composer.modelCount > 8 || searching }
    private var firstResult: ModelOption? {
        (composer.favoriteModels.first ?? composer.modelGroups.first(where: { !$0.models.isEmpty })?.models.first)?.option
    }

    var body: some View {
        // Lazy: an install with hundreds of models (pi, OpenCode) builds only
        // the sections on screen.
        LazyVStack(alignment: .leading, spacing: Space.s) {
            OptionHeader("Model", "cpu")
            if showsSearch {
                TextField("Search models", text: $search)
                    .textFieldStyle(.roundedBorder)
                    .focused($searchFocused)
                    .onSubmit {
                        guard let first = firstResult else { return }
                        Platform.selectionChanged()
                        select(first)
                    }
            }
            if !composer.favoriteModels.isEmpty {
                ModelSection(title: "Favorites", icon: "star.fill", status: "ready", rows: composer.favoriteModels,
                             selected: composer.selectedModel, update: update, select: select)
            }
            ForEach(composer.modelGroups, id: \.instanceId) { group in
                ModelSection(title: group.provider, icon: nil, status: group.status, rows: group.models,
                             selected: composer.selectedModel, update: update, select: select) {
                    // A provider that isn't ready says why (it may list no models at all).
                    if !group.message.isEmpty {
                        Text(group.message)
                            .font(TypeRole.meta)
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                            .padding(.horizontal, Space.m)
                            .padding(.vertical, Space.s)
                    }
                    if group.hiddenCount > 0 && !searching {
                        Button(composer.showHiddenModels ? "Hide Hidden Models" : "Show \(group.hiddenCount) Hidden") {
                            update(.setShowHiddenModels(show: !composer.showHiddenModels))
                        }
                        .buttonStyle(.borderless)
                        .font(.footnote)
                        .padding(.horizontal, Space.m)
                        .padding(.vertical, Space.s)
                    }
                }
            }
            if searching && composer.favoriteModels.isEmpty && composer.modelGroups.isEmpty {
                ContentUnavailableView.search(text: composer.modelQuery)
            }
        }
        .onChange(of: search) { _, value in
            guard value != sent else { return }
            sent = value
            update(.setModelQuery(query: value))
        }
        .onChange(of: composer.modelQuery) { _, value in
            // Cleared or changed by the core itself (a model picked), not an
            // echo of what was typed.
            if value != sent {
                sent = value
                search = value
            }
        }
        .onAppear {
            if !composer.modelQuery.isEmpty { update(.setModelQuery(query: "")) }
            if composer.showHiddenModels { update(.setShowHiddenModels(show: false)) }
            if focusSearch && composer.modelCount > 8 { searchFocused = true }
        }
    }
}

/// A card of models under a title (a provider, or Favorites).
private struct ModelSection<Footer: View>: View {
    let title: String
    let icon: String?
    let status: String
    let rows: [ModelRowView]
    let selected: ModelOption?
    let update: (Event) -> Void
    let select: (ModelOption) -> Void
    @ViewBuilder var footer: () -> Footer
    /// A long section (a provider listing hundreds of models) shows its first
    /// rows until asked for the rest; search narrows it too.
    @State private var showsAll = false
    private static var limit: Int { 30 }

    init(title: String, icon: String?, status: String, rows: [ModelRowView], selected: ModelOption?,
         update: @escaping (Event) -> Void, select: @escaping (ModelOption) -> Void,
         @ViewBuilder footer: @escaping () -> Footer = { EmptyView() }) {
        self.title = title
        self.icon = icon
        self.status = status
        self.rows = rows
        self.selected = selected
        self.update = update
        self.select = select
        self.footer = footer
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                if let icon {
                    Label(title, systemImage: icon).font(TypeRole.sectionHeader)
                } else {
                    Text(title).font(TypeRole.sectionHeader)
                }
                if status != "ready" {
                    Text(status.capitalized)
                        .foregroundStyle(Tint.caution)
                        .badgeCapsule(tint: Tint.caution)
                }
            }
            .padding(.horizontal, Space.m)
            .padding(.vertical, Space.s)
            // The chosen model stays in view, however far down it is.
            let chosenAt = rows.firstIndex { $0.chosen } ?? 0
            let cap = max(Self.limit, chosenAt + 1)
            let shown = showsAll || rows.count <= cap ? rows[...] : rows.prefix(cap)
            ForEach(shown, id: \.option) { row in
                ModelRow(row: row, chosen: row.chosen, update: update, select: select)
                if row.option != shown.last?.option { Divider().padding(.leading, Space.m) }
            }
            if shown.count < rows.count {
                Divider().padding(.leading, Space.m)
                Button("Show All \(rows.count) Models") { showsAll = true }
                    .buttonStyle(.borderless)
                    .font(.footnote)
                    .padding(.horizontal, Space.m)
                    .padding(.vertical, Space.s)
            }
            footer()
        }
        .cardSurface()
    }
}

/// A model: its name and second line, a checkmark when chosen, and a star.
private struct ModelRow: View {
    let row: ModelRowView
    let chosen: Bool
    let update: (Event) -> Void
    let select: (ModelOption) -> Void
    @State private var hovering = false

    private func favorite(_ on: Bool) {
        Platform.selectionChanged()
        update(.setModelFavorite(instanceId: row.option.instanceId, model: row.option.model, favorite: on))
    }

    private func hide(_ on: Bool) {
        update(.setModelHidden(instanceId: row.option.instanceId, model: row.option.model, hidden: on))
    }

    var body: some View {
        HStack(spacing: Space.s) {
            Button {
                Platform.selectionChanged()
                select(row.option)
            } label: {
                HStack {
                    VStack(alignment: .leading, spacing: Space.xxs) {
                        Text(row.name).fontWeight(chosen ? .semibold : .regular)
                        if !row.detail.isEmpty {
                            Text(row.detail).font(TypeRole.meta).foregroundStyle(.secondary)
                        }
                    }
                    Spacer(minLength: Space.s)
                    if chosen { Image(systemName: "checkmark.circle.fill").foregroundStyle(.tint) }
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityAddTraits(chosen ? .isSelected : [])
            Button { favorite(!row.favorite) } label: {
                Image(systemName: row.favorite ? "star.fill" : "star")
                    .foregroundStyle(row.favorite ? AnyShapeStyle(Tint.favorite) : AnyShapeStyle(.secondary))
                    // Tall enough to tap, reaching into the row's padding rather than making it taller.
                    .tapTarget(inPadding: Self.verticalInset, width: true)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help(row.favorite ? "Remove from Favorites" : "Add to Favorites")
            .accessibilityLabel(row.favorite ? "Remove \(row.name) from Favorites" : "Add \(row.name) to Favorites")
            .opacity(showsStar ? 1 : 0)
        }
        .padding(.horizontal, Space.m)
        .padding(.vertical, Self.verticalInset)
        .opacity(row.hidden ? 0.5 : 1)
        .hoverHighlight(Radius.chip)
        #if os(macOS)
        .onHover { hovering = $0 }
        #endif
        .contextMenu {
            Button(row.favorite ? "Remove from Favorites" : "Add to Favorites", systemImage: row.favorite ? "star.slash" : "star") {
                favorite(!row.favorite)
            }
            Button(row.hidden ? "Show Model" : "Hide Model", systemImage: row.hidden ? "eye" : "eye.slash") { hide(!row.hidden) }
        }
    }

    /// A row's top and bottom padding: roomier under a finger.
    static var verticalInset: CGFloat { Platform.isPhone ? Space.m : Space.s }

    /// On a Mac the empty star shows under the pointer; touch screens always show it.
    private var showsStar: Bool {
        #if os(macOS)
        row.favorite || hovering
        #else
        true
        #endif
    }
}

/// The model's other options (fast mode, context window, agent…): a switch
/// sends `true` or `false`, a list the choice's id.
struct TraitControls: View {
    let traits: [TraitView]
    let set: (_ id: String, _ value: String) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s) {
            OptionHeader("Model Options", "slider.horizontal.3")
            // One card, like the model sections beside it.
            VStack(alignment: .leading, spacing: 0) {
                ForEach(Array(traits.enumerated()), id: \.element.id) { index, item in
                    if index > 0 { Divider().padding(.leading, Space.m) }
                    control(item)
                        .padding(.horizontal, Space.m)
                        .padding(.vertical, Platform.isPhone ? Space.m : Space.s)
                }
            }
            .cardSurface()
        }
    }

    @ViewBuilder
    private func control(_ item: TraitView) -> some View {
        if item.kind == "switch" {
            Toggle(isOn: Binding(get: { item.on }, set: { set(item.id, $0 ? "true" : "false") })) { label(item) }
        } else {
            HStack(alignment: .firstTextBaseline) {
                label(item)
                Spacer(minLength: Space.s)
                Picker(item.label, selection: Binding(get: { item.value }, set: { set(item.id, $0) })) {
                    ForEach(item.choices, id: \.value) { Text($0.label).tag($0.value) }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .fixedSize()
            }
        }
    }

    private func label(_ item: TraitView) -> some View {
        VStack(alignment: .leading, spacing: Space.xxs) {
            Text(item.label)
            if !item.description.isEmpty {
                Text(item.description)
                    .font(TypeRole.meta)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }
}

/// Drafts put aside: stash the one being written, take one back, or drop it.
struct StashMenuItems: View {
    let stash: [String]
    let hasDraft: Bool
    /// Stashes the draft as typed (the composer syncs it first).
    let stashDraft: () -> Void
    let update: (Event) -> Void

    var body: some View {
        Button(action: stashDraft) { Label("Stash Draft", systemImage: "tray.and.arrow.down") }
            .disabled(!hasDraft)
        if !stash.isEmpty {
            Menu {
                ForEach(Array(stash.enumerated()), id: \.offset) { index, entry in
                    Menu(entry.isEmpty ? "Untitled draft" : entry) {
                        Button { update(.restoreStash(index: UInt32(index))) } label: { Label("Restore", systemImage: "tray.and.arrow.up") }
                        Button(role: .destructive) { update(.dropStash(index: UInt32(index))) } label: { Label("Delete", systemImage: "trash") }
                    }
                }
            } label: { Label("Stash (\(stash.count))", systemImage: "tray.full") }
        }
    }
}

extension SlashCommandView {
    /// The app's own commands (`/model`, `/plan`…) are marked apart from the provider's.
    var isBuiltin: Bool { kind == "builtin" }
}
