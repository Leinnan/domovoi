package com.crayen.domovoi.ui.thread

import android.content.res.Configuration
import androidx.compose.foundation.focusable
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.app.QuestionAnswer
import com.crayen.domovoi.app.QuestionView
import com.crayen.domovoi.app.RequestView
import com.crayen.domovoi.ui.theme.LocalTints
import com.crayen.domovoi.ui.theme.Mono
import com.crayen.domovoi.ui.theme.Space

private fun kindLabel(kind: String) = when (kind) {
    "user_input" -> "Question"
    "command" -> "Run command"
    "file-change" -> "Edit files"
    "file-read" -> "Read files"
    else -> "Permission"
}

/** What the agent waits for, docked above the composer: approvals and questions. */
@Composable
fun ApprovalBar(requests: List<RequestView>, send: (Event) -> Unit) {
    val haptics = LocalHapticFeedback.current
    val view = LocalView.current
    var showing by remember { mutableStateOf<RequestView?>(null) }
    val ids = requests.map { it.id }
    LaunchedEffect(ids) {
        haptics.performHapticFeedback(HapticFeedbackType.LongPress)
        view.announceForAccessibility("The agent needs your answer")
    }
    val tint = LocalTints.current.caution
    Surface(
        color = MaterialTheme.colorScheme.surfaceContainerHigh,
        shape = MaterialTheme.shapes.extraLarge,
        tonalElevation = 3.dp,
        shadowElevation = 4.dp,
        modifier = Modifier.fillMaxWidth().padding(horizontal = Space.s, vertical = Space.xs),
    ) {
        Column(
            Modifier.heightIn(max = if (requests.any { it.questions.isNotEmpty() }) 480.dp else 320.dp)
                .verticalScroll(rememberScrollState()).padding(Space.l),
            verticalArrangement = Arrangement.spacedBy(Space.m),
        ) {
            requests.forEach { request ->
                when {
                    request.kind == "user_input" && request.questions.isNotEmpty() -> QuestionForm(request, send)
                    request.kind == "user_input" -> Row(verticalAlignment = Alignment.Top, horizontalArrangement = Arrangement.spacedBy(Space.m)) {
                        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(Space.xs)) {
                            Text("Question", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, fontWeight = FontWeight.SemiBold)
                            SelectionContainer { Text(request.prompt, style = MaterialTheme.typography.bodyMedium) }
                            request.detail?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
                            Text("Answer in the message box below.", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                        OutlinedButton(onClick = { send(Event.Respond(request.id, "decline")) }) { Text("Skip") }
                    }
                    else -> Column(verticalArrangement = Arrangement.spacedBy(Space.m)) {
                        Column(verticalArrangement = Arrangement.spacedBy(Space.xxs)) {
                            Text(kindLabel(request.kind), style = MaterialTheme.typography.labelMedium, color = tint, fontWeight = FontWeight.SemiBold)
                            Text(
                                request.prompt,
                                fontFamily = if (request.kind == "command") Mono else null,
                                style = MaterialTheme.typography.bodyMedium,
                                maxLines = 6, overflow = TextOverflow.Ellipsis,
                            )
                            request.detail?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 3, overflow = TextOverflow.Ellipsis) }
                            if (request.files.isNotEmpty()) {
                                val n = request.files.size
                                TextButton(onClick = { showing = request }) { Text("Review changes ($n file${if (n == 1) "" else "s"})") }
                            } else if (request.prompt.length > 240 || request.prompt.count { it == '\n' } > 5 || (request.detail?.length ?: 0) > 160) {
                                TextButton(onClick = { showing = request }) { Text("Show all") }
                            }
                        }
                        Row(horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                            OutlinedButton(onClick = { send(Event.Respond(request.id, "decline")) }, modifier = Modifier.weight(1f)) { Text("Decline") }
                            FilledTonalButton(onClick = { send(Event.Respond(request.id, "acceptForSession")) }, modifier = Modifier.weight(1f)) { Text(if (request.sessionLabel.endsWith("Thread")) "Thread" else "Session", maxLines = 1) }
                            Button(onClick = { send(Event.Respond(request.id, "accept")) }, modifier = Modifier.weight(1f)) { Text("Allow", fontWeight = FontWeight.SemiBold) }
                        }
                    }
                }
            }
        }
    }
    showing?.let { request ->
        // The whole request; for file changes, each file's diff, then the answer.
        val reviewing = request.files.isNotEmpty()
        ModalBottomSheet(onDismissRequest = { showing = null }, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = reviewing)) {
            Column(Modifier.verticalScroll(rememberScrollState()).padding(horizontal = Space.xl), verticalArrangement = Arrangement.spacedBy(Space.m)) {
                Text(kindLabel(request.kind), style = MaterialTheme.typography.titleMedium)
                SelectionContainer { Text(request.prompt, fontFamily = if (request.kind == "command") Mono else null, style = MaterialTheme.typography.bodyMedium) }
                request.detail?.let { SelectionContainer { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant) } }
                request.files.forEach { file ->
                    Column(verticalArrangement = Arrangement.spacedBy(Space.xs)) {
                        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                            Text(file.path, fontFamily = Mono, fontSize = 13.sp, maxLines = 1, overflow = TextOverflow.StartEllipsis, modifier = Modifier.weight(1f))
                            Text("+${file.additions}", color = LocalTints.current.success, style = MaterialTheme.typography.labelMedium)
                            Text("−${file.deletions}", color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.labelMedium)
                        }
                        DiffFileBody(file)
                    }
                }
                if (reviewing && request.kind != "user_input") {
                    Row(horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                        OutlinedButton(onClick = { send(Event.Respond(request.id, "decline")); showing = null }, modifier = Modifier.weight(1f)) { Text("Decline") }
                        Button(onClick = { send(Event.Respond(request.id, "accept")); showing = null }, modifier = Modifier.weight(1f)) { Text("Allow", fontWeight = FontWeight.SemiBold) }
                    }
                }
                Spacer(Modifier.height(Space.xl))
            }
        }
    }
    // Answered elsewhere (or by another client): the sheet goes with it.
    LaunchedEffect(ids) { if (showing != null && showing?.id !in ids) showing = null }
}

/**
 * Questions with options: a choice (or several) each, or an answer of your own.
 * Several questions go one at a time ("1/3", Previous, Next); the answers stay
 * here until Submit sends them all. With a hardware keyboard, 1–9 pick options.
 */
@Composable
private fun QuestionForm(request: RequestView, send: (Event) -> Unit) {
    val chosen = remember(request.id) { mutableStateMapOf<String, Set<String>>() }
    val other = remember(request.id) { mutableStateMapOf<String, String>() }
    var page by rememberSaveable(request.id) { mutableIntStateOf(0) }
    val haptics = LocalHapticFeedback.current
    val keys = remember(request.id) { FocusRequester() }
    val configuration = LocalConfiguration.current
    fun values(q: QuestionView): List<String> {
        val picked = q.options.map { it.value }.filter { chosen[q.id]?.contains(it) == true }
        val typed = if (q.allowCustomAnswer) other[q.id].orEmpty().trim() else ""
        return if (typed.isEmpty()) picked else picked + typed
    }
    fun toggle(q: QuestionView, label: String) {
        val on = chosen[q.id]?.contains(label) == true
        val set = chosen[q.id] ?: emptySet()
        chosen[q.id] = if (q.multiSelect) { if (on) set - label else set + label } else { if (on) emptySet() else setOf(label) }
        if (!q.multiSelect) other[q.id] = ""
        haptics.performHapticFeedback(HapticFeedbackType.SegmentTick)
    }
    val total = request.questions.size
    val paged = total > 1
    val current = page.coerceIn(0, total - 1)
    val shown = if (paged) listOf(request.questions[current]) else request.questions
    // A hardware keyboard: the form takes the keys so digits pick options.
    LaunchedEffect(request.id) {
        if (configuration.keyboard != Configuration.KEYBOARD_NOKEYS) runCatching { keys.requestFocus() }
    }
    val tint = LocalTints.current.question
    Column(
        Modifier.focusRequester(keys).focusable().onKeyEvent { e ->
            if (e.type != KeyEventType.KeyDown) return@onKeyEvent false
            val digit = digitOf(e.key) ?: return@onKeyEvent false
            val q = shown.first()
            val option = q.options.getOrNull(digit - 1) ?: return@onKeyEvent false
            toggle(q, option.value)
            true
        },
        verticalArrangement = Arrangement.spacedBy(Space.l),
    ) {
        if (paged) {
            Text("${current + 1}/$total", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        shown.forEach { q ->
            Column(verticalArrangement = Arrangement.spacedBy(Space.xs)) {
                Text(
                    q.header.ifEmpty { "Question" } + if (q.multiSelect) " · choose any" else "",
                    style = MaterialTheme.typography.labelMedium, color = tint, fontWeight = FontWeight.SemiBold,
                )
                Text(q.question, style = MaterialTheme.typography.bodyLarge, fontWeight = FontWeight.Medium)
                q.options.forEachIndexed { index, option ->
                    val on = chosen[q.id]?.contains(option.value) == true
                    Surface(
                        color = if (on) tint.copy(alpha = 0.12f) else MaterialTheme.colorScheme.surfaceContainerHigh,
                        shape = MaterialTheme.shapes.small,
                        modifier = Modifier.fillMaxWidth().clickable { toggle(q, option.value) },
                    ) {
                        Row(Modifier.padding(end = Space.m, top = Space.xxs, bottom = Space.xxs), verticalAlignment = Alignment.CenterVertically) {
                            if (q.multiSelect) Checkbox(checked = on, onCheckedChange = { toggle(q, option.value) }) else RadioButton(selected = on, onClick = { toggle(q, option.value) })
                            Column(Modifier.weight(1f)) {
                                Text(option.label, style = MaterialTheme.typography.bodyMedium)
                                if (option.description.isNotEmpty() && option.description != option.label) {
                                    Text(option.description, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                                }
                            }
                            if (index < 9) Text("${index + 1}", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.outline)
                        }
                    }
                }
                if (q.allowCustomAnswer) {
                    OutlinedTextField(
                        value = other[q.id].orEmpty(),
                        onValueChange = { text ->
                            other[q.id] = text
                            // Typing an answer of your own replaces a single choice.
                            if (!q.multiSelect && text.isNotEmpty()) chosen[q.id] = emptySet()
                        },
                        placeholder = { Text("Other…") },
                        maxLines = 3,
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
            }
        }
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(Space.s, Alignment.End), verticalAlignment = Alignment.CenterVertically) {
            OutlinedButton(onClick = { send(Event.DismissUserInput(request.id)) }) { Text("Skip") }
            Spacer(Modifier.weight(1f))
            if (paged) {
                TextButton(onClick = { page = current - 1 }, enabled = current > 0) { Text("Previous") }
            }
            if (paged && current < total - 1) {
                Button(onClick = { page = current + 1 }, enabled = !shown.first().required || values(shown.first()).isNotEmpty()) { Text("Next", fontWeight = FontWeight.SemiBold) }
            } else {
                Button(
                    onClick = { send(Event.RespondUserInput(request.id, request.questions.map { QuestionAnswer(it.id, values(it)) })) },
                    enabled = request.questions.all { !it.required || values(it).isNotEmpty() },
                ) { Text("Submit", fontWeight = FontWeight.SemiBold) }
            }
        }
    }
}

private fun digitOf(key: Key): Int? = when (key) {
    Key.One, Key.NumPad1 -> 1
    Key.Two, Key.NumPad2 -> 2
    Key.Three, Key.NumPad3 -> 3
    Key.Four, Key.NumPad4 -> 4
    Key.Five, Key.NumPad5 -> 5
    Key.Six, Key.NumPad6 -> 6
    Key.Seven, Key.NumPad7 -> 7
    Key.Eight, Key.NumPad8 -> 8
    Key.Nine, Key.NumPad9 -> 9
    else -> null
}
