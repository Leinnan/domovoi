package com.crayen.domovoi.ui.threads

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.ArrowDropDown
import androidx.compose.material.icons.outlined.Computer
import androidx.compose.material.icons.outlined.Public
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.crayen.domovoi.app.ConnectionView
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.core.Core
import com.crayen.domovoi.ui.common.watch
import com.crayen.domovoi.ui.nav.Navigator
import com.crayen.domovoi.ui.nav.Screen
import com.crayen.domovoi.ui.theme.Space

/** How the server is reached, under its name: "Direct · 29 ms". */
fun routeLine(c: ConnectionView): String? = when (c.state) {
    "connected" -> if (c.remote && c.path.isNotEmpty()) "${c.path.replaceFirstChar { it.uppercase() }} · ${c.rttMs} ms" else null
    "reconnecting" -> "Reconnecting…"
    "connecting", "pairing" -> "Connecting…"
    else -> c.state.replaceFirstChar { it.uppercase() }
}

/** The server in use, as the app bar title; tap to switch or add one. */
@Composable
fun ServerMenu(core: Core, nav: Navigator) {
    val c by core.view.connection.watch()
    var open by remember { mutableStateOf(false) }
    Box {
        Row(
            Modifier.clickable { open = true }.padding(vertical = Space.xs, horizontal = Space.xs),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Column {
                Text(c.environmentLabel.ifEmpty { "Domovoi" }, style = MaterialTheme.typography.titleLarge, maxLines = 1, overflow = TextOverflow.Ellipsis)
                routeLine(c)?.let { Text(it, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant) }
            }
            Icon(Icons.Outlined.ArrowDropDown, "Switch server")
        }
        DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
            c.environments.forEach { env ->
                DropdownMenuItem(
                    text = {
                        Column {
                            Text(env.label)
                            Text(env.detail, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1)
                        }
                    },
                    leadingIcon = { Icon(if (env.remote) Icons.Outlined.Public else Icons.Outlined.Computer, null) },
                    trailingIcon = { if (env.active) Icon(Icons.Filled.Check, "In use", tint = MaterialTheme.colorScheme.primary, modifier = Modifier.size(20.dp)) },
                    onClick = { open = false; if (!env.active) core.update(Event.SelectEnvironment(env.id)) },
                )
            }
            HorizontalDivider()
            DropdownMenuItem(
                text = { Text("Add server") },
                leadingIcon = { Icon(Icons.Outlined.Add, null) },
                onClick = { open = false; nav.open(Screen.AddServer) },
            )
        }
    }
}
