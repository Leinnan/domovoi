package com.crayen.domovoi.ui.thread

import android.Manifest
import android.content.ClipboardManager
import android.content.pm.PackageManager
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.animateColorAsState
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.SubdirectoryArrowRight
import androidx.compose.material.icons.filled.ArrowUpward
import androidx.compose.material.icons.filled.Stop
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.CameraAlt
import androidx.compose.material.icons.outlined.Checklist
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.ContentPaste
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material.icons.outlined.Folder
import androidx.compose.material.icons.outlined.GraphicEq
import androidx.compose.material.icons.outlined.Image
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material.icons.outlined.LockOpen
import androidx.compose.material.icons.outlined.Memory
import androidx.compose.material.icons.outlined.Mic
import androidx.compose.material.icons.outlined.Photo
import androidx.compose.material.icons.outlined.Speed
import androidx.compose.material.icons.outlined.Warning
import androidx.compose.material.icons.outlined.AutoAwesome
import androidx.compose.material.icons.outlined.Delete
import androidx.compose.material.icons.outlined.Inventory2
import androidx.compose.material.icons.outlined.Unarchive
import androidx.compose.material3.AssistChipDefaults
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.ui.graphics.Color
import com.crayen.domovoi.app.LimitWindowView
import com.crayen.domovoi.app.SlashCommandView
import com.crayen.domovoi.ui.common.LimitWindowRow
import androidx.compose.material.icons.outlined.EditNote
import androidx.compose.material3.AssistChip
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.FilledTonalIconButton
import androidx.compose.material3.FilterChip
import androidx.compose.material3.FilterChipDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.InputChip
import androidx.compose.material3.InputChipDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.SuggestionChip
import androidx.compose.material3.SuggestionChipDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.isShiftPressed
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.core.content.FileProvider
import com.crayen.domovoi.app.ComposerView
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.core.Core
import com.crayen.domovoi.ui.common.watch
import com.crayen.domovoi.ui.theme.LocalTints
import com.crayen.domovoi.ui.theme.Mono
import com.crayen.domovoi.ui.theme.Space
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.File

object ModeInfo {
    fun permissionIcon(mode: String) = when (mode) {
        "full-access" -> Icons.Outlined.LockOpen
        "auto-accept-edits" -> Icons.Outlined.EditNote
        else -> Icons.Outlined.Lock
    }
    fun permissionShort(mode: String, label: String) = when (mode) {
        "approval-required" -> "Ask"
        "auto-accept-edits" -> "Auto-edit"
        "full-access" -> "Full access"
        else -> label
    }
    fun permissionDetail(mode: String) = when (mode) {
        "approval-required" -> "Asks before it runs commands or changes files."
        "auto-accept-edits" -> "Edits files on its own; asks before commands."
        "full-access" -> "Runs commands and edits files without asking."
        else -> ""
    }
    const val FULL_ACCESS_TITLE = "Give the agent full access?"
    const val FULL_ACCESS_MESSAGE = "It will run commands and change files on the server without asking first."
}

/**
 * The message box: a docked panel with the field, suggestion chips (@ files,
 * / commands), attachments, mode chips (plan, effort, permissions, model)
 * and one button that is Send, Steer or Stop as the turn needs.
 */
@Composable
fun Composer(core: Core, modifier: Modifier = Modifier) {
    val composer by core.view.composer.watch()
    val threadId by core.view.threadId.watch()
    val context = LocalContext.current
    val haptics = LocalHapticFeedback.current
    val focus = LocalFocusManager.current
    val scope = rememberCoroutineScope()
    var field by remember { mutableStateOf(TextFieldValue(composer.draft, TextRange(composer.draft.length))) }
    var justSent by remember { mutableStateOf<String?>(null) }
    var options by remember { mutableStateOf<ComposerOption?>(null) }
    var attachError by remember { mutableStateOf<String?>(null) }
    var addMenu by remember { mutableStateOf(false) }
    var stashList by remember { mutableStateOf(false) }
    var limitsList by remember { mutableStateOf(false) }
    val dictation = remember { Dictation(context) }
    var dictationBase by remember { mutableStateOf("") }
    val planning = composer.interactionMode == "plan"
    val tints = LocalTints.current

    // The core's draft comes back on a new revision or another thread.
    LaunchedEffect(composer.draftRevision, threadId) {
        if (field.text != composer.draft) field = TextFieldValue(composer.draft, TextRange(composer.draft.length))
    }
    // `/model` asks for the model picker: open it when the count goes up (not at first sight).
    var pickerSeen by remember { mutableStateOf(composer.modelPickerRequested) }
    LaunchedEffect(composer.modelPickerRequested) {
        if (composer.modelPickerRequested > pickerSeen) options = ComposerOption.Model
        pickerSeen = composer.modelPickerRequested
    }
    LaunchedEffect(attachError) { if (attachError != null) { delay(4000); attachError = null } }
    DisposableEffect(Unit) { onDispose { dictation.stop() } }

    fun setText(text: String) {
        field = TextFieldValue(text, TextRange(text.length))
        if (text != composer.draft) core.update(Event.SetDraft(text))
    }
    fun attach(images: List<PreparedImage?>) {
        val failed = images.count { it == null }
        images.filterNotNull().forEach { core.update(Event.AddAttachment(it.name, "image/jpeg", it.sizeBytes.toULong(), it.dataUrl)) }
        if (failed > 0) attachError = if (failed == 1) "A photo couldn't be attached." else "$failed photos couldn't be attached."
    }
    fun finishSend(event: Event) {
        dictation.stop()
        core.update(Event.SetDraft(field.text))
        core.update(event)
        justSent = field.text
        field = TextFieldValue("")
        focus.clearFocus()
        haptics.performHapticFeedback(HapticFeedbackType.Confirm)
        scope.launch { delay(1500); justSent = null }
    }

    val pickPhotos = rememberLauncherForActivityResult(ActivityResultContracts.PickMultipleVisualMedia(4)) { uris ->
        if (uris.isNotEmpty()) scope.launch {
            attach(withContext(Dispatchers.IO) { uris.map { Images.prepare(context, it) } })
        }
    }
    var cameraUri by remember { mutableStateOf<Uri?>(null) }
    val takePhoto = rememberLauncherForActivityResult(ActivityResultContracts.TakePicture()) { ok ->
        val uri = cameraUri
        if (ok && uri != null) scope.launch { attach(withContext(Dispatchers.IO) { listOf(Images.prepare(context, uri)) }) }
    }
    val askMic = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) {
            dictationBase = field.text
            dictation.toggle { t -> setText(dictationBase + (if (dictationBase.isEmpty() || dictationBase.endsWith(" ") || dictationBase.endsWith("\n")) "" else " ") + t) }
        } else attachError = "Domovoi may not use the microphone."
    }

    Column(modifier.fillMaxWidth().padding(horizontal = Space.s, vertical = Space.xs), verticalArrangement = Arrangement.spacedBy(Space.s)) {
        ProviderNotice(composer) { core.update(Event.RefreshProviders) }
        val mention = composer.mention
        if (mention != null && mention.suggestions.isNotEmpty()) {
            ChipRow {
                mention.suggestions.forEachIndexed { index, entry ->
                    // The one the keyboard's arrows have reached (Enter or Tab takes it).
                    val highlighted = index == mention.highlighted.toInt()
                    SuggestionChip(
                        onClick = { core.update(Event.PickMention(entry.path)) },
                        label = { Text(entry.name) },
                        icon = { Icon(if (entry.directory) Icons.Outlined.Folder else Icons.Outlined.Description, null, modifier = Modifier.size(16.dp)) },
                        colors = if (highlighted) SuggestionChipDefaults.suggestionChipColors(
                            containerColor = MaterialTheme.colorScheme.secondaryContainer,
                            labelColor = MaterialTheme.colorScheme.onSecondaryContainer,
                            iconContentColor = MaterialTheme.colorScheme.onSecondaryContainer,
                        ) else SuggestionChipDefaults.suggestionChipColors(),
                    )
                }
            }
        } else if (composer.slashCommands.isNotEmpty()) {
            ChipRow {
                composer.slashCommands.forEach { command ->
                    SuggestionChip(onClick = { core.update(Event.PickSlashCommand(command.name)) }, label = { CommandLabel("/", command) })
                }
            }
        } else if (composer.skills.isNotEmpty()) {
            ChipRow {
                composer.skills.forEach { skill ->
                    SuggestionChip(
                        onClick = { core.update(Event.PickSkill(skill.name)) },
                        label = { CommandLabel("$", skill) },
                        icon = { Icon(Icons.Outlined.AutoAwesome, null, modifier = Modifier.size(16.dp)) },
                    )
                }
            }
        }
        val border by animateColorAsState(if (planning) tints.plan.copy(alpha = 0.6f) else MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.5f), label = "border")
        Surface(
            color = if (planning) tints.planContainer.copy(alpha = 0.45f) else MaterialTheme.colorScheme.surfaceContainerHigh,
            shape = MaterialTheme.shapes.extraLarge,
            border = BorderStroke(1.dp, border),
            tonalElevation = 2.dp,
        ) {
            Column(Modifier.padding(Space.m), verticalArrangement = Arrangement.spacedBy(Space.s)) {
                if (composer.attachments.isNotEmpty()) {
                    ChipRow {
                        composer.attachments.forEach { a ->
                            InputChip(
                                selected = false,
                                onClick = {},
                                label = { Text(a.name, maxLines = 1) },
                                leadingIcon = { Icon(Icons.Outlined.Photo, null, modifier = Modifier.size(16.dp)) },
                                trailingIcon = {
                                    if (a.uploading) CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 1.5.dp)
                                    else IconButton(onClick = { core.update(Event.RemoveAttachment(a.id)) }, modifier = Modifier.size(20.dp)) { Icon(Icons.Outlined.Close, "Remove ${a.name}", modifier = Modifier.size(16.dp)) }
                                },
                            )
                        }
                    }
                }
                Row(verticalAlignment = Alignment.Bottom) {
                    Box(Modifier.weight(1f).padding(horizontal = Space.xs, vertical = Space.s)) {
                        if (field.text.isEmpty()) {
                            Text(
                                when { dictation.listening -> "Listening…"; planning -> "Describe what to plan…"; else -> composer.placeholder.ifEmpty { "Message" } },
                                style = MaterialTheme.typography.bodyLarge, color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        BasicTextField(
                            value = field,
                            onValueChange = { value ->
                                val sent = justSent
                                if (sent != null && value.text.isNotEmpty() && sameMessage(value.text, sent)) {
                                    field = TextFieldValue("")
                                    return@BasicTextField
                                }
                                field = value
                                if (value.text != composer.draft) core.update(Event.SetDraft(value.text))
                            },
                            textStyle = MaterialTheme.typography.bodyLarge.copy(color = MaterialTheme.colorScheme.onSurface),
                            cursorBrush = SolidColor(MaterialTheme.colorScheme.primary),
                            maxLines = 8,
                            modifier = Modifier.fillMaxWidth().heightIn(min = 24.dp).onPreviewKeyEvent { e ->
                                // A hardware keyboard: arrows move through the @ suggestions,
                                // Enter or Tab takes one, Escape closes them; Tab or Enter
                                // takes the first slash command or skill; with no list open,
                                // Up and Down step through the thread's earlier prompts.
                                if (e.type != KeyEventType.KeyDown) return@onPreviewKeyEvent false
                                val picking = composer.mention?.suggestions?.isNotEmpty() == true
                                val accept = e.key == Key.Enter || e.key == Key.NumPadEnter || e.key == Key.Tab
                                val listing = picking || composer.slashCommands.isNotEmpty() || composer.skills.isNotEmpty()
                                val caret = field.selection
                                when {
                                    // Earlier prompts: Up on the draft's first line, Down on its last.
                                    !listing && caret.collapsed && !e.isShiftPressed && e.key == Key.DirectionUp && '\n' !in field.text.take(caret.start) -> {
                                        core.update(Event.RecallPrompt(-1)); true
                                    }
                                    !listing && caret.collapsed && !e.isShiftPressed && e.key == Key.DirectionDown && '\n' !in field.text.drop(caret.start) -> {
                                        core.update(Event.RecallPrompt(1)); true
                                    }
                                    picking && e.key == Key.DirectionUp -> { core.update(Event.MoveMention(-1)); true }
                                    picking && e.key == Key.DirectionDown -> { core.update(Event.MoveMention(1)); true }
                                    picking && accept && !e.isShiftPressed -> {
                                        if (field.text != composer.draft) core.update(Event.SetDraft(field.text))
                                        core.update(Event.PickMention(""))
                                        true
                                    }
                                    picking && e.key == Key.Escape -> { core.update(Event.DismissMention); true }
                                    !picking && accept && !e.isShiftPressed && composer.slashCommands.isNotEmpty() -> {
                                        if (field.text != composer.draft) core.update(Event.SetDraft(field.text))
                                        core.update(Event.PickSlashCommand(composer.slashCommands.first().name))
                                        true
                                    }
                                    !picking && accept && !e.isShiftPressed && composer.skills.isNotEmpty() -> {
                                        if (field.text != composer.draft) core.update(Event.SetDraft(field.text))
                                        core.update(Event.PickSkill(composer.skills.first().name))
                                        true
                                    }
                                    else -> false
                                }
                            },
                        )
                    }
                    IconButton(onClick = {
                        if (dictation.listening) dictation.stop()
                        else if (ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) {
                            dictationBase = field.text
                            dictation.toggle { t -> setText(dictationBase + (if (dictationBase.isEmpty() || dictationBase.endsWith(" ") || dictationBase.endsWith("\n")) "" else " ") + t) }
                        } else askMic.launch(Manifest.permission.RECORD_AUDIO)
                    }) {
                        Icon(
                            if (dictation.listening) Icons.Outlined.GraphicEq else Icons.Outlined.Mic,
                            if (dictation.listening) "Stop dictation" else "Dictate",
                            tint = if (dictation.listening) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
                (attachError ?: dictation.error)?.let {
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.xs)) {
                        Icon(Icons.Outlined.Warning, null, tint = tints.caution, modifier = Modifier.size(14.dp))
                        Text(it, style = MaterialTheme.typography.labelMedium, color = tints.caution)
                    }
                }
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.xs)) {
                    Box {
                        FilledTonalIconButton(onClick = { addMenu = true }, modifier = Modifier.size(36.dp)) { Icon(Icons.Outlined.Add, "Attach") }
                        DropdownMenu(expanded = addMenu, onDismissRequest = { addMenu = false }) {
                            DropdownMenuItem(text = { Text("Photos") }, leadingIcon = { Icon(Icons.Outlined.Image, null) }, onClick = {
                                addMenu = false
                                pickPhotos.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly))
                            })
                            if (context.packageManager.hasSystemFeature(PackageManager.FEATURE_CAMERA_ANY)) {
                                DropdownMenuItem(text = { Text("Take photo") }, leadingIcon = { Icon(Icons.Outlined.CameraAlt, null) }, onClick = {
                                    addMenu = false
                                    val dir = File(context.cacheDir, "camera").apply { mkdirs() }
                                    val file = File(dir, "photo-${System.currentTimeMillis()}.jpg")
                                    val uri = FileProvider.getUriForFile(context, "${context.packageName}.files", file)
                                    cameraUri = uri
                                    takePhoto.launch(uri)
                                })
                            }
                            DropdownMenuItem(text = { Text("Paste image") }, leadingIcon = { Icon(Icons.Outlined.ContentPaste, null) }, onClick = {
                                addMenu = false
                                val clip = context.getSystemService(ClipboardManager::class.java)?.primaryClip
                                val uri = clip?.takeIf { it.itemCount > 0 }?.getItemAt(0)?.uri
                                if (uri == null) attachError = "The clipboard has no image."
                                else scope.launch { attach(withContext(Dispatchers.IO) { listOf(Images.prepare(context, uri)) }) }
                            })
                            HorizontalDivider()
                            DropdownMenuItem(
                                text = { Text("Stash draft") }, leadingIcon = { Icon(Icons.Outlined.Inventory2, null) },
                                enabled = field.text.isNotBlank(),
                                onClick = {
                                    addMenu = false
                                    if (field.text != composer.draft) core.update(Event.SetDraft(field.text))
                                    core.update(Event.StashDraft)
                                },
                            )
                            if (composer.stash.isNotEmpty()) {
                                DropdownMenuItem(
                                    text = { Text("Stash (${composer.stash.size})") }, leadingIcon = { Icon(Icons.Outlined.Unarchive, null) },
                                    onClick = { addMenu = false; stashList = true },
                                )
                            }
                        }
                    }
                    Box(Modifier.weight(1f)) { ModeChips(composer, open = { options = it }, showLimits = { limitsList = true }, togglePlan = {
                        haptics.performHapticFeedback(HapticFeedbackType.SegmentTick)
                        core.update(Event.SetInteractionMode(if (planning) "default" else "plan"))
                    }) }
                    ActionButtons(
                        composer = composer,
                        typed = field.text.isNotBlank(),
                        planning = planning,
                        onStop = { core.update(Event.Interrupt) },
                        onForceStop = { core.update(Event.ForceStop) },
                        onSteer = { finishSend(Event.Steer) },
                        onSend = { if (composer.canSend) finishSend(Event.Send) },
                    )
                }
            }
        }
    }
    options?.let { ComposerOptionsSheet(core, it) { options = null } }
    if (stashList) {
        LaunchedEffect(composer.stash.isEmpty()) { if (composer.stash.isEmpty()) stashList = false }
        StashSheet(composer.stash, restore = { stashList = false; core.update(Event.RestoreStash(it.toUInt())) }, drop = { core.update(Event.DropStash(it.toUInt())) }) { stashList = false }
    }
    if (limitsList) LimitsSheet(composer.limits) { limitsList = false }
}

/** A command or skill in the suggestions: its name, what it takes, and a badge for the app's own. */
@Composable
private fun CommandLabel(prefix: String, command: SlashCommandView) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.xs)) {
        Text("$prefix${command.name}", fontFamily = Mono)
        if (command.hint.isNotEmpty()) Text(command.hint, fontFamily = Mono, color = MaterialTheme.colorScheme.onSurfaceVariant, style = MaterialTheme.typography.labelSmall)
        if (command.kind == "builtin") {
            Surface(color = MaterialTheme.colorScheme.surfaceContainerHighest, shape = MaterialTheme.shapes.extraSmall) {
                Text("app", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(horizontal = Space.xs))
            }
        }
    }
}

/** Drafts put aside: restore one into the composer, or delete it. */
@Composable
private fun StashSheet(stash: List<String>, restore: (Int) -> Unit, drop: (Int) -> Unit, onDismiss: () -> Unit) {
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(Modifier.fillMaxWidth().padding(bottom = Space.xl)) {
            Text("Stashed drafts", style = MaterialTheme.typography.titleMedium, modifier = Modifier.padding(horizontal = Space.xl, vertical = Space.s))
            stash.forEachIndexed { index, line ->
                ListItem(
                    headlineContent = { Text(line, maxLines = 2, overflow = TextOverflow.Ellipsis) },
                    trailingContent = {
                        Row {
                            TextButton(onClick = { restore(index) }) { Text("Restore") }
                            IconButton(onClick = { drop(index) }) { Icon(Icons.Outlined.Delete, "Delete stashed draft") }
                        }
                    },
                    colors = ListItemDefaults.colors(containerColor = Color.Transparent),
                )
            }
        }
    }
}

/** The provider account's usage windows (from the limits chip). */
@Composable
private fun LimitsSheet(limits: List<LimitWindowView>, onDismiss: () -> Unit) {
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(Modifier.fillMaxWidth().padding(horizontal = Space.xl).padding(bottom = Space.xl), verticalArrangement = Arrangement.spacedBy(Space.l)) {
            Text("Usage limits", style = MaterialTheme.typography.titleMedium)
            if (limits.isEmpty()) Text("No limits reported.", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            limits.forEach { LimitWindowRow(it) }
        }
    }
}

/** The sent text again, give or take the keyboard's last correction. */
private fun sameMessage(a: String, b: String): Boolean {
    val common = a.zip(b).takeWhile { it.first == it.second }.size
    return kotlin.math.abs(a.length - b.length) <= 4 && common >= minOf(a.length, b.length) - 8
}

@Composable
private fun ChipRow(content: @Composable () -> Unit) {
    Row(Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = Space.xs), horizontalArrangement = Arrangement.spacedBy(Space.s)) { content() }
}

/** When the provider can't run, or why a message waits. */
@Composable
private fun ProviderNotice(composer: ComposerView, checkAgain: () -> Unit) {
    val tints = LocalTints.current
    if (composer.blockedReason.isNotEmpty()) {
        Surface(color = tints.caution.copy(alpha = 0.12f), shape = MaterialTheme.shapes.small, modifier = Modifier.fillMaxWidth()) {
            Row(Modifier.padding(start = Space.m), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                Icon(Icons.Outlined.Warning, null, tint = tints.caution, modifier = Modifier.size(16.dp))
                Text(composer.blockedReason, style = MaterialTheme.typography.bodySmall, modifier = Modifier.weight(1f))
                TextButton(onClick = checkAgain) { Text("Check again") }
            }
        }
    } else if (composer.sendHint.isNotEmpty()) {
        Text(composer.sendHint, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(horizontal = Space.s))
    }
}

/** Plan toggles in place; effort, permissions and model open the options sheet. */
@Composable
private fun ModeChips(composer: ComposerView, open: (ComposerOption) -> Unit, showLimits: () -> Unit, togglePlan: () -> Unit) {
    val tints = LocalTints.current
    val planning = composer.interactionMode == "plan"
    Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(Space.xs)) {
        if (composer.showsInteractionMode || planning) {
            FilterChip(
                selected = planning, onClick = togglePlan,
                label = { Text("Plan") },
                leadingIcon = { Icon(Icons.Outlined.Checklist, null, modifier = Modifier.size(16.dp)) },
                colors = FilterChipDefaults.filterChipColors(selectedContainerColor = tints.planContainer, selectedLabelColor = tints.plan, selectedLeadingIconColor = tints.plan),
            )
        }
        if (composer.efforts.isNotEmpty()) {
            val label = composer.efforts.firstOrNull { it.value == composer.selectedEffort }?.label
            AssistChip(onClick = { open(ComposerOption.Effort) }, label = { Text(label ?: "Effort") }, leadingIcon = { Icon(Icons.Outlined.Speed, null, modifier = Modifier.size(16.dp)) })
        }
        val mode = composer.runtimeMode
        val modeLabel = composer.runtimeModes.firstOrNull { it.value == mode }?.label ?: mode
        FilterChip(
            selected = mode == "full-access", onClick = { open(ComposerOption.Permissions) },
            label = { Text(ModeInfo.permissionShort(mode, modeLabel)) },
            leadingIcon = { Icon(ModeInfo.permissionIcon(mode), null, modifier = Modifier.size(16.dp)) },
            colors = FilterChipDefaults.filterChipColors(selectedContainerColor = tints.caution.copy(alpha = 0.18f), selectedLabelColor = tints.caution, selectedLeadingIconColor = tints.caution),
        )
        AssistChip(
            onClick = { open(ComposerOption.Model) },
            label = { Text(composer.selectedModel?.label ?: "Model", maxLines = 1, overflow = TextOverflow.Ellipsis) },
            leadingIcon = { Icon(Icons.Outlined.Memory, null, modifier = Modifier.size(16.dp)) },
        )
        // An account window nearly used up.
        if (composer.limitsChip.isNotEmpty()) {
            AssistChip(
                onClick = showLimits,
                label = { Text(composer.limitsChip, maxLines = 1) },
                leadingIcon = { Icon(Icons.Outlined.Warning, null, tint = tints.caution, modifier = Modifier.size(16.dp)) },
                colors = AssistChipDefaults.assistChipColors(containerColor = tints.caution.copy(alpha = 0.12f), labelColor = tints.caution),
                border = null,
            )
        }
    }
}

/** Send, a spinner while it goes out, or Stop while a turn runs; typed during a turn, Steer and Send (queues) beside a small Stop. */
@Composable
private fun ActionButtons(composer: ComposerView, typed: Boolean, planning: Boolean, onStop: () -> Unit, onForceStop: () -> Unit, onSteer: () -> Unit, onSend: () -> Unit) {
    val tints = LocalTints.current
    Row(horizontalArrangement = Arrangement.spacedBy(Space.xs), verticalAlignment = Alignment.CenterVertically) {
        // Stop was asked and the turn goes on: end it for good.
        if (composer.canForceStop) {
            TextButton(onClick = onForceStop, colors = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.error)) { Text("Force stop", maxLines = 1) }
        }
        if (composer.canStop) {
            FilledIconButton(
                onClick = onStop,
                colors = IconButtonDefaults.filledIconButtonColors(containerColor = MaterialTheme.colorScheme.error, contentColor = MaterialTheme.colorScheme.onError),
                modifier = Modifier.size(if (typed) 32.dp else 40.dp),
            ) { Icon(Icons.Filled.Stop, "Stop", modifier = Modifier.size(if (typed) 16.dp else 20.dp)) }
        }
        if (composer.canSteer && typed) {
            FilledTonalIconButton(onClick = onSteer, modifier = Modifier.size(40.dp)) {
                Icon(Icons.Outlined.SubdirectoryArrowRight, "Steer: send into this turn")
            }
        }
        if (!composer.canStop || typed) {
            FilledIconButton(
                onClick = onSend,
                enabled = composer.canSend,
                colors = if (planning) IconButtonDefaults.filledIconButtonColors(containerColor = tints.plan, contentColor = tints.onPlan) else IconButtonDefaults.filledIconButtonColors(),
                modifier = Modifier.size(40.dp),
            ) {
                if (composer.sending) CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp, color = MaterialTheme.colorScheme.onPrimary)
                else Icon(Icons.Filled.ArrowUpward, if (composer.canStop || composer.queuesOffline) "Queue message" else "Send")
            }
        }
    }
}
