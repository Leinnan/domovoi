package com.crayen.domovoi.ui.pairing

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.key
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import com.crayen.domovoi.app.LinkRequestView
import com.crayen.domovoi.ui.theme.Mono
import com.crayen.domovoi.ui.theme.Space

/** One line of what identifies the server a link asks to connect to. */
internal data class LinkRequestLine(val label: String, val value: String, val monospace: Boolean = false)

/**
 * What identifies the server, the same on every client: the start of its key,
 * which a link cannot fake, then the name it is saved under here. Only a
 * server not saved here shows the name the link gives it, marked as the
 * link's: anyone can write anything there.
 */
internal fun linkRequestLines(request: LinkRequestView): List<LinkRequestLine> = buildList {
    add(LinkRequestLine("Server key", "${request.endpoint}…", monospace = true))
    when {
        request.knownLabel.isNotEmpty() -> add(LinkRequestLine("Saved here as", request.knownLabel))
        request.linkName.isNotEmpty() -> add(LinkRequestLine("Name in the link", request.linkName))
    }
}

/**
 * The question a link from outside the app asks before anything connects
 * (`ConnectionView.linkRequest`). `onAnswer` takes the user's yes or no; back
 * and a tap outside count as no.
 */
@Composable
fun LinkRequestDialog(request: LinkRequestView, onAnswer: (Boolean) -> Unit) {
    // A request that replaces the one shown gets a dialog of its own.
    key(request.id) {
        AlertDialog(
            onDismissRequest = { onAnswer(false) },
            title = { Text(request.title) },
            text = {
                // The message for a server not saved here is long: it scrolls on a small screen.
                Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(Space.m)) {
                    Text(request.message)
                    for (line in linkRequestLines(request)) {
                        Column(verticalArrangement = Arrangement.spacedBy(Space.xxs)) {
                            Text(line.label, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                            Text(
                                line.value,
                                style = MaterialTheme.typography.bodyMedium,
                                fontFamily = if (line.monospace) Mono else FontFamily.Default,
                                color = MaterialTheme.colorScheme.onSurface,
                            )
                        }
                    }
                }
            },
            confirmButton = { TextButton(onClick = { onAnswer(true) }) { Text(request.confirmLabel) } },
            dismissButton = { TextButton(onClick = { onAnswer(false) }) { Text("Cancel") } },
        )
    }
}
