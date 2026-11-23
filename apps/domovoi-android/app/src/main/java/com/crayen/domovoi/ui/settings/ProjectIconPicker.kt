package com.crayen.domovoi.ui.settings

import android.icu.lang.UCharacter
import android.icu.lang.UProperty
import android.icu.text.BreakIterator
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.Image
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import coil3.compose.AsyncImage
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.app.ProjectIconFilesView
import com.crayen.domovoi.app.ProjectSettingsView
import com.crayen.domovoi.core.Core
import com.crayen.domovoi.ui.common.ProjectIconTile
import com.crayen.domovoi.ui.common.projectColor
import com.crayen.domovoi.ui.common.projectColors
import com.crayen.domovoi.ui.common.projectSymbols
import com.crayen.domovoi.ui.theme.Space
import kotlinx.coroutines.delay

private val choices = linkedMapOf("" to "Automatic", "monogram" to "Letters", "emoji" to "Emoji", "symbol" to "Symbol", "file" to "From Project Files")
private val emojis = listOf("💻", "🛠️", "🚀", "🤖", "✨", "⚡", "🌐", "📱", "🖥️", "⌨️", "⚙️", "🗄️", "☁️", "📦", "📚", "🧪", "🔒", "🎮", "🎵", "🎬", "🖼️", "🛍️", "🔥", "💡", "🧩", "📊", "🧠", "🦄", "🐙", "🌱")

private fun graphemeCount(value: String): Int {
    val iterator = BreakIterator.getCharacterInstance()
    iterator.setText(value)
    var count = 0
    while (iterator.next() != BreakIterator.DONE) count++
    return count
}

@Composable
internal fun ProjectIconPicker(
    project: ProjectSettingsView, files: ProjectIconFilesView, core: Core,
    connected: Boolean, onDismiss: () -> Unit,
) {
    val icon = project.icon
    val automatic = project.automaticIcon
    var kind by remember(project.id) { mutableStateOf(if (icon.filePath != null) "file" else if (icon.custom) icon.kind else "") }
    var letters by remember(project.id) { mutableStateOf(if (icon.custom && icon.kind == "monogram" && icon.filePath == null) icon.text else automatic.text) }
    var emoji by remember(project.id) { mutableStateOf(icon.text.takeIf { icon.kind == "emoji" } ?: emojis.first()) }
    var symbol by remember(project.id) { mutableStateOf(icon.symbol.ifEmpty { "folder-code" }) }
    var color by remember(project.id) { mutableStateOf(icon.color) }
    var path by remember(project.id) { mutableStateOf(icon.filePath) }
    var image by remember(project.id) { mutableStateOf(icon.imageUrl) }
    var query by remember(project.id) { mutableStateOf("") }
    var menu by remember { mutableStateOf(false) }
    val text = if (kind == "emoji") emoji.trim() else letters.trim()
    val valid = when (kind) {
        "monogram" -> text.isNotEmpty() && Character.isLetterOrDigit(text.codePointAt(0)) && graphemeCount(text) <= 2 && text.length <= 32 &&
            text.codePoints().allMatch { cp ->
                Character.isLetterOrDigit(cp) || cp in 0x200c..0x200d || Character.getType(cp) in
                    setOf(Character.NON_SPACING_MARK.toInt(), Character.COMBINING_SPACING_MARK.toInt(), Character.ENCLOSING_MARK.toInt())
            }
        "emoji" -> graphemeCount(text) == 1 && text.length <= 32 &&
            text.codePoints().anyMatch { it == 0x20e3 || (it > 127 && UCharacter.hasBinaryProperty(it, UProperty.EMOJI)) }
        "file" -> path != null && image != null
        else -> true
    }
    val preview = when (kind) {
        "" -> automatic
        "file" -> automatic.copy(imageUrl = if (path == icon.filePath) icon.imageUrl else image, loadKey = null)
        else -> automatic.copy(kind = kind, text = text, symbol = symbol, color = color, imageUrl = null, loadKey = null)
    }
    LaunchedEffect(project.id, kind, query, connected) {
        if (kind == "file") {
            if (query.isNotEmpty()) delay(200)
            core.update(Event.SearchProjectIconFiles(project.id, query))
        }
    }
    DisposableEffect(project.id) {
        onDispose { core.update(Event.CloseProjectIconFiles(project.id)) }
    }

    Dialog(onDismissRequest = onDismiss, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Surface(modifier = Modifier.padding(horizontal = Space.l).widthIn(max = 480.dp).fillMaxWidth(), shape = MaterialTheme.shapes.extraLarge) {
            Column(Modifier.padding(Space.l), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(Space.s)) {
                Text("Icon for ${project.title}", style = MaterialTheme.typography.titleLarge, maxLines = 2, overflow = TextOverflow.Ellipsis)
                ProjectIconTile(preview, core, size = 52.dp)
                Box {
                    OutlinedButton(onClick = { menu = true }) { Text(choices.getValue(kind)) }
                    DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                        choices.forEach { (key, label) ->
                            DropdownMenuItem(text = { Text(label) }, onClick = { kind = key; menu = false })
                        }
                    }
                }
                if (kind == "file") {
                    OutlinedTextField(query, { query = it }, label = { Text("Search project images") }, singleLine = true, modifier = Modifier.fillMaxWidth())
                    path?.let { Text(it, maxLines = 1, overflow = TextOverflow.StartEllipsis, style = MaterialTheme.typography.bodySmall) }
                }
                LazyColumn(Modifier.fillMaxWidth().heightIn(max = 280.dp).weight(1f, fill = false), verticalArrangement = Arrangement.spacedBy(Space.xs)) {
                    when (kind) {
                        "" -> item { Text("Uses a project icon file when found, or initials from the project name.", style = MaterialTheme.typography.bodyMedium) }
                        "monogram" -> item {
                            OutlinedTextField(letters, { letters = it }, label = { Text("One or two letters or digits") }, singleLine = true, modifier = Modifier.fillMaxWidth())
                        }
                        "emoji" -> {
                            item { OutlinedTextField(emoji, { emoji = it }, label = { Text("One emoji") }, singleLine = true, modifier = Modifier.fillMaxWidth()) }
                            items(emojis.chunked(5)) { row ->
                                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                                    row.forEach { char -> FilterChip(selected = emoji == char, onClick = { emoji = char }, label = { Text(char) }) }
                                }
                            }
                        }
                        "symbol" -> items(projectSymbols.keys.toList()) { name ->
                            FilterChip(
                                selected = symbol == name, onClick = { symbol = name }, label = { Text(name.replace('-', ' ')) },
                                leadingIcon = { Icon(projectSymbols.getValue(name), null, Modifier.size(18.dp)) },
                            )
                        }
                        "file" -> when {
                            files.projectId != project.id || files.loading -> item { CircularProgressIndicator(Modifier.size(24.dp)) }
                            files.error != null -> item { Text(files.error.orEmpty(), color = MaterialTheme.colorScheme.error) }
                            files.files.isEmpty() -> item { Text(if (query.isEmpty()) "No project images found." else "No matching project images.") }
                            else -> {
                                items(files.files, key = { it.path }) { file ->
                                    LaunchedEffect(files.revision, file.refresh) {
                                        if (file.refresh) core.update(Event.LoadProjectIconThumbnail(project.id, file.path))
                                    }
                                    TextButton(
                                        onClick = { path = file.path; image = file.url }, enabled = file.url != null,
                                        modifier = Modifier.fillMaxWidth(),
                                    ) {
                                        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(Space.s), verticalAlignment = Alignment.CenterVertically) {
                                            if (file.url != null) AsyncImage(file.url, null, Modifier.size(32.dp), contentScale = ContentScale.Fit)
                                            else Icon(Icons.Outlined.Image, null, Modifier.size(32.dp))
                                            Column(Modifier.weight(1f)) {
                                                Text(file.path.substringAfterLast('/'))
                                                Text(file.path, maxLines = 1, overflow = TextOverflow.StartEllipsis, style = MaterialTheme.typography.bodySmall)
                                                if (file.unavailable) Text("Preview unavailable", style = MaterialTheme.typography.bodySmall)
                                            }
                                            if (path == file.path) Icon(Icons.Outlined.Check, "Selected")
                                        }
                                    }
                                }
                                if (files.truncated) item { Text("Search to narrow the results.") }
                            }
                        }
                    }
                    if (kind == "monogram" || kind == "symbol") {
                        items(projectColors.chunked(3)) { palette ->
                            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(Space.xs)) {
                                palette.forEach { key ->
                                    FilterChip(
                                        selected = color == key, onClick = { color = key }, label = { Text(key) },
                                        leadingIcon = { Box(Modifier.size(12.dp).background(projectColor(key), CircleShape)) },
                                    )
                                }
                            }
                        }
                    }
                }
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                    TextButton(onClick = onDismiss) { Text("Cancel") }
                    TextButton(enabled = connected && valid && !icon.saving, onClick = {
                        if (kind == "file") {
                            path?.takeIf { it != icon.filePath }?.let { core.update(Event.SetProjectIconFile(project.id, it)) }
                        } else core.update(Event.SetProjectIcon(project.id, kind, text, symbol, color))
                        onDismiss()
                    }) { Text("Done") }
                }
            }
        }
    }
}
