package com.crayen.domovoi.ui.nav

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.Bolt
import androidx.compose.material.icons.outlined.ChatBubbleOutline
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.Folder
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material.icons.outlined.FindInPage
import androidx.compose.material3.AssistChip
import androidx.compose.material3.CircularProgressIndicator
import com.crayen.domovoi.ui.theme.Mono
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextField
import androidx.compose.material3.TextFieldDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.app.PaletteItemView
import com.crayen.domovoi.app.PaletteView
import com.crayen.domovoi.ui.common.EmptyState
import com.crayen.domovoi.ui.common.SectionHeader
import com.crayen.domovoi.ui.theme.Space

/** A palette row: a section title, or an item with its place in the core's list. */
private sealed interface PaletteRow {
    data class Header(val kind: String) : PaletteRow
    data class Item(val index: Int, val item: PaletteItemView) : PaletteRow
}

private fun sectionTitle(kind: String) = when (kind) {
    "action" -> "Actions"
    "thread" -> "Threads"
    "project" -> "Projects"
    "file" -> "Files"
    "match" -> "Matches"
    else -> kind.replaceFirstChar { it.uppercase() }
}

private fun kindIcon(kind: String): ImageVector = when (kind) {
    "thread" -> Icons.Outlined.ChatBubbleOutline
    "project" -> Icons.Outlined.Folder
    "file" -> Icons.Outlined.Description
    "match" -> Icons.Outlined.FindInPage
    else -> Icons.Outlined.Bolt
}

/** The core's shortcut as this app has it: only the ones Android binds. */
private fun androidShortcut(shortcut: String): String? = when (shortcut) {
    "⌘N" -> "Ctrl+N"
    "⌘F" -> "Ctrl+F"
    else -> null
}

/**
 * The command palette (Ctrl+K or the search button): actions, threads and
 * projects, filtered by the core as you type. Full screen on a phone, a card
 * on wider windows. Arrows move, Enter runs, Escape (or back) closes.
 */
@Composable
fun PaletteDialog(palette: PaletteView, expanded: Boolean, send: (Event) -> Unit, run: (String) -> Unit) {
    // Seeded again when the mode changes (files and content searches start empty).
    var query by remember(palette.mode) { mutableStateOf(TextFieldValue(palette.query, TextRange(palette.query.length))) }
    val searchingFiles = palette.mode != "commands" && palette.mode.isNotEmpty()
    val focus = remember { FocusRequester() }
    val keyboard = LocalSoftwareKeyboardController.current
    // Consecutive items of a kind under one header (the core orders actions, threads, projects).
    val rows = remember(palette.items) {
        buildList {
            var kind: String? = null
            palette.items.forEachIndexed { i, item ->
                if (item.kind != kind) { kind = item.kind; add(PaletteRow.Header(item.kind)) }
                add(PaletteRow.Item(i, item))
            }
        }
    }
    val highlighted = palette.highlighted.toInt()
    val highlightedRow = rows.indexOfFirst { it is PaletteRow.Item && it.index == highlighted }
    val list = rememberLazyListState()
    fun runHighlighted() { palette.items.getOrNull(highlighted)?.let { run(it.id) } }

    LaunchedEffect(Unit) {
        focus.requestFocus()
        keyboard?.show()
    }
    // Keep the keyboard's choice on screen (with its header when it is first).
    LaunchedEffect(highlightedRow) {
        if (highlightedRow < 0) return@LaunchedEffect
        val visible = list.layoutInfo.visibleItemsInfo
        if (visible.isEmpty()) return@LaunchedEffect
        val first = visible.first().index
        val last = visible.last().index
        when {
            highlightedRow <= first -> list.animateScrollToItem(if (highlightedRow == 1) 0 else highlightedRow)
            highlightedRow >= last -> list.animateScrollToItem(highlightedRow - (last - first) + 1)
        }
    }

    Dialog(onDismissRequest = { send(Event.ClosePalette) }, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Surface(
            color = MaterialTheme.colorScheme.surfaceContainer,
            shape = if (expanded) MaterialTheme.shapes.extraLarge else MaterialTheme.shapes.extraSmall,
            tonalElevation = 6.dp,
            modifier = if (expanded) Modifier.padding(Space.xl).widthIn(max = 640.dp).fillMaxWidth().heightIn(max = 520.dp) else Modifier.fillMaxSize(),
        ) {
            Column {
                TextField(
                    value = query,
                    onValueChange = {
                        val changed = it.text != query.text
                        query = it
                        if (changed) send(Event.SetPaletteQuery(it.text))
                    },
                    placeholder = { Text(palette.placeholder.ifEmpty { "Search actions, threads and projects" }) },
                    leadingIcon = {
                        if (expanded) Icon(Icons.Outlined.Search, null)
                        else IconButton(onClick = { send(Event.ClosePalette) }) { Icon(Icons.AutoMirrored.Outlined.ArrowBack, "Close") }
                    },
                    trailingIcon = {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            if (palette.searching) CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                            if (query.text.isNotEmpty()) IconButton(onClick = { query = TextFieldValue(""); send(Event.SetPaletteQuery("")) }) { Icon(Icons.Outlined.Close, "Clear") }
                        }
                    },
                    singleLine = true,
                    shape = CircleShape,
                    keyboardOptions = KeyboardOptions(imeAction = ImeAction.Go),
                    keyboardActions = KeyboardActions(onGo = { runHighlighted() }),
                    colors = TextFieldDefaults.colors(
                        focusedIndicatorColor = Color.Transparent,
                        unfocusedIndicatorColor = Color.Transparent,
                        unfocusedContainerColor = MaterialTheme.colorScheme.surfaceContainerHigh,
                        focusedContainerColor = MaterialTheme.colorScheme.surfaceContainerHigh,
                    ),
                    modifier = Modifier.fillMaxWidth().padding(horizontal = Space.l, vertical = Space.m).focusRequester(focus)
                        .onPreviewKeyEvent { e ->
                            if (e.type != KeyEventType.KeyDown) return@onPreviewKeyEvent false
                            when (e.key) {
                                Key.DirectionUp -> { send(Event.MovePalette(-1)); true }
                                Key.DirectionDown -> { send(Event.MovePalette(1)); true }
                                Key.Enter, Key.NumPadEnter -> { runHighlighted(); true }
                                // In a file search, Escape goes back to the commands first.
                                Key.Escape -> { send(if (searchingFiles) Event.SetPaletteMode("commands") else Event.ClosePalette); true }
                                else -> false
                            }
                        },
                )
                if (searchingFiles) {
                    AssistChip(
                        onClick = { send(Event.SetPaletteMode("commands")) },
                        label = { Text("All commands") },
                        leadingIcon = { Icon(Icons.AutoMirrored.Outlined.ArrowBack, null, modifier = Modifier.size(16.dp)) },
                        modifier = Modifier.padding(start = Space.l, bottom = Space.xs),
                    )
                }
                if (rows.isEmpty()) {
                    Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.TopCenter) {
                        when {
                            palette.searching -> Text("Searching…", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(Space.xl))
                            searchingFiles && query.text.isEmpty() -> Text(palette.placeholder.ifEmpty { "Type to search" }, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(Space.xl))
                            else -> EmptyState(Icons.Outlined.Search, "No matches")
                        }
                    }
                } else {
                    LazyColumn(state = list, modifier = Modifier.weight(1f, fill = !expanded).fillMaxWidth()) {
                        itemsIndexed(rows, key = { _, row -> if (row is PaletteRow.Item) "${row.index}/${row.item.id}" else "header:${(row as PaletteRow.Header).kind}" }) { _, row ->
                            when (row) {
                                is PaletteRow.Header -> SectionHeader(sectionTitle(row.kind))
                                is PaletteRow.Item -> PaletteItemRow(row.item, row.index == highlighted) { run(row.item.id) }
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun PaletteItemRow(item: PaletteItemView, highlighted: Boolean, onClick: () -> Unit) {
    val content = if (highlighted) MaterialTheme.colorScheme.onSecondaryContainer else MaterialTheme.colorScheme.onSurface
    Row(
        Modifier.fillMaxWidth().padding(horizontal = Space.s)
            .clip(MaterialTheme.shapes.medium)
            .background(if (highlighted) MaterialTheme.colorScheme.secondaryContainer else Color.Transparent)
            .clickable(onClick = onClick)
            .padding(horizontal = Space.m, vertical = Space.s),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        Icon(kindIcon(item.kind), null, tint = if (highlighted) content else MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.size(20.dp))
        Column(Modifier.weight(1f)) {
            Text(item.title, style = if (item.kind == "match") MaterialTheme.typography.bodyMedium else MaterialTheme.typography.bodyLarge, fontFamily = if (item.kind == "match") Mono else null, color = content, maxLines = 1, overflow = TextOverflow.Ellipsis)
            if (item.subtitle.isNotEmpty()) {
                Text(item.subtitle, style = MaterialTheme.typography.bodySmall, fontFamily = if (item.kind == "file" || item.kind == "match") Mono else null, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.StartEllipsis)
            }
        }
        androidShortcut(item.shortcut)?.let {
            Text(it, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}
