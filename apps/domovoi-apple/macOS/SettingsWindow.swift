import App
import AppKit
import SwiftUI

/// The selected Settings section (shared so the Remote Access menu item and
/// snapshots can open a section directly).
@MainActor
final class SettingsTab: ObservableObject {
    static let shared = SettingsTab()
    @Published var selection = "general"
}

enum SettingsSectionKind: String, CaseIterable, Identifiable {
    // This Mac (device-local)
    case appearance
    case servers
    // Per-server (server-stored + per-server local companion)
    case general
    case providers
    case projects
    case scheduled
    case keybindings
    case access
    case archived
    case maintenance
    case companion

    var id: String { rawValue }

    static let clientSections: [Self] = [.appearance, .servers]
    static let serverSections: [Self] = [
        .general, .providers, .projects, .scheduled, .keybindings, .access, .archived, .maintenance, .companion,
    ]

    var title: String {
        switch self {
        case .appearance: "Appearance"
        case .servers: "Servers"
        case .general: "General"
        case .providers: "Providers"
        case .projects: "Projects"
        case .scheduled: "Scheduled Tasks"
        case .keybindings: "Keybindings"
        case .access: "Access & Remote"
        case .archived: "Archived Threads"
        case .maintenance: "Maintenance"
        case .companion: "Companion"
        }
    }

    var symbol: String {
        switch self {
        case .appearance: "paintbrush"
        case .servers: "server.rack"
        case .general: "gearshape"
        case .providers: "cpu"
        case .projects: "folder"
        case .scheduled: "calendar.badge.clock"
        case .keybindings: "keyboard"
        case .access: "person.badge.key"
        case .archived: "archivebox"
        case .maintenance: "wrench.and.screwdriver"
        case .companion: "face.smiling"
        }
    }

    var isServerScoped: Bool {
        !Self.clientSections.contains(self)
    }

    var requiresConnection: Bool {
        isServerScoped && self != .companion
    }
}

struct SettingsWindow: View {
    @ObservedObject var core: Core
    @ObservedObject private var tab = SettingsTab.shared
    @State private var addingServer = false

    private var connection: ConnectionView { core.view.connection }
    private var servers: [EnvironmentView] { connection.environments }
    private var activeServer: EnvironmentView? {
        servers.first(where: { $0.id == connection.activeEnvironmentId })
            ?? servers.first(where: { $0.active })
            ?? servers.first
    }
    private var connected: Bool { ["connected", "reconnecting"].contains(connection.state) }

    private var currentSection: SettingsSectionKind {
        let chosen = SettingsSectionKind(rawValue: tab.selection) ?? .general
        if chosen.isServerScoped && servers.isEmpty { return .servers }
        return chosen
    }

    var body: some View {
        NavigationSplitView {
            sidebar
                .navigationSplitViewColumnWidth(min: 215, ideal: 236, max: 270)
        } detail: {
            VStack(spacing: 0) {
                SettingsDetailHeader(
                    core: core,
                    section: currentSection,
                    server: activeServer
                )
                Divider()
                detailBody
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .navigationSplitViewStyle(.balanced)
        .frame(minWidth: 760, idealWidth: 860, minHeight: 520, idealHeight: 620)
        .sheet(isPresented: $addingServer) {
            ConnectView(core: core, cancel: { addingServer = false })
                .frame(minWidth: 560, minHeight: 480)
        }
        .onChange(of: connection.state) { _, state in
            if state == "connected" {
                addingServer = false
                loadSection(currentSection)
            }
        }
        .onChange(of: connection.activeEnvironmentId) { _, _ in
            if connected { loadSection(currentSection) }
        }
        .onChange(of: tab.selection) { _, _ in
            if connected { loadSection(currentSection) }
        }
        .onAppear {
            if servers.isEmpty && currentSection.isServerScoped {
                tab.selection = SettingsSectionKind.servers.rawValue
            } else if connected {
                loadSection(currentSection)
            }
        }
    }

    private var sidebar: some View {
        List(selection: Binding(
            get: { currentSection.rawValue },
            set: { tab.selection = $0 }
        )) {
            Section {
                ForEach(SettingsSectionKind.clientSections) { sec in
                    Label(sec.title, systemImage: sec.symbol)
                        .tag(sec.rawValue)
                }
            } header: {
                Text("This Mac")
            }

            Section {
                if let server = activeServer {
                    ServerSwitcherControl(
                        core: core,
                        active: server,
                        addServer: { addingServer = true },
                        manageServers: { tab.selection = SettingsSectionKind.servers.rawValue }
                    )
                    .listRowInsets(EdgeInsets(top: 2, leading: 6, bottom: 6, trailing: 6))

                    ForEach(SettingsSectionKind.serverSections) { sec in
                        HStack(spacing: Space.xs) {
                            Label(sec.title, systemImage: sec.symbol)
                            Spacer(minLength: Space.xs)
                            if sec == .companion {
                                Text("Local")
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            }
                        }
                        .tag(sec.rawValue)
                    }
                } else {
                    Button {
                        addingServer = true
                    } label: {
                        Label("Add a Server…", systemImage: "plus.circle")
                            .foregroundStyle(.secondary)
                    }
                    .buttonStyle(.plain)
                }
            } header: {
                Text("Server Settings")
            }
        }
        .listStyle(.sidebar)
    }

    @ViewBuilder
    private var detailBody: some View {
        if currentSection.requiresConnection && !connected {
            ServerConnectionPlaceholder(core: core, server: activeServer, section: currentSection)
        } else {
            switch currentSection {
            case .appearance:
                AppearanceSettings()
            case .servers:
                ServersSettings(
                    core: core,
                    addServer: { addingServer = true },
                    configureServer: { id in
                        core.update(.selectEnvironment(id: id))
                        tab.selection = SettingsSectionKind.general.rawValue
                    }
                )
            case .general:
                GeneralSettings(core: core)
            case .providers:
                ProvidersSettings(core: core)
            case .projects:
                ProjectsSettings(core: core)
            case .scheduled:
                ScheduledSettings(core: core)
            case .keybindings:
                KeybindingsSettings(core: core)
            case .access:
                AccessSettings(core: core)
            case .archived:
                ArchivedSettings(core: core)
            case .maintenance:
                MaintenanceSettings(core: core)
            case .companion:
                CompanionEditor(core: core, environmentId: activeServer?.id ?? "")
            }
        }
    }

    private func loadSection(_ section: SettingsSectionKind) {
        switch section {
        case .general:
            core.update(.loadSettings(section: "general"))
        case .providers:
            core.update(.loadSettings(section: "providers"))
        case .projects:
            core.update(.loadSettings(section: "projects"))
        case .scheduled:
            core.update(.loadSettings(section: "scheduled"))
        case .archived:
            core.update(.loadSettings(section: "archived"))
        case .maintenance:
            core.update(.loadSettings(section: "storage"))
            core.update(.loadSettings(section: "diagnostics"))
        case .access:
            core.update(.loadSettings(section: "access"))
        case .appearance, .servers, .keybindings, .companion:
            break
        }
    }
}

/// Inline server switcher at the top of the Server Settings sidebar section.
private struct ServerSwitcherControl: View {
    @ObservedObject var core: Core
    let active: EnvironmentView
    let addServer: () -> Void
    let manageServers: () -> Void

    private var connection: ConnectionView { core.view.connection }

    private var dotColor: Color {
        switch connection.state {
        case "connected": Tint.success
        case "connecting", "reconnecting", "pairing": Tint.caution
        default: .secondary
        }
    }

    private var stateLabel: String {
        switch connection.state {
        case "connected":
            if connection.remote && !connection.path.isEmpty {
                return "\(connection.path.capitalized) · \(connection.rttMs) ms"
            }
            return "Connected"
        case "connecting", "pairing": return "Connecting…"
        case "reconnecting": return "Reconnecting…"
        default: return "Offline"
        }
    }

    var body: some View {
        Menu {
            Section("Configure Server") {
                ForEach(connection.environments, id: \.id) { env in
                    Button {
                        core.update(.selectEnvironment(id: env.id))
                    } label: {
                        HStack {
                            Text(env.label)
                            if env.id == active.id {
                                Image(systemName: "checkmark")
                            }
                        }
                    }
                }
            }
            Divider()
            Button("Manage Servers…", systemImage: "server.rack", action: manageServers)
            Button("Add Server…", systemImage: "plus", action: addServer)
        } label: {
            HStack(spacing: Space.s) {
                CompanionStill(look: active.companion.look, size: 24, mood: connection.state == "connected" ? .idle : .offline)
                VStack(alignment: .leading, spacing: 1) {
                    Text(active.label)
                        .font(.callout.weight(.semibold))
                        .lineLimit(1)
                    HStack(spacing: 4) {
                        Circle()
                            .fill(dotColor)
                            .frame(width: 6, height: 6)
                        Text(stateLabel)
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                }
                Spacer(minLength: 4)
                Image(systemName: "chevron.up.chevron.down")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
            .padding(.horizontal, Space.s)
            .padding(.vertical, Space.tight)
            .background(.quaternary.opacity(0.6), in: RoundedRectangle(cornerRadius: Radius.chip))
            .contentShape(Rectangle())
        }
        .menuStyle(.borderlessButton)
        .help("Choose which server to configure")
    }
}

/// Top context header in the detail pane showing section title, scope pill, and live server state.
private struct SettingsDetailHeader: View {
    @ObservedObject var core: Core
    let section: SettingsSectionKind
    let server: EnvironmentView?

    private var connection: ConnectionView { core.view.connection }

    private var scopeLabel: String {
        if !section.isServerScoped { return "This Mac" }
        if section == .companion { return "Saved on this Mac" }
        return "Server setting"
    }

    private var scopeSymbol: String {
        if !section.isServerScoped || section == .companion { return "laptopcomputer" }
        return "server.rack"
    }

    private var scopeTint: Color {
        if !section.isServerScoped || section == .companion { return .secondary }
        return .accentColor
    }

    var body: some View {
        HStack(spacing: Space.m) {
            if section.isServerScoped, let server {
                CompanionStill(look: server.companion.look, size: 28, mood: connection.state == "connected" ? .idle : .offline)
            } else {
                Image(systemName: section.symbol)
                    .font(.title3)
                    .foregroundStyle(.secondary)
                    .frame(width: 28, height: 28)
            }

            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: Space.s) {
                    Text(section.title)
                        .font(.headline)
                    Label(scopeLabel, systemImage: scopeSymbol)
                        .font(.caption2.weight(.medium))
                        .foregroundStyle(scopeTint)
                        .padding(.horizontal, 7)
                        .padding(.vertical, 2)
                        .background(scopeTint.opacity(0.12), in: Capsule())
                }
                if section.isServerScoped, let server {
                    Text(section == .companion
                         ? "Customizes how \(server.label) appears on this Mac only."
                         : "Applies on \(server.label) across all connected devices.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                } else {
                    Text("Saved on this Mac only. Applies across all servers.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }

            Spacer()

            if section.isServerScoped {
                connectionPill
            }
        }
        .padding(.horizontal, Space.l)
        .padding(.vertical, Space.m)
        .background(.bar)
    }

    @ViewBuilder
    private var connectionPill: some View {
        switch connection.state {
        case "connected":
            HStack(spacing: 5) {
                Circle().fill(Tint.success).frame(width: 7, height: 7)
                Text(connection.remote && !connection.path.isEmpty
                     ? "\(connection.path.capitalized) · \(connection.rttMs) ms"
                     : "Connected")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .padding(.horizontal, Space.s)
            .padding(.vertical, 4)
            .background(.quaternary.opacity(0.5), in: Capsule())
        case "connecting", "reconnecting", "pairing":
            HStack(spacing: 6) {
                ProgressView().controlSize(.mini)
                Text(connection.state == "reconnecting" ? "Reconnecting…" : "Connecting…")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        default:
            Button("Reconnect") { core.update(.reconnect) }
                .controlSize(.small)
        }
    }
}

/// Shown when a server-stored section is open while the selected server is connecting or offline.
private struct ServerConnectionPlaceholder: View {
    @ObservedObject var core: Core
    let server: EnvironmentView?
    let section: SettingsSectionKind

    private var connection: ConnectionView { core.view.connection }
    private var name: String { server?.label ?? "this server" }
    private var connecting: Bool { ["connecting", "reconnecting", "pairing"].contains(connection.state) }

    var body: some View {
        ContentUnavailableView {
            if connecting {
                Label("Connecting to \(name)…", systemImage: "arrow.triangle.2.circlepath")
            } else {
                Label("\(name) is Offline", systemImage: "network.slash")
            }
        } description: {
            if connecting {
                Text("Loading \(section.title.lowercased()) from \(name).")
            } else if let error = connection.error, !error.isEmpty {
                Text(error)
            } else {
                Text("\(section.title) is stored on \(name). Reconnect to view or change it.")
            }
        } actions: {
            if connecting {
                ProgressView().controlSize(.regular)
            } else {
                Button("Reconnect") { core.update(.reconnect) }
                    .buttonStyle(.borderedProminent)
            }
        }
    }
}

/// Client-local appearance and transcript text scale (saved on this Mac only).
struct AppearanceSettings: View {
    @AppStorage(AppPreferenceKeys.appearance) private var rawAppearance = AppAppearance.system.rawValue
    @AppStorage(AppPreferenceKeys.transcriptTextScale) private var rawScale = TranscriptTextScale.default.rawValue

    private var scale: TranscriptTextScale {
        TranscriptTextScale(rawValue: rawScale) ?? .default
    }

    var body: some View {
        Form {
            Section {
                Picker("Theme", selection: $rawAppearance) {
                    ForEach(AppAppearance.allCases) { mode in
                        Label(mode.label, systemImage: mode.symbol).tag(mode.rawValue)
                    }
                }
                .pickerStyle(.segmented)
            } header: {
                Text("Theme")
            } footer: {
                Text("Follows macOS appearance or forces Light or Dark on this Mac.")
            }

            Section {
                Picker("Transcript text size", selection: $rawScale) {
                    ForEach(TranscriptTextScale.allCases) { option in
                        Text(option.label).tag(option.rawValue)
                    }
                }
                .pickerStyle(.segmented)

                VStack(alignment: .leading, spacing: Space.xs) {
                    Text("Preview")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Text("The quick brown fox asks the agent to review the pull request.")
                        .font(.body)
                        .dynamicTypeSize(scale.dynamicTypeSize)
                }
                .padding(.vertical, Space.xs)
            } header: {
                Text("Reading")
            } footer: {
                Text("Adjusts message text size in threads on this Mac.")
            }
        }
        .formStyle(.grouped)
    }
}

/// Manages the servers paired with this Mac.
struct ServersSettings: View {
    @ObservedObject var core: Core
    let addServer: () -> Void
    let configureServer: (String) -> Void

    @State private var renaming: EnvironmentView?
    @State private var newName = ""
    @State private var forgetting: EnvironmentView?

    private var connection: ConnectionView { core.view.connection }
    private var servers: [EnvironmentView] { connection.environments }

    var body: some View {
        Form {
            if let local = connection.localServer {
                Section("Server on this Mac") {
                    HStack(spacing: Space.s) {
                        Image(systemName: "desktopcomputer")
                            .foregroundStyle(.secondary)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(local.label)
                            Text("\(local.url) · No pairing code needed")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        Spacer()
                        Button("Connect") { core.update(.connectLocalServer) }
                            .buttonStyle(.borderedProminent)
                    }
                }
            }

            Section {
                if servers.isEmpty {
                    Text("No servers paired with this Mac yet.")
                        .foregroundStyle(.secondary)
                }
                ForEach(Array(servers.enumerated()), id: \.element.id) { index, env in
                    HStack(spacing: Space.m) {
                        CompanionStill(
                            look: env.companion.look,
                            size: 28,
                            mood: env.active && connection.state == "connected" ? .idle : .offline
                        )
                        VStack(alignment: .leading, spacing: 2) {
                            HStack(spacing: Space.xs) {
                                Text(env.label)
                                    .font(.body.weight(env.active ? .semibold : .regular))
                                if env.active {
                                    Text(activeStatusText)
                                        .font(.caption2.weight(.medium))
                                        .foregroundStyle(connection.state == "connected" ? Tint.success : Tint.caution)
                                        .padding(.horizontal, 6)
                                        .padding(.vertical, 1)
                                        .background((connection.state == "connected" ? Tint.success : Tint.caution).opacity(0.14), in: Capsule())
                                }
                            }
                            Text(env.detail)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                                .truncationMode(.middle)
                        }
                        Spacer(minLength: Space.xs)
                        if !env.active {
                            Button("Switch") { core.update(.selectEnvironment(id: env.id)) }
                        }
                        Button("Configure") { configureServer(env.id) }
                            .buttonStyle(.bordered)
                        Menu {
                            Button("Configure Settings…") { configureServer(env.id) }
                            if !env.active {
                                Button("Switch to \(env.label)") { core.update(.selectEnvironment(id: env.id)) }
                            }
                            Divider()
                            Button("Rename on This Mac…") {
                                newName = env.label
                                renaming = env
                            }
                            if index > 0 {
                                Button("Move Up") { core.update(.moveEnvironment(id: env.id, delta: -1)) }
                            }
                            if index + 1 < servers.count {
                                Button("Move Down") { core.update(.moveEnvironment(id: env.id, delta: 1)) }
                            }
                            Divider()
                            Button("Forget…", role: .destructive) { forgetting = env }
                        } label: {
                            Image(systemName: "ellipsis.circle")
                        }
                        .menuStyle(.borderlessButton)
                        .fixedSize()
                        .help("More actions for \(env.label)")
                    }
                    .padding(.vertical, 2)
                }
            } header: {
                Text("Saved Servers")
            } footer: {
                Text("Server names and order are saved on this Mac. Choose Configure to edit settings stored on a specific server.")
            }

            Section {
                Button("Add Server…", systemImage: "plus", action: addServer)
            }
        }
        .formStyle(.grouped)
        .alert("Rename Server", isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })) {
            TextField("Name", text: $newName)
            Button("Rename") {
                if let env = renaming { core.update(.renameEnvironment(id: env.id, label: newName)) }
                renaming = nil
            }
            Button("Cancel", role: .cancel) { renaming = nil }
        } message: {
            Text("Changes how this server is named on this Mac only.")
        }
        .forgettingServer($forgetting, core: core)
    }

    private var activeStatusText: String {
        switch connection.state {
        case "connected": "Connected"
        case "connecting", "pairing": "Connecting…"
        case "reconnecting": "Reconnecting…"
        default: "Offline"
        }
    }
}

struct GeneralSettings: View {
    @ObservedObject var core: Core

    private var connection: ConnectionView { core.view.connection }

    var body: some View {
        Form {
            Section("Connection") {
                LabeledContent("Server", value: connection.environmentLabel)
                if connection.remote {
                    LabeledContent("Route", value: connection.path.isEmpty ? "Remote" : "\(connection.path.capitalized), \(connection.rttMs) ms")
                } else {
                    LabeledContent("Address", value: connection.serverUrl)
                }
                LabeledContent("Version", value: connection.serverVersion)
                LabeledContent("Platform", value: connection.platform)
                HStack(spacing: Space.s) {
                    Button("Refresh from Server") { core.update(.resync) }
                    Button("Disconnect", role: .destructive) { core.update(.disconnect) }
                }
            }
            ForEach(core.view.settings.groups, id: \.title) { group in
                Section(group.title) {
                    ForEach(group.fields, id: \.key) { field in
                        SettingFieldRow(core: core, field: field)
                    }
                }
            }
        }
        .formStyle(.grouped)
        .onAppear { core.update(.loadSettings(section: "general")) }
    }
}

struct ProvidersSettings: View {
    @ObservedObject var core: Core
    @State private var editing: ProviderEditing?

    var body: some View {
        Form {
            ForEach(core.view.settings.providers, id: \.instanceId) { p in
                Section {
                    LabeledContent("Status") {
                        Label(p.status.capitalized, systemImage: p.status == "ready" ? "checkmark.circle.fill" : "exclamationmark.triangle.fill")
                            .foregroundStyle(p.status == "ready" ? Tint.success : Tint.caution)
                    }
                    if !p.version.isEmpty { LabeledContent("Version", value: p.version) }
                    if !p.auth.isEmpty { LabeledContent("Account", value: p.auth) }
                    LabeledContent("Models", value: "\(p.modelCount)")
                    if !p.limits.isEmpty { LabeledContent("Limits", value: p.limits) }
                    if !p.message.isEmpty { Text(p.message).font(.caption).foregroundStyle(.secondary) }
                    ProviderSetupRows(provider: p, update: { core.update($0) }, edit: { editing = $0 })
                } header: {
                    Text(p.name.isEmpty ? p.instanceId : p.name)
                }
            }
            HStack {
                Button("Check Again") { core.update(.refreshProviders) }
                if core.view.settings.canAddAntigravity {
                    Button("Add Antigravity Account…") { editing = .newAntigravity() }
                }
            }
            Section("Failover Pools & Availability") {
                FailoverPoolsBody(core: core)
            }
        }
        .formStyle(.grouped)
        .sheet(item: $editing) { draft in
            NavigationStack {
                ProviderInstanceEditor(editing: draft, update: { core.update($0) }) { editing = nil }
                    .navigationTitle(draft.edit.instanceId.isEmpty ? "New Antigravity Account" : "Edit \(draft.edit.displayName.isEmpty ? draft.edit.instanceId : draft.edit.displayName)")
            }
            .frame(minWidth: 480, minHeight: 380)
        }
        .onAppear { core.update(.loadSettings(section: "providers")) }
    }
}

struct KeybindingsSettings: View {
    @ObservedObject var core: Core
    /// A binding waiting for a yes: there is no adding it back from here.
    @State private var removing: KeybindingView?

    var body: some View {
        Table(core.view.settings.keybindings) {
            TableColumn("Command", value: \.command)
            TableColumn("Shortcut", value: \.shortcut).width(90)
            TableColumn("When", value: \.condition)
            TableColumn("") { binding in
                Button { removing = binding } label: { Image(systemName: "trash") }
                    .buttonStyle(.borderless)
                    .help("Remove this shortcut")
                    .accessibilityLabel("Remove \(binding.shortcut) for \(binding.command)")
            }
            .width(28)
        }
        .contextMenu(forSelectionType: KeybindingView.ID.self) { ids in
            if let id = ids.first, let binding = core.view.settings.keybindings.first(where: { $0.id == id }) {
                Button("Remove Shortcut…") { removing = binding }
            }
        }
        .padding()
        .confirmationDialog(
            removing.map { "Remove \($0.shortcut) for “\($0.command)”?" } ?? "",
            isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }),
            titleVisibility: .visible,
            presenting: removing
        ) { binding in
            Button("Remove", role: .destructive) {
                core.update(.removeKeybinding(command: binding.command, shortcut: binding.shortcut, condition: binding.condition))
            }
        } message: { _ in
            Text("It comes off the server's keybindings.")
        }
    }
}

extension KeybindingView: @retroactive Identifiable {
    public var id: String { "\(command)|\(shortcut)|\(condition)" }
}

/// A task's last runs under its row: each opens the thread it went to.
struct ScheduledRunsList: View {
    let runs: [ScheduledRunView]
    let openThread: (String) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: Space.xxs) {
            if runs.isEmpty { Text("No runs yet.").font(.caption).foregroundStyle(.secondary) }
            ForEach(Array(runs.enumerated()), id: \.offset) { _, run in
                HStack(spacing: Space.xs) {
                    Text(run.status).font(.caption.weight(.medium)).foregroundStyle(run.failed ? .red : .primary)
                    Text([run.trigger, run.started, run.error.isEmpty ? run.threadTitle : run.error].filter { !$0.isEmpty }.joined(separator: " · "))
                        .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                    Spacer()
                    if !run.threadId.isEmpty {
                        Button("Open") { openThread(run.threadId) }.controlSize(.small)
                    }
                }
            }
        }
    }
}

struct ScheduledSettings: View {
    @ObservedObject var core: Core
    @State private var editing: ScheduledTaskEdit?
    @State private var deleting: ScheduledTaskView?

    var body: some View {
        Form {
            Section("Tasks" + (core.view.settings.scheduledTimeZone.isEmpty ? "" : " (\(core.view.settings.scheduledTimeZone))")) {
                if core.view.settings.scheduledTasks.isEmpty {
                    Text("No scheduled tasks.").foregroundStyle(.secondary)
                }
                ForEach(core.view.settings.scheduledTasks, id: \.id) { task in
                    VStack(alignment: .leading, spacing: Space.xs) {
                        HStack {
                            Toggle(isOn: Binding(get: { task.enabled }, set: { core.update(.setScheduledEnabled(id: task.id, enabled: $0)) })) {
                                Text(task.name).bold()
                            }
                            Spacer()
                            if task.scheduleKind != "webhook" {
                                Button("Run Now") { core.update(.runScheduledNow(id: task.id)) }
                            }
                            if let thread = task.openableThread {
                                Button(task.openThreadLabel) { core.update(.selectThread(threadId: thread)) }
                            }
                            Button(core.view.settings.scheduledRunsTask == task.id ? "Hide Runs" : "Runs") {
                                core.update(core.view.settings.scheduledRunsTask == task.id ? .loadScheduledRuns(id: "") : .loadScheduledRuns(id: task.id))
                            }
                            Button("Edit") { editing = ScheduledTaskEditor.from(task) }
                            Button(role: .destructive) { deleting = task } label: { Image(systemName: "trash") }
                                .buttonStyle(.borderless)
                                .help("Delete this task")
                                .accessibilityLabel("Delete \(task.name)")
                        }
                        Text(task.prompt).font(.caption).foregroundStyle(.secondary).lineLimit(2)
                        Text("\(task.schedule) · \(task.projectTitle) · \(task.destination) · next \(task.nextRun) · \(task.lastRun)").font(.caption2).foregroundStyle(.tertiary)
                        if !task.runState.isEmpty {
                            Text(task.runState).font(.caption2).foregroundStyle(.tint)
                        } else if !task.lastError.isEmpty {
                            Text(task.lastError).font(.caption2).foregroundStyle(.red).lineLimit(2)
                        }
                        if core.view.settings.scheduledRunsTask == task.id {
                            ScheduledRunsList(runs: core.view.settings.scheduledRuns) { core.update(.selectThread(threadId: $0)) }
                        }
                        if task.scheduleKind == "webhook" {
                            WebhookTaskDetails(core: core, task: task)
                        }
                    }
                }
            }
            Section {
                Button("New Task…") { editing = ScheduledTaskEditor.new(projectId: core.view.settings.projects.first?.id ?? "") }
                    .disabled(core.view.settings.projects.isEmpty)
            }
        }
        .formStyle(.grouped)
        .confirmingDestructive($deleting, title: { "Delete “\($0.name)”?" }, action: "Delete Task",
                               message: "It stops running. Threads it already started stay.") {
            core.update(.deleteScheduled(id: $0.id))
        }
        .sheet(item: $editing) { draft in
            NavigationStack {
                ScheduledTaskEditor(core: core, draft: draft) { editing = nil }
                    .navigationTitle(draft.id.isEmpty ? "New Task" : "Edit Task")
            }
            .frame(minWidth: 520, minHeight: 560)
        }
        .onAppear { core.update(.loadSettings(section: "scheduled")) }
        .onReceive(NotificationCenter.default.publisher(for: .snapshotOpenScheduledEditor)) { _ in
            var draft = ScheduledTaskEditor.new(projectId: core.view.settings.projects.first?.id ?? "")
            draft.name = "Weekday standup"
            draft.prompt = "Summarize what changed yesterday."
            draft.weekdays = [1, 2, 3, 4, 5]
            editing = draft
        }
    }
}

struct ArchivedSettings: View {
    @ObservedObject var core: Core
    @State private var deleting: ThreadRowView?

    var body: some View {
        Form {
            if core.view.settings.archived.isEmpty {
                Text("No archived threads.").foregroundStyle(.secondary)
            }
            ForEach(core.view.settings.archived, id: \.id) { thread in
                HStack {
                    Text(thread.title)
                    Spacer()
                    Text(thread.age).foregroundStyle(.secondary)
                    Button("Unarchive") { core.update(.archiveThread(threadId: thread.id, archived: false)) }
                    Button(role: .destructive) { deleting = thread } label: { Image(systemName: "trash") }
                        .buttonStyle(.borderless)
                        .help("Delete this thread")
                        .accessibilityLabel("Delete \(thread.title)")
                }
            }
        }
        .formStyle(.grouped)
        .confirmingDestructive($deleting, title: { "Delete “\($0.title)”?" }, action: "Delete Thread",
                               message: "The thread and its messages are deleted for good. This can't be undone.") {
            core.update(.deleteThread(threadId: $0.id))
        }
        .onAppear { core.update(.loadSettings(section: "archived")) }
    }
}

struct ProjectsSettings: View {
    @ObservedObject var core: Core
    @State private var removing: ProjectSettingsView?
    @State private var renaming: ProjectSettingsView?
    @State private var pickingIcon: ProjectSettingsView?
    @State private var title = ""

    var body: some View {
        Form {
            ForEach(core.view.settings.projects, id: \.id) { p in
                VStack(alignment: .leading, spacing: Space.xs) {
                    HStack(spacing: Space.s) {
                        ProjectIconTile(icon: p.icon, size: 24, core: core)
                        VStack(alignment: .leading) {
                            Text(p.title)
                            Text(p.workspaceRoot).font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.head)
                        }
                        Spacer(minLength: Space.xs)
                        Text("\(p.threadCount) threads").foregroundStyle(.secondary).fixedSize()
                        if core.view.connection.isOnThisMac {
                            Button { NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: p.workspaceRoot) } label: { Image(systemName: "folder") }
                                .buttonStyle(.borderless)
                                .help("Reveal in Finder")
                                .accessibilityLabel("Reveal \(p.title) in Finder")
                        }
                        Button("Change Icon…") { pickingIcon = p }
                            .disabled(p.icon.saving || core.view.connection.state != "connected")
                            .accessibilityLabel("Change icon for \(p.title)")
                        Button("Rename…") {
                            title = p.title
                            renaming = p
                        }
                        Button("Remove…", role: .destructive) { removing = p }
                    }
                    if !p.isScratch {
                        ProjectIndexRow(core: core, project: p)
                    }
                }
            }
        }
        .formStyle(.grouped)
        .onAppear { core.update(.loadSettings(section: "projects")) }
        .sheet(isPresented: Binding(get: { pickingIcon != nil }, set: { if !$0 { pickingIcon = nil } })) {
            if let project = pickingIcon { ProjectIconPicker(project: project, core: core) }
        }
        .confirmingDestructive($removing, title: { "Remove “\($0.title)” from Domovoi?" }, action: "Remove Project",
                               message: "Its threads leave Domovoi. The folder and its files on the server stay as they are.") {
            core.update(.removeProject(projectId: $0.id))
        }
        .alert("Rename Project", isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })) {
            TextField("Title", text: $title)
            Button("Rename") {
                if let p = renaming { core.update(.renameProject(projectId: p.id, title: title)) }
                renaming = nil
            }
            Button("Cancel", role: .cancel) { renaming = nil }
        }
    }
}

struct MaintenanceSettings: View {
    @ObservedObject var core: Core

    private var storage: StorageView { core.view.settings.storage }
    private var diagnostics: DiagnosticsView { core.view.settings.diagnostics }

    var body: some View {
        Form {
            Section("Storage") {
                LabeledContent("Database", value: storage.databaseSize)
                LabeledContent("Reclaimable", value: storage.reclaimable)
                LabeledContent("Last cleanup", value: storage.lastCleanup)
                HStack {
                    Button("Clean Up Now") { core.update(.runStorageCleanup) }
                    Button("Reclaim Database Space") { core.update(.reclaimDatabase) }
                    if storage.busy { ProgressView().controlSize(.small) }
                }
                .disabled(storage.busy)
            }
            Section("Server processes") {
                LabeledContent("CPU", value: diagnostics.cpu)
                LabeledContent("Memory", value: diagnostics.memory)
                ForEach(diagnostics.processes, id: \.pid) { p in
                    HStack {
                        Text(p.command).font(TypeRole.monoSmall).lineLimit(1).padding(.leading, CGFloat(p.depth) * Space.m)
                        Spacer()
                        Text(p.cpu).font(.caption.monospacedDigit())
                        Text(p.memory).font(.caption.monospacedDigit()).frame(width: 70, alignment: .trailing)
                    }
                }
                Button("Refresh") { core.update(.loadSettings(section: "diagnostics")) }
            }
        }
        .formStyle(.grouped)
        .onAppear {
            core.update(.loadSettings(section: "storage"))
            core.update(.loadSettings(section: "diagnostics"))
        }
    }
}

struct AccessSettings: View {
    @ObservedObject var core: Core
    @State private var signingOut: AccessEntryView?

    private var remote: RemoteAccessView { core.view.settings.remote }

    var body: some View {
        Form {
            if remote.available {
                Section {
                    Toggle("Reach this server from other devices", isOn: Binding(
                        get: { remote.enabled || remote.running },
                        set: { core.update(.setRemoteAccess(enabled: $0)) }
                    ))
                    .disabled(remote.busy || core.view.connection.remote)
                    if remote.running {
                        LabeledContent("Endpoint") {
                            Text(remote.endpointId).font(TypeRole.monoSmall).textSelection(.enabled).lineLimit(1).truncationMode(.middle)
                        }
                        LabeledContent("Relay", value: remote.homeRelay.isEmpty ? (remote.relays.first ?? "connecting…") : remote.homeRelay)
                        if remote.link.isEmpty {
                            Button("Pair a Device…") { core.update(.createPairingLink) }
                                .disabled(remote.busy)
                        } else {
                            HStack(alignment: .top, spacing: Space.l) {
                                QRCodeView(text: remote.link)
                                    .frame(width: 168, height: 168)
                                    .padding(Space.tight)
                                    .background(.white, in: RoundedRectangle(cornerRadius: Radius.chip))
                                VStack(alignment: .leading, spacing: Space.s) {
                                    Text("Scan with Domovoi on iPhone, or paste the link into Domovoi on another Mac.")
                                        .fixedSize(horizontal: false, vertical: true)
                                    Text("Code \(remote.code) · works once, until \(ServerTime.local(remote.linkExpiresAt))")
                                        .font(.caption).foregroundStyle(.secondary)
                                    HStack {
                                        Button("Copy Link") { Platform.copy(remote.link) }
                                        Button("New Code") { core.update(.createPairingLink) }
                                    }
                                }
                            }
                        }
                    }
                    if remote.busy { ProgressView().controlSize(.small) }
                    if let error = remote.error {
                        HStack {
                            Label(error, systemImage: "exclamationmark.triangle.fill").foregroundStyle(Tint.danger)
                            Spacer()
                            Button("Try Again") { core.update(.loadRemoteAccess) }.disabled(remote.busy)
                        }
                    }
                } header: {
                    Text("Remote access")
                } footer: {
                    Text(core.view.connection.remote
                        ? "You are connected remotely; turn remote access off on the server's Mac."
                        : "Devices connect over iroh: end-to-end encrypted, direct when possible, through a relay otherwise. Each paired device signs in with its own key.")
                }
            }
            Section("Signed-in devices") {
                ForEach(core.view.settings.access, id: \.id) { entry in
                    HStack {
                        Image(systemName: icon(entry))
                            .foregroundStyle(entry.connected ? Tint.success : Color.secondary)
                        VStack(alignment: .leading) {
                            Text(entry.label + (entry.current ? " (this app)" : ""))
                            Text(entry.detail + (entry.connected ? " · connected" : "")).font(.caption).foregroundStyle(.secondary)
                        }
                        Spacer()
                        if entry.kind == "session", !entry.current {
                            Button("Sign Out…") { signingOut = entry }
                        }
                    }
                }
            }
        }
        .formStyle(.grouped)
        .confirmingDestructive($signingOut, title: { "Sign out \($0.label)?" }, action: "Sign Out",
                               message: "The device loses access to this server until it's paired again.") {
            core.update(.revokeClient(sessionId: $0.id))
        }
        .onAppear { core.update(.loadSettings(section: "access")) }
        // Remote access may have been turned on or off from the server's
        // status bar icon meanwhile.
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            if core.view.connection.state == "connected" { core.update(.loadRemoteAccess) }
        }
    }

    private func icon(_ entry: AccessEntryView) -> String {
        if entry.kind != "session" { return "qrcode" }
        switch entry.deviceType {
        case "mobile": return "iphone"
        case "tablet": return "ipad"
        default: return "laptopcomputer"
        }
    }
}

extension View {
    /// Asks before a destructive action on `item`: a dialog while it's set,
    /// with one destructive button that hands it to `perform`.
    func confirmingDestructive<Item>(
        _ item: Binding<Item?>,
        title: @escaping (Item) -> String,
        action: String,
        message: String,
        perform: @escaping (Item) -> Void
    ) -> some View {
        confirmationDialog(
            item.wrappedValue.map(title) ?? "",
            isPresented: Binding(get: { item.wrappedValue != nil }, set: { if !$0 { item.wrappedValue = nil } }),
            titleVisibility: .visible,
            presenting: item.wrappedValue
        ) { value in
            Button(action, role: .destructive) { perform(value) }
        } message: { _ in
            Text(message)
        }
    }
}
