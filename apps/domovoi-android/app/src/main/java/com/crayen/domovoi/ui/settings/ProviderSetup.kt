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
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExposedDropdownMenuAnchorType
import androidx.compose.material3.ExposedDropdownMenuBox
import androidx.compose.material3.ExposedDropdownMenuDefaults
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
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
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import com.crayen.domovoi.app.ChoiceView
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.app.ProviderInstanceEdit
import com.crayen.domovoi.app.ProviderView
import com.crayen.domovoi.ui.common.clipboardText
import com.crayen.domovoi.ui.theme.Space

/** A provider instance being edited, or added (`instanceId` empty). */
data class ProviderEditing(val edit: ProviderInstanceEdit, val methods: List<ChoiceView>, val builtIn: Boolean, val hasKey: Boolean)

fun editedProvider(p: ProviderView) = ProviderEditing(
    ProviderInstanceEdit(
        instanceId = p.instanceId, driver = p.driver, displayName = p.config.displayName, enabled = p.enabled, authMethod = p.config.authMethod,
        apiKey = "", gcpProject = p.config.gcpProject, gcpLocation = p.config.gcpLocation, binaryPath = p.config.binaryPath,
    ),
    p.config.authMethods, p.config.builtIn, p.config.hasApiKey,
)

/** A new Antigravity instance: an API key or Vertex (the Google sign-in is the built-in one's). */
fun newAntigravity() = ProviderEditing(
    ProviderInstanceEdit(
        instanceId = "", driver = "antigravity", displayName = "", enabled = true, authMethod = "gemini-api-key",
        apiKey = "", gcpProject = "", gcpLocation = "", binaryPath = "",
    ),
    listOf(ChoiceView("gemini-api-key", "Gemini API key"), ChoiceView("agent-platform", "Agent Platform (Vertex AI)")),
    builtIn = false, hasKey = false,
)

/** A provider's sign-in, managed runtime and settings, under its row in the providers list. */
@Composable
fun ProviderSetup(p: ProviderView, update: (Event) -> Unit, edit: (ProviderEditing) -> Unit) {
    val s = p.signIn
    val i = p.install
    val uri = LocalUriHandler.current
    val context = LocalContext.current
    var redirect by remember { mutableStateOf("") }
    val note: @Composable (String) -> Unit = { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
    Column(Modifier.fillMaxWidth().padding(horizontal = Space.l), verticalArrangement = Arrangement.spacedBy(Space.s)) {
        if (p.canAuthenticate && !p.signedIn) {
            if (s.busy) {
                if (s.url.isNotEmpty()) FilledTonalButton(onClick = { uri.openUri(s.url) }) { Text("Open Google sign-in") }
                if (s.takesRedirect) {
                    // Google returns to the server's machine: from this phone, paste where the browser ended.
                    OutlinedTextField(
                        redirect, { redirect = it }, label = { Text("Redirect URL from the browser") }, singleLine = true,
                        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, autoCorrectEnabled = false),
                        trailingIcon = { TextButton(onClick = { clipboardText(context)?.let { redirect = it } }) { Text("Paste") } },
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
                FlowRow(horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                    if (s.takesRedirect) Button(onClick = { update(Event.CompleteProviderSignIn(p.instanceId, redirect)); redirect = "" }, enabled = redirect.isNotBlank()) { Text("Continue") }
                    TextButton(onClick = { update(Event.CancelProviderSignIn(p.instanceId)) }) { Text("Cancel sign-in") }
                }
            } else {
                Button(onClick = { update(Event.StartProviderSignIn(p.instanceId)) }) { Text(if (p.config.authMethod.startsWith("oauth")) "Sign in with Google" else "Connect") }
            }
        }
        if (s.message.isNotEmpty() && s.phase != "idle") note(s.message)
        if (p.canInstall) {
            if (i.busy) {
                if (i.progress > 0u) LinearProgressIndicator(progress = { i.progress.toFloat() / 1000f }, modifier = Modifier.fillMaxWidth())
                else LinearProgressIndicator(Modifier.fillMaxWidth())
                note(listOf(i.message.ifEmpty { "Installing…" }, i.progressLabel).filter { it.isNotEmpty() }.joinToString(" · "))
                TextButton(onClick = { update(Event.CancelProviderInstall(p.instanceId)) }) { Text("Cancel install") }
            } else {
                if (i.installedVersion.isNotEmpty()) note("Installed runtime ${i.installedVersion}")
                FlowRow(horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                    if (p.version.isEmpty() || i.updateAvailable) {
                        OutlinedButton(onClick = { update(Event.InstallProvider(p.instanceId)) }) { Text(if (i.updateAvailable) "Update to ${i.offeredVersion}" else "Install runtime") }
                    }
                    if (i.canRemove) TextButton(onClick = { update(Event.RemoveProviderInstall(p.instanceId)) }) { Text("Remove runtime", color = MaterialTheme.colorScheme.error) }
                }
                if (i.message.isNotEmpty() && i.phase != "idle" && i.phase != "succeeded") note(i.message)
            }
        }
        if (p.config.editable) TextButton(onClick = { edit(editedProvider(p)) }) { Text("Edit settings") }
    }
}

/** An Antigravity instance's settings: its sign-in method and credentials. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ProviderEditorSheet(start: ProviderEditing, update: (Event) -> Unit, onDismiss: () -> Unit) {
    var e by remember { mutableStateOf(start.edit) }
    val isNew = e.instanceId.isEmpty()
    val usesKey = e.authMethod == "gemini-api-key" || e.authMethod == "agent-platform"
    val usesProject = e.authMethod == "oauth-business" || e.authMethod == "agent-platform"
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        Column(Modifier.padding(horizontal = Space.xl).verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(Space.m)) {
            Text(if (isNew) "New Antigravity account" else "Edit ${e.displayName.ifEmpty { e.instanceId }}", style = MaterialTheme.typography.titleLarge)
            if (!start.builtIn) OutlinedTextField(e.displayName, { e = e.copy(displayName = it) }, label = { Text("Name") }, placeholder = { Text("Antigravity (work)") }, singleLine = true, modifier = Modifier.fillMaxWidth())
            MethodChoice(start.methods, e.authMethod) { e = e.copy(authMethod = it) }
            if (usesKey) {
                OutlinedTextField(
                    e.apiKey, { e = e.copy(apiKey = it) }, label = { Text("API key") },
                    placeholder = { Text(if (start.hasKey) "Saved (type to replace)" else if (e.authMethod == "agent-platform") "Optional" else "Required") },
                    visualTransformation = PasswordVisualTransformation(), singleLine = true,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password, autoCorrectEnabled = false),
                    modifier = Modifier.fillMaxWidth(),
                )
            }
            if (usesProject) {
                OutlinedTextField(e.gcpProject, { e = e.copy(gcpProject = it) }, label = { Text("GCP project") }, singleLine = true, modifier = Modifier.fillMaxWidth())
                OutlinedTextField(e.gcpLocation, { e = e.copy(gcpLocation = it) }, label = { Text("GCP location") }, placeholder = { Text("us-central1") }, singleLine = true, modifier = Modifier.fillMaxWidth())
            }
            OutlinedTextField(e.binaryPath, { e = e.copy(binaryPath = it) }, label = { Text("Binary path") }, placeholder = { Text("Automatic") }, singleLine = true, modifier = Modifier.fillMaxWidth())
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Text("Enabled", Modifier.weight(1f), style = MaterialTheme.typography.bodyLarge)
                Switch(checked = e.enabled, onCheckedChange = { e = e.copy(enabled = it) })
            }
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(Space.s), verticalAlignment = Alignment.CenterVertically) {
                if (!isNew && !start.builtIn) TextButton(onClick = { update(Event.RemoveProviderInstance(e.instanceId)); onDismiss() }) { Text("Remove", color = MaterialTheme.colorScheme.error) }
                Spacer(Modifier.weight(1f))
                TextButton(onClick = onDismiss) { Text("Cancel") }
                Button(onClick = { update(Event.SaveProviderInstance(e)); onDismiss() }, enabled = !(isNew && e.authMethod == "gemini-api-key" && e.apiKey.isEmpty())) { Text(if (isNew) "Add" else "Save") }
            }
            Spacer(Modifier.height(Space.l))
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun MethodChoice(methods: List<ChoiceView>, selected: String, pick: (String) -> Unit) {
    var open by remember { mutableStateOf(false) }
    ExposedDropdownMenuBox(expanded = open, onExpandedChange = { open = it }) {
        OutlinedTextField(
            value = methods.firstOrNull { it.value == selected }?.label ?: selected, onValueChange = {}, readOnly = true, label = { Text("Sign-in method") },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(open) },
            modifier = Modifier.fillMaxWidth().menuAnchor(ExposedDropdownMenuAnchorType.PrimaryNotEditable),
        )
        ExposedDropdownMenu(expanded = open, onDismissRequest = { open = false }) {
            methods.forEach { c -> DropdownMenuItem(text = { Text(c.label) }, onClick = { open = false; pick(c.value) }) }
        }
    }
}
