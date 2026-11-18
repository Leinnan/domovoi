package com.crayen.domovoi.ui.thread

import androidx.compose.foundation.clickable
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.filled.Star
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.StarOutline
import androidx.compose.material.icons.outlined.Visibility
import androidx.compose.material.icons.outlined.VisibilityOff
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.IconButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.outlined.Checklist
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.RadioButton
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.core.Core
import com.crayen.domovoi.ui.common.watch
import com.crayen.domovoi.ui.theme.LocalTints
import com.crayen.domovoi.ui.theme.Space

/** Sections of the options; a chip opens its own. */
enum class ComposerOption { Model, Effort, Permissions, Plan }

/** Model, effort, permissions and plan mode for the next message (`ComposerOptionsSheet`). */
@Composable
fun ComposerOptionsSheet(core: Core, focus: ComposerOption, onDismiss: () -> Unit) {
    val composer by core.view.composer.watch()
    val tints = LocalTints.current
    var confirmFull by remember { mutableStateOf(false) }
    val list = rememberLazyListState()
    val sheet = rememberModalBottomSheetState(skipPartiallyExpanded = focus == ComposerOption.Model)
    LaunchedEffect(Unit) {
        core.update(Event.SetModelQuery(""))
        core.update(Event.SetShowHiddenModels(false))
    }
    LaunchedEffect(focus) {
        val index = when (focus) {
            ComposerOption.Plan -> 0
            ComposerOption.Effort -> 1
            ComposerOption.Permissions -> 2
            ComposerOption.Model -> 3
        }
        list.scrollToItem(index)
    }
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = sheet) {
        LazyColumn(state = list, modifier = Modifier.fillMaxWidth()) {
            item("plan") {
                if (composer.showsInteractionMode || composer.interactionMode == "plan") {
                    ListItem(
                        headlineContent = { Text("Plan first") },
                        supportingContent = { Text("The agent proposes a plan and waits before changing anything.") },
                        leadingContent = { Icon(Icons.Outlined.Checklist, null, tint = tints.plan) },
                        trailingContent = {
                            Switch(checked = composer.interactionMode == "plan", onCheckedChange = { core.update(Event.SetInteractionMode(if (it) "plan" else "default")) })
                        },
                        colors = ListItemDefaults.colors(containerColor = Color.Transparent),
                    )
                }
            }
            item("effort") {
                if (composer.efforts.isNotEmpty()) {
                    Column(Modifier.padding(horizontal = Space.l, vertical = Space.s), verticalArrangement = Arrangement.spacedBy(Space.s)) {
                        Header("Reasoning effort")
                        androidx.compose.foundation.layout.FlowRow(horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                            composer.efforts.forEach { effort ->
                                androidx.compose.material3.FilterChip(
                                    selected = effort.value == composer.selectedEffort,
                                    onClick = { core.update(Event.SelectEffort(effort.value)) },
                                    label = { Text(effort.label) },
                                )
                            }
                        }
                    }
                }
            }
            item("permissions") {
                Column(Modifier.padding(vertical = Space.s)) {
                    Header("Permissions", Modifier.padding(horizontal = Space.l))
                    composer.runtimeModes.forEach { mode ->
                        val selected = mode.value == composer.runtimeMode
                        ListItem(
                            headlineContent = { Text(mode.label, color = if (mode.value == "full-access") tints.caution else Color.Unspecified) },
                            supportingContent = { ModeInfo.permissionDetail(mode.value).takeIf { it.isNotEmpty() }?.let { Text(it) } },
                            leadingContent = { Icon(ModeInfo.permissionIcon(mode.value), null) },
                            trailingContent = { RadioButton(selected = selected, onClick = null) },
                            colors = ListItemDefaults.colors(containerColor = Color.Transparent),
                            modifier = Modifier.clickable {
                                if (mode.value == "full-access" && !selected) confirmFull = true else core.update(Event.SetRuntimeMode(mode.value))
                            },
                        )
                    }
                }
            }
            item("model-h") { Header("Model", Modifier.padding(horizontal = Space.l, vertical = Space.s)) }
            if (composer.modelCount > 8u || composer.modelQuery.isNotEmpty()) {
                item("model-search") { ModelSearchField(composer.modelQuery) { core.update(Event.SetModelQuery(it)) } }
            }
            modelItems(composer, core::update) { instance, model ->
                core.update(Event.SelectModel(instance, model))
                onDismiss()
            }
            item("end") { Spacer(Modifier.height(Space.xl)) }
        }
    }
    if (confirmFull) {
        AlertDialog(
            onDismissRequest = { confirmFull = false },
            title = { Text(ModeInfo.FULL_ACCESS_TITLE) },
            text = { Text(ModeInfo.FULL_ACCESS_MESSAGE) },
            confirmButton = { TextButton(onClick = { confirmFull = false; core.update(Event.SetRuntimeMode("full-access")) }) { Text("Give full access", color = tints.caution) } },
            dismissButton = { TextButton(onClick = { confirmFull = false }) { Text("Cancel") } },
        )
    }
}

/** Favorites first, then the other models by provider; "Show N hidden" under a provider with hidden models. */
private fun LazyListScope.modelItems(
    composer: com.crayen.domovoi.app.ComposerView,
    update: (Event) -> Unit,
    pick: (String, String) -> Unit,
) {
    val selected = composer.selectedModel?.let { it.instanceId to it.model }
    val searching = composer.modelQuery.isNotEmpty()
    if (composer.favoriteModels.isNotEmpty()) {
        item("g/favorites") { GroupHeader("Favorites", "ready") }
        for (row in composer.favoriteModels) {
            item("f/${row.option.instanceId}/${row.option.model}") { ModelRow(row, selected, update, pick) }
        }
    }
    for (group in composer.modelGroups) {
        item("g/${group.instanceId}") { GroupHeader(group.provider, group.status) }
        for (row in group.models) {
            item("m/${row.option.instanceId}/${row.option.model}") { ModelRow(row, selected, update, pick) }
        }
        if (group.hiddenCount > 0u && !searching) {
            item("h/${group.instanceId}") {
                TextButton(onClick = { update(Event.SetShowHiddenModels(!composer.showHiddenModels)) }, modifier = Modifier.padding(horizontal = Space.s)) {
                    Text(if (composer.showHiddenModels) "Hide hidden models" else "Show ${group.hiddenCount} hidden")
                }
            }
        }
    }
    if (searching && composer.favoriteModels.isEmpty() && composer.modelGroups.isEmpty()) {
        item("model-none") {
            Text("No models match \u201c${composer.modelQuery}\u201d", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = Space.l, vertical = Space.m))
        }
    }
}

@Composable
private fun GroupHeader(title: String, status: String) {
    Row(Modifier.fillMaxWidth().padding(horizontal = Space.l, vertical = Space.xs), verticalAlignment = Alignment.CenterVertically) {
        Text(title, style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.weight(1f))
        if (status.isNotEmpty() && status != "ready") Text(status, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.error)
    }
}

/** A model: a tap picks it, the star marks it favorite, a long press also hides it. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun ModelRow(
    row: com.crayen.domovoi.app.ModelRowView,
    selected: Pair<String, String>?,
    update: (Event) -> Unit,
    pick: (String, String) -> Unit,
) {
    val on = selected == (row.option.instanceId to row.option.model)
    val haptics = LocalHapticFeedback.current
    var menu by remember { mutableStateOf(false) }
    val favorite = { value: Boolean -> update(Event.SetModelFavorite(row.option.instanceId, row.option.model, value)) }
    val hide = { value: Boolean -> update(Event.SetModelHidden(row.option.instanceId, row.option.model, value)) }
    Box {
        ListItem(
            headlineContent = { Text(row.name, fontWeight = if (on) FontWeight.SemiBold else FontWeight.Normal) },
            supportingContent = if (row.detail.isNotEmpty()) ({ Text(row.detail) }) else null,
            trailingContent = {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    if (on) Icon(Icons.Filled.Check, "Selected", tint = MaterialTheme.colorScheme.primary, modifier = Modifier.size(20.dp()))
                    IconButton(onClick = { favorite(!row.favorite) }) {
                        if (row.favorite) {
                            Icon(Icons.Filled.Star, "Remove from favorites", tint = LocalTints.current.caution)
                        } else {
                            Icon(Icons.Outlined.StarOutline, "Add to favorites", tint = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                    }
                }
            },
            colors = ListItemDefaults.colors(containerColor = Color.Transparent),
            modifier = Modifier
                .alpha(if (row.hidden) 0.5f else 1f)
                .combinedClickable(
                    onClick = { pick(row.option.instanceId, row.option.model) },
                    onLongClick = { haptics.performHapticFeedback(HapticFeedbackType.LongPress); menu = true },
                    onLongClickLabel = "More actions",
                ),
        )
        DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
            DropdownMenuItem(
                text = { Text(if (row.favorite) "Remove from favorites" else "Add to favorites") },
                leadingIcon = { Icon(if (row.favorite) Icons.Outlined.StarOutline else Icons.Filled.Star, null) },
                onClick = { menu = false; favorite(!row.favorite) },
            )
            DropdownMenuItem(
                text = { Text(if (row.hidden) "Show model" else "Hide model") },
                leadingIcon = { Icon(if (row.hidden) Icons.Outlined.Visibility else Icons.Outlined.VisibilityOff, null) },
                onClick = { menu = false; hide(!row.hidden) },
            )
        }
    }
}

/** The model search; kept here so typing doesn't wait for the core. */
@Composable
private fun ModelSearchField(query: String, onChange: (String) -> Unit) {
    var text by remember { mutableStateOf(query) }
    LaunchedEffect(query) { if (query.isEmpty()) text = "" }
    val set = { value: String -> text = value; onChange(value) }
    OutlinedTextField(
        value = text,
        onValueChange = set,
        placeholder = { Text("Search models") },
        leadingIcon = { Icon(Icons.Outlined.Search, null) },
        trailingIcon = { if (text.isNotEmpty()) IconButton(onClick = { set("") }) { Icon(Icons.Outlined.Close, "Clear") } },
        singleLine = true,
        keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
        modifier = Modifier.fillMaxWidth().padding(horizontal = Space.l, vertical = Space.xs),
    )
}

private fun Int.dp() = androidx.compose.ui.unit.Dp(this.toFloat())

@Composable
private fun Header(text: String, modifier: Modifier = Modifier) {
    Text(text, style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.primary, modifier = modifier)
}
