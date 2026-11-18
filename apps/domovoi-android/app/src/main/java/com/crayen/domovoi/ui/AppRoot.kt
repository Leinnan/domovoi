package com.crayen.domovoi.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.crayen.domovoi.app.ConnectionView
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.core.Core
import com.crayen.domovoi.ui.nav.MainScaffold
import com.crayen.domovoi.ui.pairing.PairingScreen
import com.crayen.domovoi.ui.pairing.ServerChangedScreen
import com.crayen.domovoi.ui.theme.Space

/** The app proper shows once a server is active and paired (`RootView.showMain`). */
fun showMain(connection: ConnectionView): Boolean = when (connection.state) {
    "connected", "reconnecting" -> true
    "connecting", "disconnected" -> connection.activeEnvironmentId.isNotEmpty() && !connection.needsPairing
    else -> false
}

@Composable
fun AppRoot(core: Core, pendingLink: String?, onConfirmLink: (String?) -> Unit) {
    val connection by core.view.connection.collectAsStateWithLifecycle()
    when {
        connection.serverChanged -> ServerChangedScreen(core, connection)
        showMain(connection) -> MainScaffold(core)
        else -> PairingScreen(core, onboarding = true)
    }
    if (pendingLink != null) {
        val label = connection.environments.firstOrNull { it.active }?.label ?: "this server"
        AlertDialog(
            onDismissRequest = { onConfirmLink(null) },
            title = { Text("Connect to another server?") },
            text = { Text("This leaves $label. You can switch back from the server menu.") },
            confirmButton = { TextButton(onClick = { onConfirmLink(pendingLink) }) { Text("Connect") } },
            dismissButton = { TextButton(onClick = { onConfirmLink(null) }) { Text("Cancel") } },
        )
    }
}
