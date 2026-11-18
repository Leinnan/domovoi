package com.crayen.domovoi.ui.pairing

import android.Manifest
import android.content.ClipboardManager
import android.content.Context
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Computer
import androidx.compose.material.icons.outlined.ContentPaste
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.ExpandLess
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material.icons.outlined.Public
import androidx.compose.material.icons.outlined.QrCodeScanner
import androidx.compose.material.icons.outlined.Terminal
import androidx.compose.material.icons.outlined.Wifi
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.outlined.Delete
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.crayen.domovoi.app.ConnectionView
import com.crayen.domovoi.app.EnvironmentView
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.core.Core
import com.crayen.domovoi.ui.theme.Mono
import com.crayen.domovoi.ui.theme.Space

/**
 * Pairing: scan the server's QR code, paste its link, or enter a local
 * address and code. Also lists saved servers (`AddServerView`).
 */
@Composable
fun PairingScreen(core: Core, onboarding: Boolean, onBack: (() -> Unit)? = null) {
    val connection by core.view.connection.collectAsStateWithLifecycle()
    val context = LocalContext.current
    var scanning by rememberSaveable { mutableStateOf(false) }
    var cameraDenied by rememberSaveable { mutableStateOf(false) }
    val busy = connection.state == "pairing" || connection.state == "connecting"
    val askCamera = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        cameraDenied = !granted
        scanning = granted
    }
    val connectRemote: (String) -> Unit = { link -> core.update(Event.ConnectRemote(link.trim())) }

    if (scanning) {
        QrScannerScreen(
            onFound = { scanning = false; connectRemote(it) },
            onClose = { scanning = false },
        )
        return
    }

    Scaffold { padding ->
        Box(Modifier.fillMaxSize().padding(padding), contentAlignment = Alignment.TopCenter) {
            Column(
                Modifier
                    .widthIn(max = 560.dp)
                    .fillMaxWidth()
                    .verticalScroll(rememberScrollState())
                    .imePadding()
                    .padding(horizontal = Space.l, vertical = Space.xl),
                verticalArrangement = Arrangement.spacedBy(Space.l),
            ) {
                if (onboarding) Hero() else Text("Add server", style = MaterialTheme.typography.headlineMedium)

                RemoteCard(
                    busy = busy,
                    cameraDenied = cameraDenied,
                    onScan = {
                        if (ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) scanning = true
                        else askCamera.launch(Manifest.permission.CAMERA)
                    },
                    onConnect = connectRemote,
                )
                LocalCard(busy = busy, onConnect = { url, code -> core.update(Event.Connect(url.trim(), code.trim())) })
                Status(connection, busy)
                if (connection.environments.isNotEmpty()) SavedServers(core, connection.environments)
                onBack?.let { TextButton(onClick = it, modifier = Modifier.align(Alignment.CenterHorizontally)) { Text("Close") } }
            }
        }
    }
}

@Composable
private fun Hero() {
    Column(verticalArrangement = Arrangement.spacedBy(Space.s), modifier = Modifier.padding(top = Space.xl, bottom = Space.s)) {
        Card(
            shape = MaterialTheme.shapes.large,
            colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.primaryContainer),
        ) {
            Icon(
                Icons.Outlined.Terminal, contentDescription = null,
                tint = MaterialTheme.colorScheme.onPrimaryContainer,
                modifier = Modifier.padding(Space.m).size(36.dp),
            )
        }
        Text("Domovoi", style = MaterialTheme.typography.displaySmall, fontWeight = FontWeight.SemiBold)
        Text(
            "Your coding agents, from anywhere. Pair this phone with Domovoi Server on your computer.",
            style = MaterialTheme.typography.bodyLarge,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun SectionCard(title: String, icon: androidx.compose.ui.graphics.vector.ImageVector, content: @Composable androidx.compose.foundation.layout.ColumnScope.() -> Unit) {
    Card(
        shape = MaterialTheme.shapes.medium,
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceContainerLow),
    ) {
        Column(Modifier.padding(Space.l), verticalArrangement = Arrangement.spacedBy(Space.m)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                Icon(icon, contentDescription = null, tint = MaterialTheme.colorScheme.primary, modifier = Modifier.size(20.dp))
                Text(title, style = MaterialTheme.typography.titleMedium)
            }
            content()
        }
    }
}

@Composable
private fun RemoteCard(busy: Boolean, cameraDenied: Boolean, onScan: () -> Unit, onConnect: (String) -> Unit) {
    var link by rememberSaveable { mutableStateOf("") }
    val context = LocalContext.current
    SectionCard("Remote server", Icons.Outlined.Public) {
        Button(onClick = onScan, enabled = !busy, modifier = Modifier.fillMaxWidth()) {
            Icon(Icons.Outlined.QrCodeScanner, contentDescription = null)
            Spacer(Modifier.size(Space.s))
            Text("Scan pairing QR code")
        }
        if (cameraDenied) {
            Text(
                "Domovoi may not use the camera. Allow it in Settings, or paste the link.",
                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error,
            )
        }
        OutlinedTextField(
            value = link,
            onValueChange = { link = it },
            label = { Text("Pairing link") },
            placeholder = { Text("domovoi://connect?…", fontFamily = Mono) },
            singleLine = true,
            textStyle = MaterialTheme.typography.bodyMedium.copy(fontFamily = Mono),
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, imeAction = ImeAction.Go, autoCorrectEnabled = false),
            trailingIcon = {
                IconButton(onClick = { link = clipboardText(context) ?: link }) {
                    Icon(Icons.Outlined.ContentPaste, contentDescription = "Paste")
                }
            },
            modifier = Modifier.fillMaxWidth(),
        )
        FilledTonalButton(
            onClick = { onConnect(link); link = "" },
            enabled = link.isNotBlank() && !busy,
            modifier = Modifier.align(Alignment.End),
        ) { Text("Connect") }
        Text(
            "On the computer: Settings → Access in Domovoi, or `domovoi-server pair --remote`. Works over any network; traffic is end-to-end encrypted.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun LocalCard(busy: Boolean, onConnect: (String, String) -> Unit) {
    var expanded by rememberSaveable { mutableStateOf(false) }
    var url by rememberSaveable { mutableStateOf("") }
    var code by rememberSaveable { mutableStateOf("") }
    Card(
        shape = MaterialTheme.shapes.medium,
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceContainerLow),
    ) {
        ListItem(
            headlineContent = { Text("Local network address") },
            supportingContent = { Text("Same Wi-Fi as the server, with a pairing code") },
            leadingContent = { Icon(Icons.Outlined.Wifi, contentDescription = null, tint = MaterialTheme.colorScheme.primary) },
            trailingContent = { Icon(if (expanded) Icons.Outlined.ExpandLess else Icons.Outlined.ExpandMore, contentDescription = null) },
            colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.surfaceContainerLow),
            modifier = Modifier.clickable { expanded = !expanded },
        )
        AnimatedVisibility(expanded) {
            Column(Modifier.padding(start = Space.l, end = Space.l, bottom = Space.l), verticalArrangement = Arrangement.spacedBy(Space.m)) {
                OutlinedTextField(
                    value = url, onValueChange = { url = it },
                    label = { Text("Server address") },
                    placeholder = { Text("http://192.168.1.10:13773") },
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, autoCorrectEnabled = false),
                    modifier = Modifier.fillMaxWidth(),
                )
                OutlinedTextField(
                    value = code, onValueChange = { code = it },
                    label = { Text("Pairing code") },
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Characters, autoCorrectEnabled = false),
                    modifier = Modifier.fillMaxWidth(),
                )
                FilledTonalButton(
                    onClick = { onConnect(url, code); code = "" },
                    enabled = url.isNotBlank() && !busy,
                    modifier = Modifier.align(Alignment.End),
                ) { Text("Connect") }
            }
        }
    }
}

@Composable
private fun Status(connection: ConnectionView, busy: Boolean) {
    val error = connection.error
    when {
        busy -> Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.m)) {
            CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
            Text(if (connection.state == "pairing") "Pairing…" else "Connecting…", color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        error != null -> Card(colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.errorContainer)) {
            Row(Modifier.padding(Space.l), horizontalArrangement = Arrangement.spacedBy(Space.m)) {
                Icon(Icons.Outlined.ErrorOutline, contentDescription = null, tint = MaterialTheme.colorScheme.onErrorContainer)
                Column(verticalArrangement = Arrangement.spacedBy(Space.xs)) {
                    Text(error, color = MaterialTheme.colorScheme.onErrorContainer)
                    if (connection.hint.isNotEmpty()) Text(connection.hint, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onErrorContainer)
                }
            }
        }
    }
}

@Composable
private fun SavedServers(core: Core, environments: List<EnvironmentView>) {
    Column {
        Text("Saved servers", style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.primary, modifier = Modifier.padding(start = Space.l, bottom = Space.xs))
        Card(shape = MaterialTheme.shapes.medium, colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceContainerLow)) {
            environments.forEachIndexed { i, env ->
                if (i > 0) HorizontalDivider(Modifier.padding(start = 56.dp))
                ListItem(
                    headlineContent = { Text(env.label) },
                    supportingContent = { Text(env.detail, maxLines = 1) },
                    leadingContent = { Icon(if (env.remote) Icons.Outlined.Public else Icons.Outlined.Computer, contentDescription = null) },
                    trailingContent = {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            if (env.active) Icon(Icons.Filled.Check, contentDescription = "In use", tint = MaterialTheme.colorScheme.primary)
                            IconButton(onClick = { core.update(Event.ForgetEnvironment(env.id)) }) {
                                Icon(Icons.Outlined.Delete, contentDescription = "Forget ${env.label}")
                            }
                        }
                    },
                    colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.surfaceContainerLow),
                    modifier = Modifier.clickable { core.update(Event.SelectEnvironment(env.id)) },
                )
            }
        }
    }
}

private fun clipboardText(context: Context): String? =
    context.getSystemService(ClipboardManager::class.java)?.primaryClip?.takeIf { it.itemCount > 0 }?.getItemAt(0)?.coerceToText(context)?.toString()
