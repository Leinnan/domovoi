package com.crayen.domovoi.ui.settings

import android.content.Intent
import android.graphics.Bitmap
import android.os.Build
import android.provider.Settings
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowForward
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.Build
import androidx.compose.material.icons.outlined.CalendarMonth
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.Computer
import androidx.compose.material.icons.outlined.ContentCopy
import androidx.compose.material.icons.outlined.CreateNewFolder
import androidx.compose.material.icons.outlined.Delete
import androidx.compose.material.icons.outlined.Download
import androidx.compose.material.icons.outlined.Folder
import androidx.compose.material.icons.outlined.Key
import androidx.compose.material.icons.outlined.Memory
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material.icons.outlined.Notifications
import androidx.compose.material.icons.outlined.PlayArrow
import androidx.compose.material.icons.outlined.Public
import androidx.compose.material.icons.outlined.Share
import androidx.compose.material.icons.outlined.Tune
import androidx.compose.material.icons.outlined.Unarchive
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExposedDropdownMenuAnchorType
import androidx.compose.material3.ExposedDropdownMenuBox
import androidx.compose.material3.ExposedDropdownMenuDefaults
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Slider
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.app.ScheduledTaskView
import com.crayen.domovoi.app.SettingFieldView
import com.crayen.domovoi.core.Core
import com.crayen.domovoi.ui.common.ProjectIconTile
import com.crayen.domovoi.ui.common.EmptyState
import com.crayen.domovoi.ui.common.SectionHeader
import com.crayen.domovoi.ui.common.copyText
import com.crayen.domovoi.ui.common.watch
import com.crayen.domovoi.ui.nav.Navigator
import com.crayen.domovoi.ui.nav.Screen
import com.crayen.domovoi.ui.threads.NameDialog
import com.crayen.domovoi.ui.theme.LocalTints
import com.crayen.domovoi.ui.theme.Mono
import com.crayen.domovoi.ui.theme.Space
import com.crayen.domovoi.ui.tools.ToolScaffold
import com.google.zxing.BarcodeFormat
import com.google.zxing.qrcode.QRCodeWriter

private val clear = Color.Transparent

@Composable
private fun Nav(label: String, icon: androidx.compose.ui.graphics.vector.ImageVector, supporting: String? = null, onClick: () -> Unit) {
    ListItem(
        headlineContent = { Text(label) },
        supportingContent = supporting?.let { { Text(it) } },
        leadingContent = { Icon(icon, null) },
        trailingContent = { Icon(Icons.AutoMirrored.Outlined.ArrowForward, null, modifier = Modifier.size(18.dp)) },
        colors = ListItemDefaults.colors(containerColor = clear),
        modifier = Modifier.clickable(onClick = onClick),
    )
}

/** Servers, this server's settings, projects, archived threads and notifications. */
@Composable
fun SettingsScreen(core: Core, nav: Navigator) {
    val connection by core.view.connection.watch()
    val settings by core.view.settings.watch()
    val context = LocalContext.current
    var renamingEnv by remember { mutableStateOf<com.crayen.domovoi.app.EnvironmentView?>(null) }
    var forgetting by remember { mutableStateOf<com.crayen.domovoi.app.EnvironmentView?>(null) }
    var pickingProject by remember(connection.activeEnvironmentId) { mutableStateOf<String?>(null) }
    var renamingProject by remember { mutableStateOf<com.crayen.domovoi.app.ProjectSettingsView?>(null) }
    var removing by remember { mutableStateOf<com.crayen.domovoi.app.ProjectSettingsView?>(null) }
    LaunchedEffect(connection.state) { if (connection.state == "connected") { core.update(Event.LoadRemoteAccess); core.update(Event.LoadSettings("archived")) } }
    ToolScaffold("Settings", onBack = null) { padding ->
        LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(top = padding.calculateTopPadding(), bottom = Space.xxl)) {
            item("server") {
                SectionHeader("Connected server")
                ListItem(
                    headlineContent = { Text(connection.environmentLabel.ifEmpty { "—" }) },
                    supportingContent = { Text(listOf(connection.state, connection.serverVersion.ifEmpty { null }, connection.platform.ifEmpty { null }).filterNotNull().joinToString(" · ")) },
                    leadingContent = { Icon(if (connection.remote) Icons.Outlined.Public else Icons.Outlined.Computer, null) },
                    colors = ListItemDefaults.colors(containerColor = clear),
                )
                Row(Modifier.padding(horizontal = Space.l), horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                    if (connection.state == "connected") OutlinedButton(onClick = { core.update(Event.Disconnect) }) { Text("Disconnect") }
                    else FilledTonalButton(onClick = { core.update(Event.Reconnect) }) { Text("Reconnect") }
                }
            }
            item("servers-h") { SectionHeader("Servers") }
            items(connection.environments, key = { "env/${it.id}" }) { env ->
                var menu by remember { mutableStateOf(false) }
                ListItem(
                    headlineContent = { Text(env.label) },
                    supportingContent = { Text(env.detail, maxLines = 1, overflow = TextOverflow.Ellipsis) },
                    leadingContent = { Icon(if (env.remote) Icons.Outlined.Public else Icons.Outlined.Computer, null) },
                    trailingContent = {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            if (env.active) Icon(Icons.Outlined.Check, "In use", tint = MaterialTheme.colorScheme.primary)
                            Box {
                                IconButton(onClick = { menu = true }) { Icon(Icons.Outlined.MoreVert, "Actions for ${env.label}") }
                                DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                                    DropdownMenuItem(text = { Text("Rename…") }, onClick = { menu = false; renamingEnv = env })
                                    DropdownMenuItem(text = { Text("Move up") }, onClick = { menu = false; core.update(Event.MoveEnvironment(env.id, -1)) })
                                    DropdownMenuItem(text = { Text("Move down") }, onClick = { menu = false; core.update(Event.MoveEnvironment(env.id, 1)) })
                                    DropdownMenuItem(text = { Text("Forget", color = MaterialTheme.colorScheme.error) }, onClick = { menu = false; forgetting = env })
                                }
                            }
                        }
                    },
                    colors = ListItemDefaults.colors(containerColor = clear),
                    modifier = Modifier.clickable { if (!env.active) core.update(Event.SelectEnvironment(env.id)) },
                )
            }
            item("add-server") { Nav("Add server", Icons.Outlined.Add) { nav.open(Screen.AddServer) } }
            item("this-h") { SectionHeader("This server") }
            item("this") {
                Column {
                    Nav("Server settings", Icons.Outlined.Tune) { nav.open(Screen.ServerSettings) }
                    Nav("Providers", Icons.Outlined.Memory) { nav.open(Screen.Providers) }
                    Nav("Scheduled tasks", Icons.Outlined.CalendarMonth) { nav.open(Screen.ScheduledTasks) }
                    Nav("Access & devices", Icons.Outlined.Key) { nav.open(Screen.Access) }
                    Nav("Maintenance", Icons.Outlined.Build) { nav.open(Screen.Maintenance) }
                }
            }
            item("projects-h") { SectionHeader("Projects") }
            items(settings.projects, key = { "project/${it.id}" }) { project ->
                var menu by remember { mutableStateOf(false) }
                ListItem(
                    headlineContent = { Text(project.title) },
                    supportingContent = { Text("${project.workspaceRoot} · ${project.threadCount} threads", maxLines = 1, overflow = TextOverflow.StartEllipsis) },
                    leadingContent = { ProjectIconTile(project.icon, core) },
                    trailingContent = {
                        Box {
                            IconButton(onClick = { menu = true }) { Icon(Icons.Outlined.MoreVert, "Actions for ${project.title}") }
                            DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                                DropdownMenuItem(text = { Text("Change Icon…") }, enabled = !project.icon.saving && connection.state == "connected", onClick = { menu = false; pickingProject = project.id })
                                DropdownMenuItem(text = { Text("Rename…") }, onClick = { menu = false; renamingProject = project })
                                DropdownMenuItem(text = { Text("Remove", color = MaterialTheme.colorScheme.error) }, onClick = { menu = false; removing = project })
                            }
                        }
                    },
                    colors = ListItemDefaults.colors(containerColor = clear),
                )
            }
            item("projects-add") {
                Column {
                    Nav("Import from Claude Code", Icons.Outlined.Download) { nav.open(Screen.ImportSessions) }
                    Nav("Add folder on server", Icons.Outlined.CreateNewFolder) { nav.open(Screen.BrowseFolders) }
                    Text("Removing a project keeps its folder. A removed project is not imported again on its own.", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(horizontal = Space.l))
                }
            }
            if (settings.archived.isNotEmpty()) {
                item("archived-h") { SectionHeader("Archived threads") }
                items(settings.archived, key = { "archived/${it.id}" }) { thread ->
                    ListItem(
                        headlineContent = { Text(thread.title, maxLines = 1, overflow = TextOverflow.Ellipsis) },
                        supportingContent = { Text(thread.age) },
                        trailingContent = { IconButton(onClick = { core.update(Event.ArchiveThread(thread.id, false)) }) { Icon(Icons.Outlined.Unarchive, "Unarchive") } },
                        colors = ListItemDefaults.colors(containerColor = clear),
                    )
                }
            }
            item("notifications") {
                SectionHeader("Notifications")
                ListItem(
                    headlineContent = { Text("Notifications while connected") },
                    supportingContent = { Text("Approvals and finished turns, while Domovoi is open or a turn runs in the background.") },
                    leadingContent = { Icon(Icons.Outlined.Notifications, null) },
                    trailingContent = { Icon(Icons.AutoMirrored.Outlined.ArrowForward, null, modifier = Modifier.size(18.dp)) },
                    colors = ListItemDefaults.colors(containerColor = clear),
                    modifier = Modifier.clickable {
                        context.startActivity(Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS).putExtra(Settings.EXTRA_APP_PACKAGE, context.packageName))
                    },
                )
            }
        }
    }
    settings.projects.firstOrNull { it.id == pickingProject }?.let { project ->
        ProjectIconPicker(project, settings.projectIconFiles, core, connection.state == "connected") { pickingProject = null }
    }
    renamingEnv?.let { env ->
        NameDialog("Rename server", "Name", "Rename", env.label, onDismiss = { renamingEnv = null }) { renamingEnv = null; core.update(Event.RenameEnvironment(env.id, it)) }
    }
    renamingProject?.let { p ->
        NameDialog("Rename project", "Name", "Rename", p.title, onDismiss = { renamingProject = null }) { renamingProject = null; core.update(Event.RenameProject(p.id, it)) }
    }
    forgetting?.let { env ->
        AlertDialog(
            onDismissRequest = { forgetting = null },
            title = { Text("Forget ${env.label}?") },
            text = { Text("Forgetting the server in use also signs this phone out there. Forgetting another one only removes it here.") },
            confirmButton = { TextButton(onClick = { forgetting = null; core.update(Event.ForgetEnvironment(env.id)) }) { Text("Forget", color = MaterialTheme.colorScheme.error) } },
            dismissButton = { TextButton(onClick = { forgetting = null }) { Text("Cancel") } },
        )
    }
    removing?.let { p ->
        AlertDialog(
            onDismissRequest = { removing = null },
            title = { Text("Remove ${p.title}?") },
            text = { Text("Its folder stays on the server. Its threads go with it.") },
            confirmButton = { TextButton(onClick = { removing = null; core.update(Event.RemoveProject(p.id)) }) { Text("Remove", color = MaterialTheme.colorScheme.error) } },
            dismissButton = { TextButton(onClick = { removing = null }) { Text("Cancel") } },
        )
    }
}

/** One server setting: a switch, a menu or a text field, written back as it changes. */
@Composable
private fun SettingField(core: Core, field: SettingFieldView) {
    when (field.kind) {
        "bool" -> ListItem(
            headlineContent = { Text(field.label) },
            supportingContent = field.help.takeIf { it.isNotEmpty() }?.let { { Text(it) } },
            trailingContent = { Switch(checked = field.boolValue, onCheckedChange = { core.update(Event.SetSettingBool(field.key, it)) }) },
            colors = ListItemDefaults.colors(containerColor = clear),
        )
        "choice" -> {
            var open by remember { mutableStateOf(false) }
            Column(Modifier.padding(horizontal = Space.l, vertical = Space.s)) {
                ExposedDropdownMenuBox(expanded = open, onExpandedChange = { open = it }) {
                    OutlinedTextField(
                        value = field.choices.firstOrNull { it.value == field.textValue }?.label ?: field.textValue,
                        onValueChange = {}, readOnly = true, label = { Text(field.label) },
                        trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(open) },
                        modifier = Modifier.fillMaxWidth().menuAnchor(ExposedDropdownMenuAnchorType.PrimaryNotEditable),
                    )
                    ExposedDropdownMenu(expanded = open, onDismissRequest = { open = false }) {
                        field.choices.forEach { c -> DropdownMenuItem(text = { Text(c.label) }, onClick = { open = false; core.update(Event.SetSettingText(field.key, c.value)) }) }
                    }
                }
                if (field.help.isNotEmpty()) Text(field.help, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(top = Space.xs))
            }
        }
        else -> {
            var text by remember(field.textValue) { mutableStateOf(field.textValue) }
            OutlinedTextField(
                value = text, onValueChange = { text = it }, label = { Text(field.label) },
                supportingText = field.help.takeIf { it.isNotEmpty() }?.let { { Text(it) } },
                singleLine = true,
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done, autoCorrectEnabled = false),
                keyboardActions = KeyboardActions(onDone = { core.update(Event.SetSettingText(field.key, text)) }),
                modifier = Modifier.fillMaxWidth().padding(horizontal = Space.l, vertical = Space.s),
            )
        }
    }
}

@Composable
fun ServerSettingsScreen(core: Core, onBack: () -> Unit) {
    val settings by core.view.settings.watch()
    LaunchedEffect(Unit) { core.update(Event.LoadSettings("general")) }
    ToolScaffold("Server settings", onBack) { padding ->
        LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(top = padding.calculateTopPadding(), bottom = Space.xxl)) {
            settings.groups.forEach { group ->
                item("g/${group.title}") { SectionHeader(group.title) }
                items(group.fields, key = { "f/${it.key}" }) { SettingField(core, it) }
            }
        }
    }
}

@Composable
fun ProvidersScreen(core: Core, onBack: () -> Unit) {
    val settings by core.view.settings.watch()
    val tints = LocalTints.current
    var editing by remember { mutableStateOf<ProviderEditing?>(null) }
    LaunchedEffect(Unit) { core.update(Event.LoadSettings("providers")) }
    editing?.let { ProviderEditorSheet(it, { e -> core.update(e) }) { editing = null } }
    ToolScaffold("Providers", onBack, actions = { TextButton(onClick = { core.update(Event.RefreshProviders) }) { Text("Check again") } }) { padding ->
        LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(top = padding.calculateTopPadding(), bottom = Space.xxl)) {
            if (settings.providers.isEmpty()) item("none") { EmptyState(Icons.Outlined.Memory, "No providers yet") }
            items(settings.providers, key = { it.instanceId }) { p ->
                val ok = p.status == "ready"
                ListItem(
                    headlineContent = { Text(p.name) },
                    overlineContent = { Text(p.driver) },
                    supportingContent = {
                        Text(listOf(p.version.ifEmpty { null }, p.auth.ifEmpty { null }, if (p.modelCount > 0u) "${p.modelCount} models" else null, p.limits.ifEmpty { null }, p.message.ifEmpty { null }).filterNotNull().joinToString(" · "))
                    },
                    trailingContent = { Text(p.status, color = if (ok) tints.success else tints.caution, style = MaterialTheme.typography.labelLarge) },
                    colors = ListItemDefaults.colors(containerColor = clear),
                )
                ProviderSetup(p, { core.update(it) }) { editing = it }
            }
            if (settings.canAddAntigravity) item("add") {
                TextButton(onClick = { editing = newAntigravity() }, modifier = Modifier.padding(horizontal = Space.l)) { Text("Add Antigravity account") }
            }
            item("fields") {
                settings.groups.forEach { group ->
                    SectionHeader(group.title)
                    group.fields.forEach { SettingField(core, it) }
                }
            }
        }
    }
}

@Composable
fun ScheduledTasksScreen(core: Core, onBack: () -> Unit, openThread: (String) -> Unit) {
    val settings by core.view.settings.watch()
    val context = androidx.compose.ui.platform.LocalContext.current
    var editing by remember { mutableStateOf<com.crayen.domovoi.app.ScheduledTaskEdit?>(null) }
    var deleting by remember { mutableStateOf<com.crayen.domovoi.app.ScheduledTaskView?>(null) }
    var rotating by remember { mutableStateOf<com.crayen.domovoi.app.ScheduledTaskView?>(null) }
    LaunchedEffect(Unit) { core.update(Event.LoadSettings("scheduled")) }
    // "No project" can hold tasks too; a new one starts in a real project when there is one.
    val projects = settings.projects
    val firstProject = (projects.firstOrNull { !it.isScratch } ?: projects.firstOrNull())?.id.orEmpty()
    ToolScaffold(
        "Scheduled tasks", onBack,
        floatingActionButton = {
            ExtendedFloatingActionButton(onClick = { editing = newScheduledTask(firstProject) }, icon = { Icon(Icons.Outlined.Add, null) }, text = { Text("New task") })
        },
    ) { padding ->
        LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(top = padding.calculateTopPadding(), bottom = 96.dp)) {
            if (settings.scheduledTimeZone.isNotEmpty()) item("tz") { Text("Times in ${settings.scheduledTimeZone}", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(Space.l)) }
            if (settings.scheduledTasks.isEmpty()) item("none") { EmptyState(Icons.Outlined.CalendarMonth, "No scheduled tasks", "A task sends its prompt on its own: at a time, every so often, or when its URL gets a request.") }
            items(settings.scheduledTasks, key = { it.id }) { task ->
                var menu by remember { mutableStateOf(false) }
                val webhook = task.scheduleKind == "webhook"
                Column(Modifier.clickable { editing = editedScheduledTask(task) }) {
                    ListItem(
                        headlineContent = { Text(task.name) },
                        supportingContent = {
                            Column {
                                Text(listOf(task.schedule, task.projectTitle, scheduledDestination(task), if (task.nextRun.isNotEmpty() && !webhook) "next ${task.nextRun}" else null, task.lastRun).filterNotNull().filter { it.isNotEmpty() }.joinToString(" · "))
                                if (task.runState.isNotEmpty()) Text(task.runState, color = MaterialTheme.colorScheme.primary, style = MaterialTheme.typography.bodySmall)
                                else if (task.lastError.isNotEmpty()) Text(task.lastError, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall, maxLines = 2)
                            }
                        },
                        trailingContent = {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Switch(checked = task.enabled, onCheckedChange = { core.update(Event.SetScheduledEnabled(task.id, it)) })
                                Box {
                                    IconButton(onClick = { menu = true }) { Icon(Icons.Outlined.MoreVert, "Actions for ${task.name}") }
                                    DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                                        DropdownMenuItem(text = { Text("Edit") }, leadingIcon = { Icon(Icons.Outlined.Tune, null) }, onClick = { menu = false; editing = editedScheduledTask(task) })
                                        val thread = task.threadId.ifEmpty { task.lastThreadId }
                                        if (thread.isNotEmpty()) DropdownMenuItem(text = { Text("Open thread") }, onClick = { menu = false; openThread(thread) })
                                        val showing = settings.scheduledRunsTask == task.id
                                        DropdownMenuItem(text = { Text(if (showing) "Hide runs" else "Runs") }, onClick = { menu = false; core.update(Event.LoadScheduledRuns(if (showing) "" else task.id)) })
                                        if (!webhook) DropdownMenuItem(text = { Text("Run now") }, leadingIcon = { Icon(Icons.Outlined.PlayArrow, null) }, onClick = { menu = false; core.update(Event.RunScheduledNow(task.id)) })
                                        if (webhook) {
                                            DropdownMenuItem(text = { Text("Copy URL") }, leadingIcon = { Icon(Icons.Outlined.ContentCopy, null) }, onClick = { menu = false; copyText(context, task.webhookUrl) })
                                            DropdownMenuItem(text = { Text("New URL…") }, leadingIcon = { Icon(Icons.Outlined.Key, null) }, onClick = { menu = false; rotating = task })
                                            DropdownMenuItem(text = { Text("Deliveries") }, leadingIcon = { Icon(Icons.Outlined.Download, null) }, onClick = { menu = false; core.update(Event.LoadWebhookDeliveries(task.id)) })
                                        }
                                        DropdownMenuItem(text = { Text("Delete", color = MaterialTheme.colorScheme.error) }, leadingIcon = { Icon(Icons.Outlined.Delete, null) }, onClick = { menu = false; deleting = task })
                                    }
                                }
                            }
                        },
                        colors = ListItemDefaults.colors(containerColor = clear),
                    )
                    if (settings.scheduledRunsTask == task.id) {
                        if (settings.scheduledRuns.isEmpty()) Text("No runs yet.", style = MaterialTheme.typography.bodySmall, modifier = Modifier.padding(horizontal = Space.l, vertical = Space.xs))
                        settings.scheduledRuns.forEach { r ->
                            Text(
                                listOf(r.status, r.trigger, r.started, r.error.ifEmpty { r.threadTitle }).filter { it.isNotEmpty() }.joinToString(" · "),
                                style = MaterialTheme.typography.bodySmall,
                                color = if (r.failed) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface,
                                modifier = Modifier.fillMaxWidth().clickable(enabled = r.threadId.isNotEmpty()) { openThread(r.threadId) }.padding(horizontal = Space.l, vertical = 2.dp),
                            )
                        }
                    }
                    if (webhook) {
                        Text(task.webhookUrl, style = MaterialTheme.typography.bodySmall, maxLines = 1, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(horizontal = Space.l))
                        if (settings.webhookDeliveriesTask == task.id) {
                            if (settings.webhookDeliveries.isEmpty()) Text("No requests yet.", style = MaterialTheme.typography.bodySmall, modifier = Modifier.padding(horizontal = Space.l, vertical = Space.xs))
                            settings.webhookDeliveries.forEach { d ->
                                Text(listOf(d.outcome, d.method, d.detail, d.received).filter { it.isNotEmpty() }.joinToString(" · "), style = MaterialTheme.typography.bodySmall, modifier = Modifier.padding(horizontal = Space.l, vertical = 2.dp))
                            }
                        }
                    }
                }
            }
        }
    }
    editing?.let { draft -> ScheduledTaskEditorFor(core, draft) { editing = null } }
    deleting?.let { task ->
        AlertDialog(
            onDismissRequest = { deleting = null },
            title = { Text("Delete “${task.name}”?") },
            confirmButton = { TextButton(onClick = { deleting = null; core.update(Event.DeleteScheduled(task.id)) }) { Text("Delete", color = MaterialTheme.colorScheme.error) } },
            dismissButton = { TextButton(onClick = { deleting = null }) { Text("Cancel") } },
        )
    }
    rotating?.let { task ->
        AlertDialog(
            onDismissRequest = { rotating = null },
            title = { Text("Give “${task.name}” a new URL?") },
            text = { Text("The current URL stops working.") },
            confirmButton = { TextButton(onClick = { rotating = null; core.update(Event.RotateWebhookToken(task.id)) }) { Text("New URL") } },
            dismissButton = { TextButton(onClick = { rotating = null }) { Text("Cancel") } },
        )
    }
}

@Composable
fun MaintenanceScreen(core: Core, onBack: () -> Unit) {
    val settings by core.view.settings.watch()
    LaunchedEffect(Unit) { core.update(Event.LoadSettings("storage")); core.update(Event.LoadSettings("diagnostics")) }
    val storage = settings.storage
    ToolScaffold("Maintenance", onBack) { padding ->
        LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(top = padding.calculateTopPadding(), bottom = Space.xxl)) {
            item("storage") {
                SectionHeader("Storage")
                ListItem(headlineContent = { Text("Database") }, trailingContent = { Text(storage.databaseSize) }, colors = ListItemDefaults.colors(containerColor = clear))
                ListItem(headlineContent = { Text("Reclaimable") }, trailingContent = { Text(storage.reclaimable) }, colors = ListItemDefaults.colors(containerColor = clear))
                if (storage.lastCleanup.isNotEmpty()) ListItem(headlineContent = { Text("Last cleanup") }, trailingContent = { Text(storage.lastCleanup) }, colors = ListItemDefaults.colors(containerColor = clear))
                if (storage.busy) LinearProgressIndicator(Modifier.fillMaxWidth().padding(horizontal = Space.l))
                Row(Modifier.padding(horizontal = Space.l, vertical = Space.s), horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                    FilledTonalButton(onClick = { core.update(Event.RunStorageCleanup) }, enabled = !storage.busy) { Text("Clean up now") }
                    OutlinedButton(onClick = { core.update(Event.ReclaimDatabase) }, enabled = !storage.busy) { Text("Reclaim space") }
                }
            }
            item("diag") {
                SectionHeader("Server processes")
                Text("CPU ${settings.diagnostics.cpu} · memory ${settings.diagnostics.memory}", style = MaterialTheme.typography.bodyMedium, modifier = Modifier.padding(horizontal = Space.l))
            }
            items(settings.diagnostics.processes, key = { "p/${it.pid}" }) { p ->
                ListItem(
                    headlineContent = { Text(p.command, fontFamily = Mono, maxLines = 1, overflow = TextOverflow.Ellipsis) },
                    supportingContent = { Text("pid ${p.pid} · ${p.cpu} · ${p.memory}") },
                    colors = ListItemDefaults.colors(containerColor = clear),
                    modifier = Modifier.padding(start = (p.depth.toInt() * 12).dp),
                )
            }
        }
    }
}

/** Remote access, pairing another device, and who is signed in. */
@Composable
fun AccessScreen(core: Core, onBack: () -> Unit) {
    val settings by core.view.settings.watch()
    val remote = settings.remote
    val context = LocalContext.current
    var revoking by remember { mutableStateOf<com.crayen.domovoi.app.AccessEntryView?>(null) }
    var pairing by remember { mutableStateOf(false) }
    LaunchedEffect(Unit) { core.update(Event.LoadRemoteAccess); core.update(Event.LoadSettings("access")) }
    ToolScaffold("Access & devices", onBack) { padding ->
        LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(top = padding.calculateTopPadding(), bottom = Space.xxl)) {
            item("remote") {
                SectionHeader("Remote access")
                ListItem(
                    headlineContent = { Text("Reach this server from anywhere") },
                    supportingContent = { Text(if (!remote.available) "Not available on this server." else remote.state.ifEmpty { if (remote.running) "Running" else "Off" }) },
                    trailingContent = { Switch(checked = remote.enabled, enabled = remote.available && !remote.busy, onCheckedChange = { core.update(Event.SetRemoteAccess(it)) }) },
                    colors = ListItemDefaults.colors(containerColor = clear),
                )
                remote.error?.let { Text(it, color = MaterialTheme.colorScheme.error, modifier = Modifier.padding(horizontal = Space.l)) }
                Button(onClick = { pairing = true; core.update(Event.CreatePairingLink) }, enabled = !remote.busy, modifier = Modifier.padding(horizontal = Space.l, vertical = Space.s)) { Text("Pair a device…") }
            }
            item("devices-h") { SectionHeader("Signed-in devices") }
            items(settings.access, key = { it.id }) { entry ->
                ListItem(
                    headlineContent = { Text(entry.label + if (entry.current) " (this device)" else "") },
                    supportingContent = { Text(listOf(entry.detail, if (entry.connected) "connected" else null).filterNotNull().filter { it.isNotEmpty() }.joinToString(" · ")) },
                    trailingContent = { if (!entry.current && entry.kind == "session") TextButton(onClick = { revoking = entry }) { Text("Sign out") } },
                    colors = ListItemDefaults.colors(containerColor = clear),
                )
            }
        }
    }
    if (pairing) {
        ModalBottomSheet(onDismissRequest = { pairing = false }) {
            Column(Modifier.fillMaxWidth().padding(horizontal = Space.xl), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(Space.m)) {
                Text("Pair a device", style = MaterialTheme.typography.titleLarge)
                if (remote.link.isEmpty()) LinearProgressIndicator(Modifier.fillMaxWidth())
                else {
                    QrCode(remote.link, Modifier.size(240.dp))
                    if (remote.code.isNotEmpty()) Text(remote.code, style = MaterialTheme.typography.headlineSmall, fontFamily = Mono)
                    if (remote.linkExpiresAt.isNotEmpty()) Text("Expires ${remote.linkExpiresAt}", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    Row(horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                        FilledTonalButton(onClick = {
                            context.startActivity(Intent.createChooser(Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, remote.link), "Share pairing link"))
                        }) { Icon(Icons.Outlined.Share, null); Text("  Share") }
                        OutlinedButton(onClick = { copyText(context, remote.link) }) { Icon(Icons.Outlined.ContentCopy, null); Text("  Copy") }
                        TextButton(onClick = { core.update(Event.CreatePairingLink) }) { Text("New code") }
                    }
                }
                Spacer(Modifier.height(Space.xl))
            }
        }
    }
    revoking?.let { entry ->
        AlertDialog(
            onDismissRequest = { revoking = null },
            title = { Text("Sign out ${entry.label}?") },
            text = { Text("It has to pair again to reach this server.") },
            confirmButton = { TextButton(onClick = { revoking = null; core.update(Event.RevokeClient(entry.id)) }) { Text("Sign out", color = MaterialTheme.colorScheme.error) } },
            dismissButton = { TextButton(onClick = { revoking = null }) { Text("Cancel") } },
        )
    }
}

/** `text` as a QR code (ZXing). */
@Composable
fun QrCode(text: String, modifier: Modifier = Modifier) {
    val bitmap = remember(text) {
        val matrix = QRCodeWriter().encode(text, BarcodeFormat.QR_CODE, 512, 512)
        Bitmap.createBitmap(matrix.width, matrix.height, Bitmap.Config.ARGB_8888).apply {
            for (x in 0 until matrix.width) for (y in 0 until matrix.height) setPixel(x, y, if (matrix[x, y]) android.graphics.Color.BLACK else android.graphics.Color.WHITE)
        }
    }
    Surface(color = Color.White, shape = MaterialTheme.shapes.medium, modifier = modifier) {
        Image(bitmap.asImageBitmap(), contentDescription = "Pairing QR code", modifier = Modifier.padding(Space.m))
    }
}

/** Where a scheduled task's runs go, for its row. */
private fun scheduledDestination(task: ScheduledTaskView): String = when {
    task.threadMode == "new_thread" -> "new thread each run"
    task.threadId.isEmpty() -> "the thread its first run starts"
    else -> {
        val title = if (task.threadTitle.isEmpty()) "a thread not on this device" else "“${task.threadTitle}”"
        if (task.threadMode == "fresh_session") "in $title, fresh session" else "in $title"
    }
}

/** The editor for `draft`, its choices from the core's views; saving closes it. */
@Composable
fun ScheduledTaskEditorFor(core: Core, draft: com.crayen.domovoi.app.ScheduledTaskEdit, onDismiss: () -> Unit) {
    val settings by core.view.settings.watch()
    val composer by core.view.composer.watch()
    ScheduledTaskEditorSheet(
        start = draft,
        projects = settings.projects.map { it.id to it.title },
        threadsOf = { id -> settings.scheduledThreadChoices.filter { it.projectId == id } },
        isScratch = { id -> settings.projects.any { it.id == id && it.isScratch } },
        // Favorites are left out of the groups: listed first here.
        models = composer.favoriteModels.map { "${it.option.instanceId}|${it.option.model}" to "\u2605 ${if (it.detail.isEmpty()) it.option.label else "${it.detail} · ${it.name}"}" } +
            composer.modelGroups.flatMap { g -> g.models.map { "${it.option.instanceId}|${it.option.model}" to "${g.provider} · ${it.name}" } },
        onDismiss = onDismiss,
    ) { task, threadMode ->
        onDismiss()
        if (threadMode != null) core.update(Event.SetThreadRuntimeMode(task.threadId, threadMode))
        core.update(Event.SaveScheduled(task))
    }
}
