package com.crayen.domovoi.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.crayen.domovoi.app.ConnectionView
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.core.Core
import com.crayen.domovoi.ui.nav.MainScaffold
import com.crayen.domovoi.ui.pairing.LinkRequestDialog
import com.crayen.domovoi.ui.pairing.PairingScreen
import com.crayen.domovoi.ui.pairing.ServerChangedScreen
import com.crayen.domovoi.ui.theme.Space

/** The app proper shows once a server is active and paired (`RootView.showMain`). */
fun showMain(connection: ConnectionView): Boolean = when (connection.state) {
    "connected", "reconnecting" -> true
    "connecting", "disconnected" -> connection.activeEnvironmentId.isNotEmpty() && !connection.needsPairing
    else -> false
}

/**
 * The screen for the connection's state, and over it the question a link from
 * outside the app asks (`ConnectionView.linkRequest`): it can arrive on any
 * screen, also at a cold start before a server is paired.
 */
@Composable
fun AppRoot(core: Core) {
    val connection by core.view.connection.collectAsStateWithLifecycle()
    when {
        connection.serverChanged -> ServerChangedScreen(core, connection)
        showMain(connection) -> MainScaffold(core)
        else -> PairingScreen(core, onboarding = true)
    }
    connection.linkRequest?.let { request ->
        LinkRequestDialog(request) { accept -> core.update(Event.AnswerLinkRequest(request.id, accept)) }
    }
}
