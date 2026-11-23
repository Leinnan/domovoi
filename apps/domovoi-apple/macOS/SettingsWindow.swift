import App
import AppKit
import SwiftUI

/// The selected Settings tab (shared so the Remote Access menu item and
/// snapshots can open a tab).
@MainActor
final class SettingsTab: ObservableObject {
    static let shared = SettingsTab()
    @Published var selection = "general"
}

struct SettingsWindow: View {
    @ObservedObject var core: Core
    @ObservedObject private var tab = SettingsTab.shared

    private var connected: Bool { ["connected", "reconnecting"].contains(core.view.connection.state) }

    var body: some View {
        TabView(selection: Binding(
            get: { connected ? tab.selection : "companion" },
            set: { tab.selection = $0 }
        )) {
            if connected {
                GeneralSettings(core: core).tabItem { Label("General", systemImage: "gearshape") }.tag("general")
                ProvidersSettings(core: core).tabItem { Label("Providers", systemImage: "cpu") }.tag("providers")
                KeybindingsSettings(core: core).tabItem { Label("Keybindings", systemImage: "keyboard") }.tag("keybindings")
                ScheduledSettings(core: core).tabItem { Label("Scheduled", systemImage: "calendar.badge.clock") }.tag("scheduled")
                ArchivedSettings(core: core).tabItem { Label("Archived", systemImage: "archivebox") }.tag("archived")
                ProjectsSettings(core: core).tabItem { Label("Projects", systemImage: "folder") }.tag("projects")
                MaintenanceSettings(core: core).tabItem { Label("Maintenance", systemImage: "wrench.and.screwdriver") }.tag("maintenance")
                AccessSettings(core: core).tabItem { Label("Access", systemImage: "person.badge.key") }.tag("access")
            }
            CompanionSettings(core: core).tabItem { Label("Companion", systemImage: "face.smiling") }.tag("companion")
        }
        .frame(width: 720, height: 600)
    }
}

private struct CompanionSettings: View {
    @ObservedObject var core: Core
    @State private var chosenServer: String?

    private var servers: [EnvironmentView] { core.view.connection.environments }
    private var serverId: String {
        if let chosenServer, servers.contains(where: { $0.id == chosenServer }) { return chosenServer }
        return (servers.first(where: { $0.active }) ?? servers.first)?.id ?? ""
    }

    var body: some View {
        VStack(spacing: 0) {
            if !servers.isEmpty {
                Picker("Server", selection: Binding(get: { serverId }, set: { chosenServer = $0 })) {
                    ForEach(servers, id: \.id) { Text($0.label).tag($0.id) }
                }
                .padding()
            }
            CompanionEditor(core: core, environmentId: serverId)
        }
    }
}

struct GeneralSettings: View {
    @ObservedObject var core: Core

    var body: some View {
        Form {
            Section("Server") {
                LabeledContent("Address", value: core.view.connection.serverUrl)
                LabeledContent("Environment", value: core.view.connection.environmentLabel)
                LabeledContent("Version", value: core.view.connection.serverVersion)
                LabeledContent("Platform", value: core.view.connection.platform)
                Button("Disconnect") { core.update(.disconnect) }
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
                                Button("Open Thread") { core.update(.selectThread(threadId: thread)) }
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
            }
        }
        .formStyle(.grouped)
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
                        Label(error, systemImage: "exclamationmark.triangle.fill").foregroundStyle(Tint.danger)
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
