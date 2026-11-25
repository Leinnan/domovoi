package com.crayen.domovoi.ui.thread

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Button
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
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.app.SecretRequestView
import com.crayen.domovoi.app.WorkLinkView
import com.crayen.domovoi.ui.theme.LocalTints
import com.crayen.domovoi.ui.theme.Space

/**
 * Secrets the agent asked for (`request_secret`), docked above the composer
 * beside the approvals: a password field, Save and Decline each.
 *
 * What is typed is held in plain `remember` state (never `rememberSaveable`,
 * which would put it in the saved-instance bundle) until it is handed to the
 * core once (`AnswerSecret`), which sends it and keeps it nowhere; the field
 * is cleared at once, and when the card goes.
 */
@Composable
fun SecretRequestBar(requests: List<SecretRequestView>, send: (Event) -> Unit, modifier: Modifier = Modifier) {
    val view = LocalView.current
    LaunchedEffect(requests.map { it.itemId }) { view.announceForAccessibility("The agent asks for a secret") }
    Column(modifier.fillMaxWidth().padding(horizontal = Space.s, vertical = Space.xs), verticalArrangement = Arrangement.spacedBy(Space.s)) {
        requests.forEach { request -> SecretRequestCard(request, send) }
    }
}

@Composable
private fun SecretRequestCard(request: SecretRequestView, send: (Event) -> Unit) {
    var value by remember(request.itemId) { mutableStateOf("") }
    DisposableEffect(request.itemId) { onDispose { value = "" } }
    val uri = LocalUriHandler.current
    val canSave = !request.sending && value.isNotBlank()
    val save = {
        if (canSave) {
            val secret = value
            value = ""
            send(Event.AnswerSecret(request.itemId, secret))
        }
    }
    Surface(
        color = MaterialTheme.colorScheme.surfaceContainerHigh,
        shape = MaterialTheme.shapes.extraLarge,
        tonalElevation = 3.dp,
        shadowElevation = 4.dp,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(Modifier.padding(Space.l), verticalArrangement = Arrangement.spacedBy(Space.s)) {
            Text("Secret requested", style = MaterialTheme.typography.labelMedium, color = LocalTints.current.caution, fontWeight = FontWeight.SemiBold)
            Text("The agent asks for ${request.label}", style = MaterialTheme.typography.titleSmall)
            if (request.reason.isNotEmpty()) {
                Text(request.reason, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            request.url?.let { url -> TextButton(onClick = { uri.openUri(url) }) { Text("Get one") } }
            OutlinedTextField(
                value = value,
                onValueChange = { value = it },
                enabled = !request.sending,
                singleLine = true,
                placeholder = { Text(request.placeholder.ifEmpty { "Secret value" }) },
                visualTransformation = PasswordVisualTransformation(),
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password, imeAction = ImeAction.Done),
                keyboardActions = KeyboardActions(onDone = { save() }),
                modifier = Modifier.fillMaxWidth(),
            )
            Text(request.hint, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            Row(horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                OutlinedButton(onClick = { value = ""; send(Event.DeclineSecret(request.itemId)) }, enabled = !request.sending, modifier = Modifier.weight(1f)) { Text("Decline") }
                Button(onClick = save, enabled = canSave, modifier = Modifier.weight(1f)) {
                    Text(if (request.sending) "Saving…" else "Save secret", fontWeight = FontWeight.SemiBold)
                }
            }
        }
    }
}

/** The pull requests and issues linked to the thread, as chips that open in the browser. */
@Composable
fun WorkLinksStrip(links: List<WorkLinkView>, modifier: Modifier = Modifier) {
    if (links.isEmpty()) return
    val uri = LocalUriHandler.current
    val tints = LocalTints.current
    Row(modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = Space.l), horizontalArrangement = Arrangement.spacedBy(Space.s)) {
        links.forEach { link ->
            AssistChip(
                onClick = { uri.openUri(link.url) },
                label = {
                    val text = buildString {
                        append(link.label)
                        if (link.title.isNotEmpty()) append(" · ").append(link.title)
                    }
                    Text(text, maxLines = 1, overflow = TextOverflow.Ellipsis)
                },
                trailingIcon = if (link.state.isEmpty()) null else {
                    {
                        Text(
                            link.state,
                            style = MaterialTheme.typography.labelSmall,
                            color = when (link.state) {
                                "merged" -> tints.plan
                                "closed" -> MaterialTheme.colorScheme.error
                                "draft" -> MaterialTheme.colorScheme.onSurfaceVariant
                                else -> tints.success
                            },
                        )
                    }
                },
            )
        }
    }
}
