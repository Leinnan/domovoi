package com.crayen.domovoi.ui.settings

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExposedDropdownMenuAnchorType
import androidx.compose.material3.ExposedDropdownMenuBox
import androidx.compose.material3.ExposedDropdownMenuDefaults
import androidx.compose.material3.FilterChip
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import com.crayen.domovoi.app.ScheduledTaskEdit
import com.crayen.domovoi.app.ScheduledThreadChoiceView
import com.crayen.domovoi.app.ScheduledTaskView
import com.crayen.domovoi.ui.theme.Space
import java.time.LocalDateTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter

/** How often a task can run: the same short list as the Apple apps. */
val scheduleIntervals = listOf(5 to "5 minutes", 15 to "15 minutes", 30 to "30 minutes", 60 to "hour", 180 to "3 hours", 360 to "6 hours", 720 to "12 hours", 1440 to "day", 10080 to "week")

/** The thread modes as the editor and the task rows name them. */
val scheduledThreadModes = listOf("new_thread" to "New thread each run", "continue" to "Same thread", "fresh_session" to "Same thread, fresh session")

/** A new task in `projectId`: daily at 9:00 in a new thread, with the composer's model, in full access (nobody answers an unattended run). */
fun newScheduledTask(projectId: String) = ScheduledTaskEdit(
    id = "", name = "", prompt = "", projectId = projectId, scheduleKind = "fixed_time", intervalMinutes = 60u, timeOfDay = "09:00", weekdays = emptyList(),
    runAt = "", threadMode = "new_thread", threadId = "", workspace = "root", baseBranch = "", modelInstance = "", model = "",
    runtimeMode = "full-access", interactionMode = "default", enabled = true,
)

/** Said under a permissions picker set to "Ask first". */
const val ASKS_WARNING = "Each run stops at the first tool that needs your approval, and waits until you answer."

/** A new task that continues `thread`, with its model and modes: the thread's "Schedule this thread". */
fun scheduledTaskFromThread(thread: com.crayen.domovoi.app.ThreadPageView) = newScheduledTask(thread.projectId).copy(
    name = thread.title, threadMode = "continue", threadId = thread.id,
    modelInstance = if (thread.model.isEmpty()) "" else thread.modelInstanceId, model = thread.model,
    runtimeMode = thread.runtimeMode.ifEmpty { "approval-required" }, interactionMode = if (thread.interactionMode == "plan") "plan" else "default",
)

/** The editor's fields for a task that exists. */
fun editedScheduledTask(t: ScheduledTaskView) = ScheduledTaskEdit(
    id = t.id, name = t.name, prompt = t.prompt, projectId = t.projectId, scheduleKind = t.scheduleKind.ifEmpty { "interval" }, intervalMinutes = t.intervalMinutes,
    timeOfDay = t.timeOfDay.ifEmpty { "09:00" }, weekdays = t.weekdays, runAt = t.runAt, threadMode = t.threadMode, threadId = t.threadId, workspace = t.workspace,
    baseBranch = t.baseBranch, modelInstance = t.modelInstance, model = t.model, runtimeMode = t.runtimeMode, interactionMode = t.interactionMode, enabled = t.enabled,
)

private val localMinute: DateTimeFormatter = DateTimeFormatter.ofPattern("yyyy-MM-dd HH:mm")

/** `runAt` (ISO with an offset) as the editor shows it, in this device's zone. */
private fun shownRunAt(iso: String): String = runCatching {
    java.time.OffsetDateTime.parse(iso).atZoneSameInstant(ZoneId.systemDefault()).toLocalDateTime().format(localMinute)
}.getOrDefault(LocalDateTime.now().plusHours(1).withSecond(0).withNano(0).format(localMinute))

/** What the editor's "yyyy-MM-dd HH:mm" says, as ISO with this device's offset; empty if it doesn't parse. */
private fun isoRunAt(shown: String): String = runCatching {
    LocalDateTime.parse(shown.trim(), localMinute).atZone(ZoneId.systemDefault()).toOffsetDateTime().format(DateTimeFormatter.ISO_OFFSET_DATE_TIME)
}.getOrDefault("")

/**
 * Creates or changes a scheduled task: when it runs (at a time on chosen days,
 * once, every so often, or on webhook requests), where (a new thread each
 * time, or one thread continued or started fresh), in which folder, and with
 * which agent settings.
 */
@OptIn(ExperimentalMaterial3Api::class, androidx.compose.foundation.layout.ExperimentalLayoutApi::class)
@Composable
fun ScheduledTaskEditorSheet(
    start: ScheduledTaskEdit,
    projects: List<Pair<String, String>>,
    threadsOf: (String) -> List<ScheduledThreadChoiceView>,
    isScratch: (String) -> Boolean,
    models: List<Pair<String, String>>,
    onDismiss: () -> Unit,
    /** The task, and the bound thread's permissions when they were changed here. */
    save: (ScheduledTaskEdit, String?) -> Unit,
) {
    var t by remember { mutableStateOf(start) }
    // The bound thread's permissions as changed here (saved to the thread); null: as they are.
    var threadMode by remember { mutableStateOf<String?>(null) }
    var runAt by remember { mutableStateOf(shownRunAt(start.runAt)) }
    val days = listOf("S", "M", "T", "W", "T", "F", "S")
    val timeOk = Regex("^([01]?\\d|2[0-3]):[0-5]\\d$").matches(t.timeOfDay.trim())
    // A task already bound in this project can't be unbound by leaving the
    // thread out (the server keeps the one it has), so it picks one.
    val firstRunStartsThread = start.id.isEmpty() || start.threadId.isEmpty() || t.projectId != start.projectId
    val complete = t.name.isNotBlank() && t.prompt.isNotBlank() && t.projectId.isNotEmpty() &&
        (t.threadMode == "new_thread" || t.threadId.isNotEmpty() || firstRunStartsThread) &&
        when (t.scheduleKind) {
            "fixed_time" -> timeOk
            "once" -> isoRunAt(runAt).isNotEmpty()
            else -> true
        }
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        Column(Modifier.padding(horizontal = Space.xl).verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(Space.m)) {
            Text(if (t.id.isEmpty()) "New scheduled task" else "Edit task", style = MaterialTheme.typography.titleLarge)
            OutlinedTextField(t.name, { t = t.copy(name = it) }, label = { Text("Name") }, singleLine = true, modifier = Modifier.fillMaxWidth())
            OutlinedTextField(t.prompt, { t = t.copy(prompt = it) }, label = { Text("Prompt") }, minLines = 3, modifier = Modifier.fillMaxWidth())
            if (t.scheduleKind == "webhook") Text("The prompt can use {{body.path}}, {{headers.name}} and {{query.name}} from each request.", style = MaterialTheme.typography.bodySmall)

            Text("When", style = MaterialTheme.typography.titleSmall)
            val kinds = listOf("fixed_time" to "At a time", "once" to "Once", "interval" to "Every", "webhook" to "Webhook")
            SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
                kinds.forEachIndexed { i, (kind, label) ->
                    SegmentedButton(selected = t.scheduleKind == kind, onClick = { t = t.copy(scheduleKind = kind) }, shape = SegmentedButtonDefaults.itemShape(i, kinds.size)) { Text(label, maxLines = 1) }
                }
            }
            when (t.scheduleKind) {
                "fixed_time" -> {
                    OutlinedTextField(t.timeOfDay, { t = t.copy(timeOfDay = it) }, label = { Text("Time (HH:MM)") }, isError = !timeOk, singleLine = true, modifier = Modifier.fillMaxWidth())
                    FlowRow(horizontalArrangement = Arrangement.spacedBy(Space.xs)) {
                        days.forEachIndexed { i, d ->
                            val on = t.weekdays.contains(i.toUByte())
                            FilterChip(selected = on, onClick = { t = t.copy(weekdays = if (on) t.weekdays - i.toUByte() else t.weekdays + i.toUByte()) }, label = { Text(d) })
                        }
                    }
                    if (t.weekdays.isEmpty()) Text("Every day", style = MaterialTheme.typography.bodySmall)
                }
                "once" -> OutlinedTextField(runAt, { runAt = it }, label = { Text("At (yyyy-MM-dd HH:mm)") }, isError = isoRunAt(runAt).isEmpty(), singleLine = true, modifier = Modifier.fillMaxWidth())
                "interval" -> Choice("Every", scheduleIntervals.map { it.first.toString() to it.second }, t.intervalMinutes.toString()) { t = t.copy(intervalMinutes = it.toUInt()) }
                else -> Text("Runs on each request to the task's URL, shown once it is saved.", style = MaterialTheme.typography.bodyMedium)
            }

            Text("Where", style = MaterialTheme.typography.titleSmall)
            Choice("Project", projects, t.projectId) { t = t.copy(projectId = it, threadId = "") }
            Choice("Thread", scheduledThreadModes, t.threadMode) { t = t.copy(threadMode = it) }
            val threads = threadsOf(t.projectId)
            if (t.threadMode != "new_thread") ThreadChoice(threads, firstRunStartsThread, t.threadId) { t = t.copy(threadId = it); threadMode = null }
            val bound = t.threadMode != "new_thread" && t.threadId.isNotEmpty()
            Text(
                when (t.threadMode) {
                    "continue" -> "Each run is a new message in one thread, which remembers the earlier runs."
                    "fresh_session" -> "Each run is a new message in one thread, answered by an agent that starts without the earlier conversation."
                    else -> if (isScratch(t.projectId)) "Each run starts its own thread, listed under No project." else "Each run starts its own thread."
                },
                style = MaterialTheme.typography.bodySmall,
            )
            if (!bound) {
                Choice("Folder", listOf("root" to "Project folder", "worktree" to "New worktree"), t.workspace) { t = t.copy(workspace = it) }
                if (t.workspace == "worktree") OutlinedTextField(t.baseBranch, { t = t.copy(baseBranch = it) }, label = { Text("Base branch") }, singleLine = true, modifier = Modifier.fillMaxWidth())
            }

            Text("Agent", style = MaterialTheme.typography.titleSmall)
            val permissions = listOf("approval-required" to "Ask first", "auto-accept-edits" to "Accept edits", "full-access" to "Full access")
            if (bound) {
                val thread = threads.firstOrNull { it.id == t.threadId }
                Text("Model: ${thread?.modelLabel.orEmpty().ifEmpty { "The thread's" }}", style = MaterialTheme.typography.bodyLarge)
                val shown = threadMode ?: thread?.runtimeMode.orEmpty().ifEmpty { "approval-required" }
                Choice("Permissions", permissions, shown) { threadMode = it }
                Text(
                    (if (shown == "approval-required") "$ASKS_WARNING " else "") + "Runs use the thread's own model, permissions and folder: a change of permissions here changes the thread.",
                    style = MaterialTheme.typography.bodySmall,
                )
            } else {
                Choice("Model", listOf("" to "The composer's model") + models, if (t.model.isEmpty()) "" else "${t.modelInstance}|${t.model}") { key ->
                    val parts = key.split("|", limit = 2)
                    t = t.copy(modelInstance = parts.getOrElse(0) { "" }, model = parts.getOrElse(1) { "" })
                }
                Choice("Permissions", permissions, t.runtimeMode) { t = t.copy(runtimeMode = it) }
                if (t.runtimeMode == "approval-required") Text(ASKS_WARNING, style = MaterialTheme.typography.bodySmall)
                Toggle("Plan first", t.interactionMode == "plan") { t = t.copy(interactionMode = if (it) "plan" else "default") }
            }
            Toggle("Enabled", t.enabled) { t = t.copy(enabled = it) }

            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(Space.s, Alignment.End)) {
                TextButton(onClick = onDismiss) { Text("Cancel") }
                Button(onClick = {
                    val changed = threadMode?.takeIf { bound && it != threads.firstOrNull { c -> c.id == t.threadId }?.runtimeMode }
                    save(t.copy(name = t.name.trim(), prompt = t.prompt.trim(), timeOfDay = t.timeOfDay.trim(), runAt = if (t.scheduleKind == "once") isoRunAt(runAt) else ""), changed)
                }, enabled = complete) {
                    Text(if (t.id.isEmpty()) "Create" else "Save")
                }
            }
            Spacer(Modifier.height(Space.xl))
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun Choice(label: String, options: List<Pair<String, String>>, selected: String, pick: (String) -> Unit) {
    var open by remember { mutableStateOf(false) }
    ExposedDropdownMenuBox(expanded = open, onExpandedChange = { open = it }) {
        OutlinedTextField(
            value = options.firstOrNull { it.first == selected }?.second ?: "Choose…", onValueChange = {}, readOnly = true, label = { Text(label) },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(open) },
            modifier = Modifier.fillMaxWidth().menuAnchor(ExposedDropdownMenuAnchorType.PrimaryNotEditable),
        )
        ExposedDropdownMenu(expanded = open, onDismissRequest = { open = false }) {
            options.forEach { (id, title) -> DropdownMenuItem(text = { Text(title) }, onClick = { open = false; pick(id) }) }
        }
    }
}

/**
 * The thread a one-thread task posts into: a field that opens the project's
 * threads, searched by title, with "Start one with the first run" first for a
 * task not bound yet.
 */
@Composable
private fun ThreadChoice(threads: List<ScheduledThreadChoiceView>, firstRunStartsThread: Boolean, selected: String, pick: (String) -> Unit) {
    var open by remember { mutableStateOf(false) }
    var query by remember { mutableStateOf("") }
    val shown = threads.find { it.id == selected }?.title?.ifEmpty { "New thread" }
        ?: if (selected.isEmpty()) (if (firstRunStartsThread) "Started by the first run" else "Choose…") else "A thread not on this device"
    Box(Modifier.fillMaxWidth()) {
        OutlinedTextField(shown, {}, readOnly = true, label = { Text("Which thread") }, modifier = Modifier.fillMaxWidth())
        // Over the field: a read-only text field takes no clicks of its own.
        Box(Modifier.matchParentSize().clickable { query = ""; open = true })
    }
    if (open) {
        val matching = threads.filter { query.isBlank() || it.title.contains(query.trim(), ignoreCase = true) }
        AlertDialog(
            onDismissRequest = { open = false },
            title = { Text("Thread") },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
                    OutlinedTextField(query, { query = it }, label = { Text("Thread title") }, singleLine = true, modifier = Modifier.fillMaxWidth())
                    LazyColumn(Modifier.heightIn(max = 420.dp)) {
                        if (firstRunStartsThread && query.isBlank()) item("first") {
                            ListItem(
                                headlineContent = { Text("Start one with the first run") },
                                supportingContent = { Text("Later runs go to the thread it starts.") },
                                modifier = Modifier.clickable { open = false; pick("") },
                            )
                        }
                        items(matching, key = { it.id }) { c ->
                            ListItem(
                                headlineContent = { Text(c.title.ifEmpty { "New thread" }, maxLines = 2) },
                                supportingContent = { if (c.detail.isNotEmpty()) Text(c.detail) },
                                trailingContent = { if (c.id == selected) Text("✓") },
                                modifier = Modifier.clickable { open = false; pick(c.id) },
                            )
                        }
                        if (matching.isEmpty()) item("none") { Text("No thread of this project is called that.", modifier = Modifier.padding(Space.m)) }
                    }
                }
            },
            confirmButton = { TextButton(onClick = { open = false }) { Text("Close") } },
        )
    }
}

@Composable
private fun Toggle(label: String, on: Boolean, set: (Boolean) -> Unit) {
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        Text(label, Modifier.weight(1f), style = MaterialTheme.typography.bodyLarge)
        Switch(checked = on, onCheckedChange = set)
    }
}
