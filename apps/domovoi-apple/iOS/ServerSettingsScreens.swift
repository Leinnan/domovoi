import App
import SwiftUI

/// The connected server's own settings, grouped as the server describes them.
struct ServerSettingsScreen: View {
    @ObservedObject var core: Core

    private var groups: [SettingGroupView] { core.view.settings.groups }

    private var connected: Bool { ["connected", "reconnecting"].contains(core.view.connection.state) }

    var body: some View {
        Form {
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
            if groups.isEmpty, connected {
                ProgressView()
            } else if groups.isEmpty {
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

/// The agents on the server: installed, signed in, how many models.
struct ProvidersScreen: View {
    @ObservedObject var core: Core
    @State private var editing: ProviderEditing?

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
                    if !p.limits.isEmpty { LabeledContent("Limits", value: p.limits) }
                    if !p.message.isEmpty { Text(p.message).font(TypeRole.meta).foregroundStyle(.secondary) }
                    ProviderSetupRows(provider: p, update: { core.update($0) }, edit: { editing = $0 })
                } header: {
                    Text(p.name.isEmpty ? p.instanceId : p.name)
                }
            }
            Section {
                Button("Check Again") { core.update(.refreshProviders) }
                if core.view.settings.canAddAntigravity {
                    Button("Add Antigravity Account") { editing = .newAntigravity() }
                }
            } footer: {
                Text("Other agents sign in and install on the server itself, in their command-line tools. Antigravity signs in here: Google returns to the server's machine, so from this device paste the address the browser ended on.")
            }
        }
        .navigationTitle("Providers")
        .navigationBarTitleDisplayMode(.inline)
        .sheet(item: $editing) { draft in
            NavigationStack {
                ProviderInstanceEditor(editing: draft, update: { core.update($0) }) { editing = nil }
                    .navigationTitle(draft.edit.instanceId.isEmpty ? "New Account" : "Edit Account")
                    .navigationBarTitleDisplayMode(.inline)
            }
        }
        .onAppear { core.update(.loadSettings(section: "providers")) }
        .refreshable { await core.refresh(.refreshProviders) }
    }
}

/// One scheduled task: what it does, its last runs (each opens its thread),
/// and Run Now, Open Thread and Edit.
struct ScheduledTaskDetail: View {
    @ObservedObject var core: Core
    let taskId: String
    let open: (Screen) -> Void
    @State private var editing: ScheduledTaskEdit?

    private var task: ScheduledTaskView? { core.view.settings.scheduledTasks.first { $0.id == taskId } }
    private var runs: [ScheduledRunView] { core.view.settings.scheduledRunsTask == taskId ? core.view.settings.scheduledRuns : [] }

    var body: some View {
        List {
            if let task {
                Section {
                    Toggle("Enabled", isOn: Binding(get: { task.enabled }, set: { core.update(.setScheduledEnabled(id: task.id, enabled: $0)) }))
                    LabeledContent("Runs", value: task.schedule)
                    LabeledContent("Project", value: task.projectTitle)
                    LabeledContent("Where", value: task.destination)
                    if task.enabled && task.scheduleKind != "webhook" { LabeledContent("Next", value: task.nextRun) }
                    if !task.runState.isEmpty { LabeledContent("Now", value: task.runState) }
                } footer: {
                    Text(task.prompt).lineLimit(6)
                }
                Section {
                    if task.scheduleKind != "webhook" {
                        Button("Run Now", systemImage: "play") { core.update(.runScheduledNow(id: task.id)) }
                    }
                    if let thread = task.openableThread {
                        Button("Open Thread", systemImage: "bubble.left.and.text.bubble.right") { open(.thread(thread)) }
                    }
                    Button("Edit", systemImage: "pencil") { editing = ScheduledTaskEditor.from(task) }
                }
                Section("Last runs") {
                    if runs.isEmpty {
                        Text(task.lastRun).foregroundStyle(.secondary)
                    }
                    ForEach(Array(runs.enumerated()), id: \.offset) { _, run in
                        Button {
                            if !run.threadId.isEmpty { open(.thread(run.threadId)) }
                        } label: {
                            VStack(alignment: .leading, spacing: Space.xxs) {
                                HStack {
                                    Text(run.status).foregroundStyle(run.failed ? Tint.danger : .primary)
                                    Spacer()
                                    Text(run.trigger).font(TypeRole.meta).foregroundStyle(.secondary)
                                }
                                Text([run.started, run.threadTitle].filter { !$0.isEmpty }.joined(separator: " · "))
                                    .font(TypeRole.meta).foregroundStyle(.secondary)
                                if !run.error.isEmpty { Text(run.error).font(TypeRole.meta).foregroundStyle(Tint.danger).lineLimit(3) }
                            }
                        }
                        .buttonStyle(.plain)
                        .disabled(run.threadId.isEmpty)
                    }
                }
            }
        }
        .overlay {
            if task == nil {
                ContentUnavailableView("Task Deleted", systemImage: "calendar.badge.exclamationmark", description: Text("This task is no longer on the server."))
            }
        }
        .navigationTitle(task?.name ?? "Task")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { core.update(.loadScheduledRuns(id: taskId)) }
        .refreshable { core.update(.loadScheduledRuns(id: taskId)) }
        .sheet(item: $editing) { draft in
            NavigationStack {
                ScheduledTaskEditor(core: core, draft: draft) { editing = nil }
                    .navigationTitle("Edit Task")
                    .navigationBarTitleDisplayMode(.inline)
            }
        }
    }
}

/// Prompts the server runs on a schedule (or on webhook requests).
struct ScheduledTasksScreen: View {
    @ObservedObject var core: Core
    let open: (Screen) -> Void
    @State private var editing: ScheduledTaskEdit?
    @State private var deleting: ScheduledTaskView?
    /// The task whose runs and details are pushed.
    @State private var detail: String?

    private var settings: SettingsView { core.view.settings }

    var body: some View {
        List {
            Section {
                ForEach(settings.scheduledTasks, id: \.id) { task in
                    VStack(alignment: .leading, spacing: Space.xs) {
                        Toggle(isOn: Binding(get: { task.enabled }, set: { core.update(.setScheduledEnabled(id: task.id, enabled: $0)) })) {
                            Text(task.name)
                        }
                        Text(task.prompt).font(TypeRole.meta).foregroundStyle(.secondary).lineLimit(2)
                        Text([task.schedule, task.projectTitle, task.destination, task.enabled && task.scheduleKind != "webhook" ? "next \(task.nextRun)" : "", task.lastRun].filter { !$0.isEmpty }.joined(separator: " · "))
                            .font(TypeRole.metaSmall).foregroundStyle(.tertiary)
                        if !task.runState.isEmpty {
                            Label(task.runState, systemImage: task.runState == "Needs you" ? "hand.raised.fill" : task.runState == "Running" ? "play.circle" : "hourglass")
                                .font(TypeRole.metaSmall).foregroundStyle(task.runState == "Needs you" ? AnyShapeStyle(Tint.caution) : AnyShapeStyle(.tint))
                        } else if !task.lastError.isEmpty {
                            Text(task.lastError).font(TypeRole.metaSmall).foregroundStyle(Tint.danger).lineLimit(2)
                        }
                        if task.scheduleKind == "webhook" {
                            WebhookTaskDetails(core: core, task: task)
                        }
                        // A button, not a link nested in the row: a link took
                        // the row's tap, so a tap meant to edit opened details.
                        Button { detail = task.id } label: {
                            HStack(spacing: Space.xs) {
                                Text("Runs and details")
                                Image(systemName: "chevron.right").imageScale(.small)
                            }
                            .font(TypeRole.meta)
                            .tapTarget()
                        }
                        .buttonStyle(.borderless)
                    }
                    .contentShape(Rectangle())
                    // The rest of the row edits.
                    .onTapGesture { editing = ScheduledTaskEditor.from(task) }
                    .swipeActions {
                        Button("Delete", role: .destructive) { deleting = task }
                        if task.scheduleKind != "webhook" {
                            Button("Run Now") { core.update(.runScheduledNow(id: task.id)) }.tint(.blue)
                        }
                    }
                    .swipeActions(edge: .leading) {
                        if let thread = task.openableThread {
                            Button("Open Thread") { open(.thread(thread)) }.tint(.indigo)
                        }
                    }
                    .contextMenu {
                        Button("Edit", systemImage: "pencil") { editing = ScheduledTaskEditor.from(task) }
                        if let thread = task.openableThread {
                            Button("Open Thread", systemImage: "bubble.left.and.text.bubble.right") { open(.thread(thread)) }
                        }
                        if task.scheduleKind != "webhook" {
                            Button("Run Now", systemImage: "play") { core.update(.runScheduledNow(id: task.id)) }
                        }
                        Button("Delete…", systemImage: "trash", role: .destructive) { deleting = task }
                    }
                }
            } footer: {
                if !settings.scheduledTimeZone.isEmpty { Text("Times are in \(settings.scheduledTimeZone), the server's time zone.") }
            }
        }
        .overlay {
            if settings.scheduledTasks.isEmpty {
                ContentUnavailableView("No Scheduled Tasks", systemImage: "calendar.badge.clock", description: Text("A scheduled task sends its prompt on its own: at a time, every so often, or when its URL gets a request."))
            }
        }
        .navigationTitle("Scheduled Tasks")
        .navigationBarTitleDisplayMode(.inline)
        .navigationDestination(item: $detail) { id in
            ScheduledTaskDetail(core: core, taskId: id, open: open)
        }
        .toolbar {
            Button { editing = ScheduledTaskEditor.new(projectId: settings.projects.first?.id ?? "") } label: { Image(systemName: "plus") }
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
        .sheet(item: $editing) { draft in
            NavigationStack {
                ScheduledTaskEditor(core: core, draft: draft) { editing = nil }
                    .navigationTitle(draft.id.isEmpty ? "New Scheduled Task" : "Edit Task")
                    .navigationBarTitleDisplayMode(.inline)
            }
        }
        .onAppear { core.update(.loadSettings(section: "scheduled")) }
        .onReceive(IOSSnapshots.openScheduledEditor) { draft in
            // Closed first: an open sheet keeps its draft for the same (empty) id.
            editing = nil
            Task { @MainActor in
                try? await Task.sleep(for: .milliseconds(600))
                editing = draft
            }
        }
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
                if storage.busy { LoadingRow("Working…") }
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
                        Text(p.cpu).font(TypeRole.meta.monospacedDigit()).foregroundStyle(.secondary)
                        Text(p.memory).font(TypeRole.meta.monospacedDigit()).foregroundStyle(.secondary)
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
                    if remote.busy { LoadingRow("Working…") }
                    if let error = remote.error { ErrorRow(error) }
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
                    SettingsRow(title: entry.label + (entry.current ? " (this device)" : ""), subtitle: entry.detail + (entry.connected ? " · connected" : "")) {
                        Image(systemName: icon(entry))
                            .foregroundStyle(entry.connected ? Tint.success : .secondary)
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
                .font(TypeRole.meta).foregroundStyle(.secondary).multilineTextAlignment(.center)
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
