import App
import SwiftUI

/// Creates or changes a scheduled task: when it runs (at a time on chosen
/// days, once, every so often, or on webhook requests), where (a new thread
/// each time, or one thread continued or started fresh), in which folder,
/// and with which agent settings.
struct ScheduledTaskEditor: View {
    @ObservedObject var core: Core
    @State var draft: ScheduledTaskEdit
    var onDone: () -> Void
    @State private var time: Date
    @State private var runAt: Date
    /// The project and thread the task was bound to when the editor opened.
    private let opened: (projectId: String, threadId: String)
    /// The bound thread's permissions as changed here (saved to the thread); nil: as they are.
    @State private var threadRuntimeMode: String?

    init(core: Core, draft: ScheduledTaskEdit, onDone: @escaping () -> Void) {
        self.core = core
        _draft = State(initialValue: draft)
        self.onDone = onDone
        opened = (draft.projectId, draft.id.isEmpty ? "" : draft.threadId)
        _time = State(initialValue: ScheduledTaskEditor.time(of: draft.timeOfDay))
        _runAt = State(initialValue: ServerTime.date(draft.runAt) ?? Date().addingTimeInterval(3600))
    }

    /// A new task in `projectId`: daily at 9:00 in a new thread, with the
    /// composer's model, in full access: nobody is there to
    /// answer an unattended run's questions.
    static func new(projectId: String) -> ScheduledTaskEdit {
        ScheduledTaskEdit(id: "", name: "", prompt: "", projectId: projectId, scheduleKind: "fixed_time", intervalMinutes: 60, timeOfDay: "09:00",
                          weekdays: [], runAt: "", threadMode: "new_thread", threadId: "", workspace: "root", baseBranch: "",
                          modelInstance: "", model: "", runtimeMode: "full-access", interactionMode: "default", enabled: true)
    }

    /// Said under a permissions picker set to "Ask first".
    static let asksWarning = "Each run stops at the first tool that needs your approval, and waits until you answer."


    /// A new task that continues `thread`, with its model and modes (kept
    /// should the task later go back to new threads): the thread's "Schedule…".
    static func new(thread: ThreadPageView) -> ScheduledTaskEdit {
        var draft = new(projectId: thread.projectId)
        draft.threadMode = "continue"
        draft.threadId = thread.id
        draft.name = thread.title
        draft.modelInstance = thread.model.isEmpty ? "" : thread.modelInstanceId
        draft.model = thread.model
        if !thread.runtimeMode.isEmpty { draft.runtimeMode = thread.runtimeMode }
        draft.interactionMode = thread.interactionMode == "plan" ? "plan" : "default"
        return draft
    }

    /// The labels of the thread modes, here and in the task rows.
    static let threadModes: [(tag: String, label: String)] = [
        ("new_thread", "New thread each run"), ("continue", "Same thread"), ("fresh_session", "Same thread, fresh session"),
    ]

    /// The editor's fields for a task that exists.
    static func from(_ t: ScheduledTaskView) -> ScheduledTaskEdit {
        ScheduledTaskEdit(id: t.id, name: t.name, prompt: t.prompt, projectId: t.projectId, scheduleKind: t.scheduleKind.isEmpty ? "interval" : t.scheduleKind,
                          intervalMinutes: t.intervalMinutes, timeOfDay: t.timeOfDay.isEmpty ? "09:00" : t.timeOfDay, weekdays: t.weekdays, runAt: t.runAt,
                          threadMode: t.threadMode, threadId: t.threadId, workspace: t.workspace, baseBranch: t.baseBranch, modelInstance: t.modelInstance,
                          model: t.model, runtimeMode: t.runtimeMode, interactionMode: t.interactionMode, enabled: t.enabled)
    }

    private static func time(of text: String) -> Date {
        let parts = text.split(separator: ":").compactMap { Int($0) }
        let calendar = Calendar.current
        return calendar.date(bySettingHour: parts.first ?? 9, minute: parts.dropFirst().first ?? 0, second: 0, of: Date()) ?? Date()
    }

    /// The project's threads, newest first, whatever the thread list's layout.
    private var threads: [ScheduledThreadChoiceView] {
        core.view.settings.scheduledThreadChoices.filter { $0.projectId == draft.projectId }
    }

    /// Whether a one-thread task may leave its thread to the first run. A
    /// task already bound in this project can't be unbound by leaving the
    /// thread out (the server keeps the one it has), so it picks one.
    private var firstRunStartsThread: Bool {
        opened.threadId.isEmpty || draft.projectId != opened.projectId
    }

    private var chosenThread: ScheduledThreadChoiceView? {
        draft.threadId.isEmpty ? nil : core.view.settings.scheduledThreadChoices.first { $0.id == draft.threadId }
    }

    private var threadLabel: String {
        if draft.threadId.isEmpty { return firstRunStartsThread ? "Started by the first run" : "Choose…" }
        return chosenThread?.title ?? "A thread not on this device"
    }

    /// Runs go into a thread chosen here: its own model, modes and folder apply.
    private var boundToChosenThread: Bool {
        draft.threadMode != "new_thread" && !draft.threadId.isEmpty
    }

    private var scratchProject: Bool {
        core.view.settings.projects.first { $0.id == draft.projectId }?.isScratch ?? false
    }

    private var modeFooter: String {
        switch draft.threadMode {
        case "continue": return "Each run is a new message in one thread, which remembers the earlier runs."
        case "fresh_session": return "Each run is a new message in one thread, answered by an agent that starts without the earlier conversation."
        default: return scratchProject ? "Each run starts its own thread, listed under No project." : "Each run starts its own thread."
        }
    }

    private var modelKey: Binding<String> {
        Binding(get: { draft.model.isEmpty ? "" : "\(draft.modelInstance)|\(draft.model)" },
                set: { key in
                    let parts = key.split(separator: "|", maxSplits: 1).map(String.init)
                    draft.modelInstance = parts.first ?? ""
                    draft.model = parts.count > 1 ? parts[1] : ""
                })
    }

    var body: some View {
        Form {
            Section {
                TextField("Name", text: $draft.name)
                TextField("Prompt", text: $draft.prompt, axis: .vertical).lineLimit(3...8)
                if draft.scheduleKind == "webhook" {
                    Text("The prompt can use {{body.path}}, {{headers.name}} and {{query.name}} from each request.")
                        .font(TypeRole.meta).foregroundStyle(.secondary)
                }
            }
            Section("When") {
                Picker("Runs", selection: $draft.scheduleKind) {
                    Text("At a time").tag("fixed_time")
                    Text("Once").tag("once")
                    Text("Every").tag("interval")
                    Text("On webhook").tag("webhook")
                }
                .pickerStyle(.segmented)
                switch draft.scheduleKind {
                case "fixed_time":
                    DatePicker("Time", selection: $time, displayedComponents: .hourAndMinute)
                    WeekdayChips(selected: $draft.weekdays)
                case "once":
                    DatePicker("At", selection: $runAt, in: Date()...)
                case "interval":
                    Picker("Every", selection: Binding(get: { Int(draft.intervalMinutes) }, set: { draft.intervalMinutes = UInt32($0) })) {
                        ForEach(ScheduleIntervals.all, id: \.minutes) { Text($0.label).tag($0.minutes) }
                    }
                default:
                    Text("Runs on each request to the task's URL, shown once it is saved.").foregroundStyle(.secondary)
                }
            }
            Section {
                Picker("Project", selection: $draft.projectId) {
                    ForEach(core.view.settings.projects, id: \.id) { Text($0.title).tag($0.id) }
                }
                Picker("Thread", selection: $draft.threadMode) {
                    ForEach(Self.threadModes, id: \.tag) { Text($0.label).tag($0.tag) }
                }
                if draft.threadMode != "new_thread" {
                    NavigationLink {
                        ScheduledThreadPicker(choices: threads, firstRunStartsThread: firstRunStartsThread, threadId: $draft.threadId)
                    } label: {
                        LabeledContent("Which", value: threadLabel)
                    }
                }
                if !boundToChosenThread {
                    Picker("Folder", selection: $draft.workspace) {
                        Text("Project folder").tag("root")
                        Text("New worktree").tag("worktree")
                    }
                    if draft.workspace == "worktree" {
                        TextField("Base branch", text: $draft.baseBranch)
                    }
                }
            } header: {
                Text("Where")
            } footer: {
                Text(modeFooter)
            }
            if boundToChosenThread {
                Section {
                    LabeledContent("Model", value: chosenThread.map { $0.modelLabel.isEmpty ? "The thread's" : $0.modelLabel } ?? "The thread's")
                    Picker("Permissions", selection: threadPermissions) {
                        Text("Ask first").tag("approval-required")
                        Text("Accept edits").tag("auto-accept-edits")
                        Text("Full access").tag("full-access")
                    }
                    Toggle("Enabled", isOn: $draft.enabled)
                } header: {
                    Text("Agent")
                } footer: {
                    Text(threadPermissions.wrappedValue == "approval-required"
                         ? "\(Self.asksWarning) The permissions are the thread's own: a change here changes the thread."
                         : "Runs use the thread's own model, permissions and folder: a change of permissions here changes the thread.")
                }
            } else {
                agentSection
            }
        }
        .formStyle(.grouped)
        // A thread belongs to its project.
        .onChange(of: draft.projectId) { draft.threadId = "" }
        .onChange(of: draft.threadId) { threadRuntimeMode = nil }
        .toolbar {
            ToolbarItem(placement: .cancellationAction) { Button("Cancel") { onDone() } }
            ToolbarItem(placement: .confirmationAction) {
                Button(draft.id.isEmpty ? "Create" : "Save") { save() }.disabled(!complete)
            }
        }
    }

    /// The agent a new thread runs: for new threads and a thread the first run starts.
    private var agentSection: some View {
        Section {
            Picker("Model", selection: modelKey) {
                Text("The composer's model").tag("")
                // Favorites are left out of the groups: listed first here.
                ForEach(core.view.composer.favoriteModels, id: \.option) { row in
                    Text("★ \(row.detail.isEmpty ? row.option.label : "\(row.detail) · \(row.name)")").tag("\(row.option.instanceId)|\(row.option.model)")
                }
                ForEach(core.view.composer.modelGroups, id: \.instanceId) { group in
                    ForEach(group.models, id: \.option.model) { row in
                        Text("\(group.provider) · \(row.name)").tag("\(row.option.instanceId)|\(row.option.model)")
                    }
                }
            }
            Picker("Permissions", selection: $draft.runtimeMode) {
                Text("Ask first").tag("approval-required")
                Text("Accept edits").tag("auto-accept-edits")
                Text("Full access").tag("full-access")
            }
            Toggle("Plan first", isOn: Binding(get: { draft.interactionMode == "plan" }, set: { draft.interactionMode = $0 ? "plan" : "default" }))
            Toggle("Enabled", isOn: $draft.enabled)
        } header: {
            Text("Agent")
        } footer: {
            if draft.runtimeMode == "approval-required" { Text(Self.asksWarning) }
        }
    }

    /// The bound thread's permissions: as changed here, else as the thread has them.
    private var threadPermissions: Binding<String> {
        Binding(get: { threadRuntimeMode ?? chosenThread.map { $0.runtimeMode.isEmpty ? "approval-required" : $0.runtimeMode } ?? "approval-required" },
                set: { threadRuntimeMode = $0 })
    }

    private var complete: Bool {
        !draft.name.trimmingCharacters(in: .whitespaces).isEmpty && !draft.prompt.trimmingCharacters(in: .whitespaces).isEmpty
            && !draft.projectId.isEmpty && (draft.threadMode == "new_thread" || !draft.threadId.isEmpty || firstRunStartsThread)
    }

    private func save() {
        var task = draft
        let parts = Calendar.current.dateComponents([.hour, .minute], from: time)
        task.timeOfDay = String(format: "%02d:%02d", parts.hour ?? 9, parts.minute ?? 0)
        let iso = ISO8601DateFormatter()
        iso.timeZone = .current
        iso.formatOptions = [.withInternetDateTime]
        task.runAt = task.scheduleKind == "once" ? iso.string(from: runAt) : ""
        if boundToChosenThread, let mode = threadRuntimeMode, mode != chosenThread?.runtimeMode {
            core.update(.setThreadRuntimeMode(threadId: draft.threadId, mode: mode))
        }
        core.update(.saveScheduled(task: task))
        onDone()
    }
}

/// The thread a one-thread task posts into: any of its project's, searched
/// by title, or (for a task not bound yet) the one its first run starts.
struct ScheduledThreadPicker: View {
    let choices: [ScheduledThreadChoiceView]
    let firstRunStartsThread: Bool
    @Binding var threadId: String
    @State private var query = ""
    @Environment(\.dismiss) private var dismiss

    private var shown: [ScheduledThreadChoiceView] {
        let q = query.trimmingCharacters(in: .whitespaces)
        return q.isEmpty ? choices : choices.filter { $0.title.localizedCaseInsensitiveContains(q) }
    }

    var body: some View {
        List {
            if firstRunStartsThread && query.isEmpty {
                row(id: "", title: "Start one with the first run", detail: "Later runs go to the thread it starts.")
            }
            ForEach(shown, id: \.id) { row(id: $0.id, title: $0.title.isEmpty ? "New Thread" : $0.title, detail: $0.detail) }
            if shown.isEmpty && !query.isEmpty {
                Text("No thread of this project is called that.").foregroundStyle(.secondary)
            }
        }
        .searchable(text: $query, prompt: "Thread title")
        .navigationTitle("Thread")
    }

    private func row(id: String, title: String, detail: String) -> some View {
        Button {
            threadId = id
            dismiss()
        } label: {
            HStack {
                VStack(alignment: .leading, spacing: Space.xxs) {
                    Text(title).foregroundStyle(.primary).lineLimit(2)
                    if !detail.isEmpty { Text(detail).font(TypeRole.meta).foregroundStyle(.secondary) }
                }
                Spacer()
                if threadId == id { Image(systemName: "checkmark").foregroundStyle(.tint) }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(threadId == id ? .isSelected : [])
    }
}

/// The days a task runs on, Sunday (0) to Saturday (6); none is every day.
struct WeekdayChips: View {
    @Binding var selected: [UInt8]
    private let names = Calendar.current.veryShortWeekdaySymbols

    var body: some View {
        VStack(alignment: .leading, spacing: Space.xs) {
            // Seven equal chips across the row, whatever the day's letter.
            HStack(spacing: Space.xs) {
                ForEach(0..<7, id: \.self) { day in
                    let on = selected.contains(UInt8(day))
                    Button {
                        if on { selected.removeAll { $0 == UInt8(day) } } else { selected.append(UInt8(day)) }
                    } label: {
                        Text(names[day])
                            .fontWeight(on ? .bold : .regular)
                            .foregroundStyle(.primary)
                            .frame(maxWidth: .infinity, minHeight: Size.chip)
                            .glassChip(selected: on)
                            .contentShape(Capsule())
                    }
                    // Plain: in a form row each chip takes its own tap, not the row's.
                    .buttonStyle(.plain)
                    .accessibilityLabel(Calendar.current.weekdaySymbols[day])
                    .accessibilityAddTraits(on ? .isSelected : [])
                }
            }
            if selected.isEmpty {
                Text("Every day").font(TypeRole.meta).foregroundStyle(.secondary)
            }
        }
    }
}

/// A webhook task's URL with copy and a new token, and its last deliveries.
struct WebhookTaskDetails: View {
    @ObservedObject var core: Core
    let task: ScheduledTaskView
    @State private var confirmRotate = false

    var body: some View {
        VStack(alignment: .leading, spacing: Space.xs) {
            HStack {
                Text(task.webhookUrl).font(TypeRole.monoSmall).textSelection(.enabled).lineLimit(1).truncationMode(.middle)
                Spacer()
                // Borderless: in a list row a bordered button fired on any tap on the row.
                Button { Platform.copy(task.webhookUrl) } label: { Image(systemName: "doc.on.doc").tapTarget(width: true) }
                    .buttonStyle(.borderless).help("Copy the URL").accessibilityLabel("Copy webhook URL")
                Button { confirmRotate = true } label: { Text("New URL").tapTarget() }
                    .buttonStyle(.borderless).controlSize(.small)
                Button { core.update(.loadWebhookDeliveries(id: task.id)) } label: { Text("Deliveries").tapTarget() }
                    .buttonStyle(.borderless).controlSize(.small)
            }
            if core.view.settings.webhookDeliveriesTask == task.id {
                if core.view.settings.webhookDeliveries.isEmpty {
                    Text("No requests yet.").font(TypeRole.meta).foregroundStyle(.secondary)
                }
                ForEach(core.view.settings.webhookDeliveries, id: \.id) { d in
                    HStack {
                        Text(d.outcome).font(TypeRole.meta.weight(.medium))
                        Text(d.method).font(TypeRole.meta).foregroundStyle(.secondary)
                        Text(d.detail).font(TypeRole.meta).foregroundStyle(.secondary).lineLimit(1)
                        Spacer()
                        Text(d.received).font(TypeRole.metaSmall).foregroundStyle(.tertiary)
                    }
                }
            }
        }
        .confirmationDialog("Give this task a new URL?", isPresented: $confirmRotate, titleVisibility: .visible) {
            Button("New URL", role: .destructive) { core.update(.rotateWebhookToken(id: task.id)) }
        } message: {
            Text("The current URL stops working.")
        }
    }
}

extension ScheduledTaskEdit: @retroactive Identifiable {}

extension ScheduledTaskView {
    /// The thread to open from the task: its own, else the one its last run started.
    var openableThread: String? {
        let id = threadId.isEmpty ? self.lastThreadId : threadId
        return id.isEmpty ? nil : id
    }

    /// "Open Thread", or "Open Last Run" for a task that starts a new thread each run.
    var openThreadLabel: String { threadId.isEmpty && !self.lastThreadId.isEmpty ? "Open Last Run" : "Open Thread" }

    /// Where its runs go, for the task's row.
    var destination: String {
        if threadMode == "new_thread" { return "new thread each run" }
        if threadId.isEmpty { return "the thread its first run starts" }
        let title = threadTitle.isEmpty ? "a thread not on this device" : "“\(threadTitle)”"
        return threadMode == "fresh_session" ? "in \(title), fresh session" : "in \(title)"
    }
}
