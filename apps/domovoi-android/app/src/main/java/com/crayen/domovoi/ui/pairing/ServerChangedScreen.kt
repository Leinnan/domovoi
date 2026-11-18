package com.crayen.domovoi.ui.pairing

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.SyncProblem
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.crayen.domovoi.app.ConnectionView
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.core.Core
import com.crayen.domovoi.ui.theme.LocalTints
import com.crayen.domovoi.ui.theme.Space

/** The server refuses this device for good: pair again, or forget it. */
@Composable
fun ServerChangedScreen(core: Core, connection: ConnectionView) {
    var pairing by rememberSaveable { mutableStateOf(false) }
    if (pairing) {
        PairingScreen(core, onboarding = false, onBack = { pairing = false })
        return
    }
    val label = connection.environments.firstOrNull { it.id == connection.activeEnvironmentId }?.label ?: "This server"
    Scaffold { padding ->
        Box(Modifier.fillMaxSize().padding(padding), contentAlignment = Alignment.Center) {
            Column(
                Modifier.widthIn(max = 420.dp).padding(Space.xl),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(Space.l),
            ) {
                Icon(Icons.Outlined.SyncProblem, contentDescription = null, tint = LocalTints.current.caution, modifier = Modifier.size(48.dp))
                Text("$label no longer accepts this device", style = MaterialTheme.typography.titleLarge, textAlign = TextAlign.Center)
                connection.error?.let {
                    Text(it, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, textAlign = TextAlign.Center)
                }
                Button(onClick = { pairing = true }, modifier = Modifier.fillMaxWidth()) { Text("Pair again") }
                OutlinedButton(
                    onClick = { core.update(Event.ForgetEnvironment(connection.activeEnvironmentId)) },
                    colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
                    modifier = Modifier.fillMaxWidth(),
                ) { Text("Forget $label") }
            }
        }
    }
}
