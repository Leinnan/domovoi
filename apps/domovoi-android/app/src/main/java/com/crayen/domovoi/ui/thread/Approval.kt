package com.crayen.domovoi.ui.thread

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
                            if (request.prompt.length > 240 || request.prompt.count { it == '\n' } > 5 || (request.detail?.length ?: 0) > 160) {
                                TextButton(onClick = { showing = request }) { Text("Show all") }
                            }
                        }
                        Row(horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                            OutlinedButton(onClick = { send(Event.Respond(request.id, "decline")) }, modifier = Modifier.weight(1f)) { Text("Decline") }
                            FilledTonalButton(onClick = { send(Event.Respond(request.id, "acceptForSession")) }, modifier = Modifier.weight(1f)) { Text("Session", maxLines = 1) }
                            Button(onClick = { send(Event.Respond(request.id, "accept")) }, modifier = Modifier.weight(1f)) { Text("Allow", fontWeight = FontWeight.SemiBold) }
                        }
                    }
                }
            }
        }
    }
    showing?.let { request ->
        ModalBottomSheet(onDismissRequest = { showing = null }) {
            Column(Modifier.verticalScroll(rememberScrollState()).padding(horizontal = Space.xl), verticalArrangement = Arrangement.spacedBy(Space.m)) {
                Text(kindLabel(request.kind), style = MaterialTheme.typography.titleMedium)
                SelectionContainer { Text(request.prompt, fontFamily = if (request.kind == "command") Mono else null, style = MaterialTheme.typography.bodyMedium) }
                request.detail?.let { SelectionContainer { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant) } }
                Spacer(Modifier.height(Space.xl))
            }
        }
    }
}

/** Questions with options: a choice (or several) each, or an answer of your own. */
@Composable
private fun QuestionForm(request: RequestView, send: (Event) -> Unit) {
    val chosen = remember(request.id) { mutableStateMapOf<String, Set<String>>() }
    val other = remember(request.id) { mutableStateMapOf<String, String>() }
    val haptics = LocalHapticFeedback.current
    fun values(q: QuestionView): List<String> {
        val picked = q.options.map { it.label }.filter { chosen[q.id]?.contains(it) == true }
        val typed = other[q.id].orEmpty().trim()
        return if (typed.isEmpty()) picked else picked + typed
    }
    val tint = LocalTints.current.question
    Column(verticalArrangement = Arrangement.spacedBy(Space.l)) {
        request.questions.forEach { q ->
            Column(verticalArrangement = Arrangement.spacedBy(Space.xs)) {
                Text(
                    q.header.ifEmpty { "Question" } + if (q.multiSelect) " · choose any" else "",
                    style = MaterialTheme.typography.labelMedium, color = tint, fontWeight = FontWeight.SemiBold,
                )
                Text(q.question, style = MaterialTheme.typography.bodyLarge, fontWeight = FontWeight.Medium)
                q.options.forEach { option ->
                    val on = chosen[q.id]?.contains(option.label) == true
                    val toggle = {
                        val set = chosen[q.id] ?: emptySet()
                        chosen[q.id] = if (q.multiSelect) { if (on) set - option.label else set + option.label } else { if (on) emptySet() else setOf(option.label) }
                        if (!q.multiSelect) other[q.id] = ""
                        haptics.performHapticFeedback(HapticFeedbackType.SegmentTick)
                    }
                    Surface(
                        color = if (on) tint.copy(alpha = 0.12f) else MaterialTheme.colorScheme.surfaceContainerHigh,
                        shape = MaterialTheme.shapes.small,
                        modifier = Modifier.fillMaxWidth().clickable(onClick = toggle),
                    ) {
                        Row(Modifier.padding(end = Space.m, top = Space.xxs, bottom = Space.xxs), verticalAlignment = Alignment.CenterVertically) {
                            if (q.multiSelect) Checkbox(checked = on, onCheckedChange = { toggle() }) else RadioButton(selected = on, onClick = toggle)
                            Column(Modifier.weight(1f)) {
                                Text(option.label, style = MaterialTheme.typography.bodyMedium)
                                if (option.description.isNotEmpty() && option.description != option.label) {
                                    Text(option.description, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                                }
                            }
                        }
                    }
                }
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
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(Space.s, Alignment.End)) {
            OutlinedButton(onClick = { send(Event.DismissUserInput(request.id)) }) { Text("Skip") }
            Button(
                onClick = { send(Event.RespondUserInput(request.id, request.questions.map { QuestionAnswer(it.id, values(it)) })) },
                enabled = request.questions.all { values(it).isNotEmpty() },
            ) { Text("Submit", fontWeight = FontWeight.SemiBold) }
        }
    }
}
