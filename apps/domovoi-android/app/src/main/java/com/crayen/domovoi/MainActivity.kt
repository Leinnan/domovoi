package com.crayen.domovoi

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.mutableStateOf
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.core.Notifier
import com.crayen.domovoi.system.AppRequest
import com.crayen.domovoi.system.AppRequests
import com.crayen.domovoi.ui.thread.Images
import androidx.core.content.IntentCompat
import androidx.lifecycle.lifecycleScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import com.crayen.domovoi.ui.AppRoot
import com.crayen.domovoi.ui.common.LocalCore
import com.crayen.domovoi.ui.theme.DomovoiTheme

class MainActivity : ComponentActivity() {
    private val core by lazy { DomovoiApp.core(this) }

    /** A `domovoi://connect` link that would leave the server in use, waiting for a yes. */
    private val pendingLink = mutableStateOf<String?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        if (savedInstanceState == null) handle(intent)
        if (savedInstanceState == null) com.crayen.domovoi.system.UnifiedPushService.start(this)
        setContent {
            DomovoiTheme {
                CompositionLocalProvider(LocalCore provides core) {
                    AppRoot(
                        core = core,
                        pendingLink = pendingLink.value,
                        onConfirmLink = { link ->
                            pendingLink.value = null
                            if (link != null) core.update(Event.ConnectRemote(link))
                        },
                    )
                }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handle(intent)
    }

    private fun handle(intent: Intent?) {
        intent ?: return
        if (intent.action == Intent.ACTION_SEND || intent.action == Intent.ACTION_SEND_MULTIPLE) {
            share(intent)
            return
        }
        intent.getStringExtra(Notifier.EXTRA_THREAD)?.let { thread ->
            core.openThread(thread, intent.getStringExtra(Notifier.EXTRA_ENVIRONMENT).orEmpty())
            return
        }
        val uri = intent.data ?: return
        if (uri.scheme != "domovoi") return
        when (uri.host) {
            "thread" -> uri.getQueryParameter("id")?.let { core.openThread(it, uri.getQueryParameter("server").orEmpty()) }
            "new" -> AppRequests.post(AppRequest.NewThread())
            "connect" -> {
                val link = uri.toString()
                val connection = core.view.connection.value
                // Already connected elsewhere: leaving that server takes a yes.
                if (connection.state == "connected" && connection.activeEnvironmentId.isNotEmpty()) pendingLink.value = link
                else core.update(Event.ConnectRemote(link))
            }
        }
    }

    /** Shared text, links and images: a new thread's draft, once the server's list is there. */
    private fun share(intent: Intent) {
        val text = listOfNotNull(intent.getStringExtra(Intent.EXTRA_SUBJECT), intent.getStringExtra(Intent.EXTRA_TEXT)).distinct().joinToString("\n\n")
        val uris = buildList {
            IntentCompat.getParcelableExtra(intent, Intent.EXTRA_STREAM, Uri::class.java)?.let { add(it) }
            IntentCompat.getParcelableArrayListExtra(intent, Intent.EXTRA_STREAM, Uri::class.java)?.let { addAll(it) }
        }.distinct().take(4)
        lifecycleScope.launch {
            val images = withContext(Dispatchers.IO) { uris.mapNotNull { Images.prepare(this@MainActivity, it) } }
            AppRequests.post(AppRequest.NewThread(text, images))
        }
    }

    companion object {
        fun threadIntent(context: Context, thread: String, environment: String): Intent =
            Intent(context, MainActivity::class.java)
                .setAction(Intent.ACTION_VIEW)
                .setData(Uri.parse("domovoi://thread?id=${Uri.encode(thread)}"))
                .putExtra(Notifier.EXTRA_THREAD, thread)
                .putExtra(Notifier.EXTRA_ENVIRONMENT, environment)
                .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
    }
}
