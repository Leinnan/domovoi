package com.crayen.domovoi.ui.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExposedDropdownMenuAnchorType
import androidx.compose.material3.ExposedDropdownMenuBox
import androidx.compose.material3.ExposedDropdownMenuDefaults
import androidx.compose.material3.FilterChip
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
import com.crayen.domovoi.app.ScheduledTaskEdit
import com.crayen.domovoi.app.ScheduledTaskView
import com.crayen.domovoi.ui.theme.Space
import java.time.LocalDateTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter

/** How often a task can run: the same short list as the Apple apps. */
val scheduleIntervals = listOf(5 to "5 minutes", 15 to "15 minutes", 30 to "30 minutes", 60 to "hour", 180 to "3 hours", 360 to "6 hours", 720 to "12 hours", 1440 to "day", 10080 to "week")

/** A new task in `projectId`: daily at 9:00 in a new thread, with the composer's model. */
fun newScheduledTask(projectId: String) = ScheduledTaskEdit(
    id = "", name = "", prompt = "", projectId = projectId, scheduleKind = "fixed_time", intervalMinutes = 60u, timeOfDay = "09:00", weekdays = emptyList(),
    runAt = "", threadMode = "new_thread", threadId = "", workspace = "root", baseBranch = "", modelInstance = "", model = "",
    runtimeMode = "approval-required", interactionMode = "default", enabled = true,
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
    threadsOf: (String) -> List<Pair<String, String>>,
    models: List<Pair<String, String>>,
    onDismiss: () -> Unit,
    save: (ScheduledTaskEdit) -> Unit,
) {
    var t by remember { mutableStateOf(start) }
    var runAt by remember { mutableStateOf(shownRunAt(start.runAt)) }
    val days = listOf("S", "M", "T", "W", "T", "F", "S")
    val timeOk = Regex("^([01]?\\d|2[0-3]):[0-5]\\d$").matches(t.timeOfDay.trim())
    val complete = t.name.isNotBlank() && t.prompt.isNotBlank() && t.projectId.isNotEmpty() && (t.threadMode == "new_thread" || t.threadId.isNotEmpty()) &&
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
            Choice("Thread", listOf("new_thread" to "A new thread each time", "continue" to "Continue one thread", "fresh_session" to "One thread, fresh session each time"), t.threadMode) { t = t.copy(threadMode = it) }
            if (t.threadMode != "new_thread") Choice("Which thread", threadsOf(t.projectId), t.threadId) { t = t.copy(threadId = it) }
            Choice("Folder", listOf("root" to "Project folder", "worktree" to "New worktree"), t.workspace) { t = t.copy(workspace = it) }
            if (t.workspace == "worktree") OutlinedTextField(t.baseBranch, { t = t.copy(baseBranch = it) }, label = { Text("Base branch") }, singleLine = true, modifier = Modifier.fillMaxWidth())

            Text("Agent", style = MaterialTheme.typography.titleSmall)
            Choice("Model", listOf("" to "The composer's model") + models, if (t.model.isEmpty()) "" else "${t.modelInstance}|${t.model}") { key ->
                val parts = key.split("|", limit = 2)
                t = t.copy(modelInstance = parts.getOrElse(0) { "" }, model = parts.getOrElse(1) { "" })
            }
            Choice("Permissions", listOf("approval-required" to "Ask first", "auto-accept-edits" to "Accept edits", "full-access" to "Full access"), t.runtimeMode) { t = t.copy(runtimeMode = it) }
            Toggle("Plan first", t.interactionMode == "plan") { t = t.copy(interactionMode = if (it) "plan" else "default") }
            Toggle("Enabled", t.enabled) { t = t.copy(enabled = it) }

            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(Space.s, Alignment.End)) {
                TextButton(onClick = onDismiss) { Text("Cancel") }
                Button(onClick = { save(t.copy(name = t.name.trim(), prompt = t.prompt.trim(), timeOfDay = t.timeOfDay.trim(), runAt = if (t.scheduleKind == "once") isoRunAt(runAt) else "")) }, enabled = complete) {
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

@Composable
private fun Toggle(label: String, on: Boolean, set: (Boolean) -> Unit) {
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        Text(label, Modifier.weight(1f), style = MaterialTheme.typography.bodyLarge)
        Switch(checked = on, onCheckedChange = set)
    }
}
