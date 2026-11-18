import App
import SwiftUI

/// The connected server's own settings, grouped as the server describes them.
struct ServerSettingsScreen: View {
    @ObservedObject var core: Core

    private var groups: [SettingGroupView] { core.view.settings.groups }

    private var connected: Bool { ["connected", "reconnecting"].contains(core.view.connection.state) }

    var body: some View {
        Form {
            if groups.isEmpty, connected {
                ProgressView().frame(maxWidth: .infinity)
            }
            ForEach(groups, id: \.title) { group in
                Section(group.title) {
                    ForEach(group.fields, id: \.key) { field in
                        SettingFieldRow(core: core, field: field)
                    }
                }
            }
        }
        .overlay {
            // Not a spinner forever: the settings come from a server.
            if groups.isEmpty, !connected {
                ContentUnavailableView {
                    Label("Not Connected", systemImage: "network.slash")
                } description: {
                    Text("Server settings load once Domovoi is connected to the server.")
                } actions: {
                    Button("Try Again") { core.update(.loadSettings(section: "general")) }
                }
            }
        }
        .navigationTitle("Server Settings")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { core.update(.loadSettings(section: "general")) }
    }
}

/// Codex and Claude on the server: installed, signed in, how many models.
struct ProvidersScreen: View {
    @ObservedObject var core: Core

    var body: some View {
        Form {
            ForEach(core.view.settings.providers, id: \.instanceId) { p in
                Section {
                    LabeledContent("Status") {
                        HStack(spacing: Space.xs) {
                            Image(systemName: p.status == "ready" ? "checkmark.circle.fill" : "exclamationmark.triangle.fill")
                            Text(p.status.capitalized)
                        }
                        .foregroundStyle(p.status == "ready" ? Tint.success : Tint.caution)
                    }
                    if !p.version.isEmpty { LabeledContent("Version", value: p.version) }
                    if !p.auth.isEmpty { LabeledContent("Account", value: p.auth) }
                    LabeledContent("Models", value: "\(p.modelCount)")
                    if !p.message.isEmpty { Text(p.message).font(.caption).foregroundStyle(.secondary) }
                } header: {
                    Text(p.name.isEmpty ? p.instanceId : p.name)
                }
            }
            Section {
                Button("Check Again") { core.update(.refreshProviders) }
            } footer: {
                Text("Sign in and install on the server itself, in the Claude Code and Codex command-line tools.")
            }
        }
        .navigationTitle("Providers")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { core.update(.loadSettings(section: "providers")) }
        .refreshable { await core.refresh(.refreshProviders) }
    }
}

/// Prompts the server runs on a schedule, each in a new thread.
struct ScheduledTasksScreen: View {
    @ObservedObject var core: Core
    @State private var creating = false
    @State private var deleting: ScheduledTaskView?

    private var settings: SettingsView { core.view.settings }

    var body: some View {
        List {
            Section {
                if settings.scheduledTasks.isEmpty {
                    ContentUnavailableView("No scheduled tasks", systemImage: "calendar.badge.clock", description: Text("A scheduled task sends its prompt in a new thread, on its own."))
                }
                ForEach(settings.scheduledTasks, id: \.id) { task in
                    VStack(alignment: .leading, spacing: Space.xs) {
                        Toggle(isOn: Binding(get: { task.enabled }, set: { core.update(.setScheduledEnabled(id: task.id, enabled: $0)) })) {
                            Text(task.name).font(.body.weight(.semibold))
                        }
                        Text(task.prompt).font(.caption).foregroundStyle(.secondary).lineLimit(2)
                        Text([task.schedule, task.projectTitle, task.enabled ? "next \(task.nextRun)" : "", task.lastRun].filter { !$0.isEmpty }.joined(separator: " · "))
                            .font(.caption2).foregroundStyle(.tertiary)
                    }
                    .swipeActions {
                        Button("Delete", role: .destructive) { deleting = task }
                        Button("Run Now") { core.update(.runScheduledNow(id: task.id)) }.tint(.blue)
                    }
                    .contextMenu {
                        Button("Run Now", systemImage: "play") { core.update(.runScheduledNow(id: task.id)) }
                        Button("Delete…", systemImage: "trash", role: .destructive) { deleting = task }
                    }
                }
            } footer: {
                if !settings.scheduledTimeZone.isEmpty { Text("Times are in \(settings.scheduledTimeZone), the server's time zone.") }
            }
        }
        .navigationTitle("Scheduled Tasks")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            Button { creating = true } label: { Image(systemName: "plus") }
                .accessibilityLabel("New Scheduled Task")
                .disabled(settings.projects.isEmpty)
        }
        .confirmationDialog(
            "Delete \(deleting?.name ?? "this task")?",
            isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } }),
            titleVisibility: .visible,
            presenting: deleting
        ) { task in
            Button("Delete", role: .destructive) { core.update(.deleteScheduled(id: task.id)) }
        } message: { _ in
            Text("It stops running. Threads it already started stay.")
        }
        .sheet(isPresented: $creating) {
            NavigationStack { NewScheduledTaskScreen(core: core, done: { creating = false }) }
        }
        .onAppear { core.update(.loadSettings(section: "scheduled")) }
    }
}

private struct NewScheduledTaskScreen: View {
    @ObservedObject var core: Core
    let done: () -> Void
    @State private var name = ""
    @State private var prompt = ""
    @State private var project = ""
    @State private var minutes = 60

    var body: some View {
        Form {
            Section {
                TextField("Name", text: $name)
                TextField("Prompt", text: $prompt, axis: .vertical).lineLimit(3...8)
            }
            Section {
                Picker("Project", selection: $project) {
                    ForEach(core.view.settings.projects, id: \.id) { Text($0.title).tag($0.id) }
                }
                Picker("Every", selection: $minutes) {
                    ForEach(ScheduleIntervals.all, id: \.minutes) { Text($0.label).tag($0.minutes) }
                }
            }
        }
        .navigationTitle("New Scheduled Task")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) { Button("Cancel", action: done) }
            ToolbarItem(placement: .confirmationAction) {
                Button("Create") {
                    core.update(.createScheduled(name: name, prompt: prompt, projectId: project, intervalMinutes: UInt32(minutes)))
                    done()
                }
                .disabled(name.trimmingCharacters(in: .whitespaces).isEmpty || prompt.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || project.isEmpty)
            }
        }
        .onAppear { pickProject() }
        // Projects may arrive after the sheet opens.
        .onChange(of: core.view.settings.projects.map(\.id)) { _, _ in pickProject() }
    }

    private func pickProject() {
        let projects = core.view.settings.projects
        if !projects.contains(where: { $0.id == project }) { project = projects.first?.id ?? "" }
    }
}

/// The server's database and processes.
struct MaintenanceScreen: View {
    @ObservedObject var core: Core
    @State private var reclaiming = false

    private var storage: StorageView { core.view.settings.storage }
    private var diagnostics: DiagnosticsView { core.view.settings.diagnostics }

    var body: some View {
        Form {
            Section {
                LabeledContent("Database", value: storage.databaseSize)
                LabeledContent("Reclaimable", value: storage.reclaimable)
                LabeledContent("Last cleanup", value: storage.lastCleanup)
                Button("Clean Up Now") { core.update(.runStorageCleanup) }
                Button("Reclaim Database Space…") { reclaiming = true }
                if storage.busy { ProgressView() }
            } header: {
                Text("Storage")
            } footer: {
                Text("Cleanup removes what deleted threads and old turns left behind. Reclaiming compacts the database file.")
            }
            .disabled(storage.busy)
            Section("Server processes") {
                LabeledContent("CPU", value: diagnostics.cpu)
                LabeledContent("Memory", value: diagnostics.memory)
                ForEach(diagnostics.processes, id: \.pid) { p in
                    HStack {
                        Text(p.command).font(TypeRole.monoSmall).lineLimit(1).truncationMode(.middle)
                            .padding(.leading, CGFloat(p.depth) * Space.m)
                        Spacer()
                        Text(p.cpu).font(.caption.monospacedDigit()).foregroundStyle(.secondary)
                        Text(p.memory).font(.caption.monospacedDigit()).foregroundStyle(.secondary)
                    }
                }
            }
        }
        .navigationTitle("Maintenance")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear {
            core.update(.loadSettings(section: "storage"))
            core.update(.loadSettings(section: "diagnostics"))
        }
        .refreshable { await core.refresh(.loadSettings(section: "diagnostics")) }
        .confirmationDialog("Reclaim database space?", isPresented: $reclaiming, titleVisibility: .visible) {
            Button("Reclaim \(storage.reclaimable)") { core.update(.reclaimDatabase) }
        } message: {
            Text("The server compacts its database file. It may pause for a moment while it does.")
        }
    }
}

/// Remote access (iroh) and the devices signed in to the server.
struct AccessScreen: View {
    @ObservedObject var core: Core
    @State private var signingOut: AccessEntryView?
    @State private var turningOff = false

    private var remote: RemoteAccessView { core.view.settings.remote }

    var body: some View {
        Form {
            if remote.available {
                Section {
                    Toggle("Reach this server from other devices", isOn: Binding(
                        get: { remote.enabled || remote.running },
                        // Off cuts every remote device at once: ask first.
                        set: { on in if on { core.update(.setRemoteAccess(enabled: true)) } else { turningOff = true } }
                    ))
                    .disabled(remote.busy || core.view.connection.remote)
                    if remote.running {
                        LabeledContent("Relay", value: remote.homeRelay.isEmpty ? (remote.relays.first ?? "connecting…") : remote.homeRelay)
                        if remote.link.isEmpty {
                            Button("Pair a Device…") { core.update(.createPairingLink) }
                                .disabled(remote.busy)
                        } else {
                            pairing
                        }
                    }
                    if remote.busy { ProgressView() }
                    if let error = remote.error {
                        Label(error, systemImage: "exclamationmark.triangle.fill").foregroundStyle(Tint.danger)
                    }
                } header: {
                    Text("Remote access")
                } footer: {
                    Text(core.view.connection.remote
                        ? "This device is connected remotely, so remote access can only be turned off on the server's Mac."
                        : "Devices connect over iroh: end-to-end encrypted, direct when possible, through a relay otherwise. Each paired device signs in with its own key.")
                }
            }
            Section("Signed-in devices") {
                ForEach(core.view.settings.access, id: \.id) { entry in
                    HStack(spacing: Space.m) {
                        Image(systemName: icon(entry))
                            .foregroundStyle(entry.connected ? Tint.success : .secondary)
                            .frame(width: 24)
                        VStack(alignment: .leading, spacing: Space.xxs) {
                            Text(entry.label + (entry.current ? " (this device)" : ""))
                            Text(entry.detail + (entry.connected ? " · connected" : "")).font(.caption).foregroundStyle(.secondary)
                        }
                    }
                    .swipeActions {
                        if entry.kind == "session", !entry.current {
                            Button("Sign Out", role: .destructive) { signingOut = entry }
                        }
                    }
                    .contextMenu {
                        if entry.kind == "session", !entry.current {
                            Button("Sign Out…", systemImage: "rectangle.portrait.and.arrow.right", role: .destructive) { signingOut = entry }
                        }
                    }
                }
            }
        }
        .navigationTitle("Access")
        .confirmationDialog("Turn off remote access?", isPresented: $turningOff, titleVisibility: .visible) {
            Button("Turn Off", role: .destructive) { core.update(.setRemoteAccess(enabled: false)) }
        } message: {
            Text("Devices paired for remote access can't reach this server until it's back on.")
        }
        .navigationBarTitleDisplayMode(.inline)
        .confirmationDialog(
            "Sign out \(signingOut?.label ?? "this device")?",
            isPresented: Binding(get: { signingOut != nil }, set: { if !$0 { signingOut = nil } }),
            titleVisibility: .visible,
            presenting: signingOut
        ) { entry in
            Button("Sign Out", role: .destructive) { core.update(.revokeClient(sessionId: entry.id)) }
        } message: { _ in
            Text("It needs a new pairing code to connect again.")
        }
        .onAppear { core.update(.loadSettings(section: "access")) }
    }

    private var pairing: some View {
        VStack(spacing: Space.m) {
            QRCodeView(text: remote.link)
                .frame(width: 200, height: 200)
                .padding(Space.s)
                .background(.white, in: RoundedRectangle(cornerRadius: Radius.chip))
                .accessibilityLabel("Pairing QR code")
            Text("Scan with Domovoi on another iPhone or iPad, or send the link to a Mac.")
                .font(.callout).multilineTextAlignment(.center)
            Text("Code \(remote.code) · works once, until \(ServerTime.local(remote.linkExpiresAt))")
                .font(.caption).foregroundStyle(.secondary).multilineTextAlignment(.center)
            HStack(spacing: Space.l) {
                ShareLink(item: remote.link) { Label("Share", systemImage: "square.and.arrow.up") }
                Button("Copy", systemImage: "doc.on.doc") { Platform.copy(remote.link) }
                Button("New Code", systemImage: "arrow.clockwise") { core.update(.createPairingLink) }
            }
            .buttonStyle(.borderless)
            .font(.callout)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, Space.s)
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
