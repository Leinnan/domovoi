package com.crayen.domovoi.ui.common

import android.annotation.SuppressLint
import android.net.Uri
import android.webkit.JavascriptInterface
import android.webkit.RenderProcessGoneDetail
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.automirrored.outlined.Send
import androidx.compose.material.icons.outlined.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import com.crayen.domovoi.app.ActiveArtifactView
import com.crayen.domovoi.app.ArtifactsPanelView
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.app.ThreadPageView
import com.crayen.domovoi.core.Core
import com.crayen.domovoi.ffi.artifactPreviewDocument
import com.crayen.domovoi.ui.theme.Mono
import com.crayen.domovoi.ui.theme.Space
import java.io.ByteArrayInputStream
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

internal fun artifactIcon(kind: String): ImageVector = when (kind.lowercase()) {
    "react" -> Icons.Outlined.Code
    "svg" -> Icons.Outlined.Draw
    "mermaid" -> Icons.Outlined.AccountTree
    "markdown" -> Icons.Outlined.Description
    "hosted" -> Icons.Outlined.Language
    "code" -> Icons.Outlined.Code
    else -> Icons.Outlined.Web
}

internal fun artifactLabel(kind: String): String = when (kind.lowercase()) {
    "react" -> "Interactive React"
    "svg" -> "SVG graphic"
    "mermaid" -> "Mermaid diagram"
    "markdown" -> "Document"
    "hosted" -> "Claude artifact"
    "code" -> "Code"
    else -> "Interactive HTML"
}

/** Compact inline card in the transcript representing a previewable artifact. */
@Composable
fun ArtifactCard(
    title: String,
    kind: String,
    version: UInt = 1u,
    streaming: Boolean = false,
    hostedUrl: String? = null,
    onOpen: () -> Unit,
    onViewCode: (() -> Unit)? = null,
) {
    val label = artifactLabel(kind)
    val subtitle = when {
        streaming -> "Generating $label…"
        version > 1u -> "$label · v$version"
        else -> label
    }
    OutlinedCard(
        modifier = Modifier.fillMaxWidth().clickable(onClick = onOpen),
        shape = MaterialTheme.shapes.medium,
    ) {
        Row(
            Modifier.padding(horizontal = Space.m, vertical = Space.s + Space.xxs),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.m),
        ) {
            Surface(
                color = MaterialTheme.colorScheme.primaryContainer,
                shape = MaterialTheme.shapes.small,
                modifier = Modifier.size(36.dp),
            ) {
                Box(contentAlignment = Alignment.Center) {
                    Icon(artifactIcon(kind), null, tint = MaterialTheme.colorScheme.onPrimaryContainer, modifier = Modifier.size(20.dp))
                }
            }
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text(
                    title.ifEmpty { label },
                    style = MaterialTheme.typography.titleSmall,
                    fontWeight = FontWeight.SemiBold,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Space.xs)) {
                    if (streaming) CircularProgressIndicator(Modifier.size(12.dp), strokeWidth = 1.5.dp)
                    Text(
                        if (hostedUrl != null) "$subtitle · claude.ai" else subtitle,
                        style = MaterialTheme.typography.labelMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
            if (onViewCode != null && !streaming) {
                IconButton(onClick = onViewCode) {
                    Icon(Icons.Outlined.Code, "View source", modifier = Modifier.size(18.dp))
                }
            }
            Icon(Icons.Outlined.ChevronRight, null, tint = MaterialTheme.colorScheme.outline, modifier = Modifier.size(18.dp))
        }
    }
}

/** Full-screen or tablet extra-pane screen hosting the unified ArtifactPane. */
@Composable
fun ArtifactsScreen(core: Core, onBack: () -> Unit) {
    val panel by core.view.panel.watch()
    val threadId by core.view.threadId.watch()
    val dark = isSystemInDarkTheme()
    LaunchedEffect(threadId) {
        if (panel.artifacts.active == null && panel.artifacts.items.isNotEmpty() && threadId != null) {
            core.update(Event.OpenArtifact(threadId!!, panel.artifacts.items.first().id, null, dark))
        }
    }
    Scaffold(
        topBar = {
            TopAppBar(
                navigationIcon = {
                    IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Outlined.ArrowBack, "Back") }
                },
                title = {
                    Text(
                        panel.artifacts.active?.title ?: "Artifacts",
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                },
            )
        },
    ) { padding ->
        ArtifactPane(
            core = core,
            artifacts = panel.artifacts,
            modifier = Modifier.fillMaxSize().padding(padding),
            close = onBack,
        )
    }
}

@Composable
fun ArtifactPane(
    core: Core,
    artifacts: ArtifactsPanelView,
    modifier: Modifier = Modifier,
    close: (() -> Unit)? = null,
    jumpToMessage: ((String) -> Unit)? = null,
) {
    val systemDark = isSystemInDarkTheme()
    var forceTheme by remember { mutableStateOf<Boolean?>(null) }
    val previewDark = forceTheme ?: systemDark
    var interactive by remember { mutableStateOf(true) }
    var runtimeError by remember { mutableStateOf<String?>(null) }
    var quickPrompt by remember { mutableStateOf("") }
    val threadId by core.view.threadId.watch()
    val context = LocalContext.current
    val uriHandler = LocalUriHandler.current

    val active = artifacts.active
    LaunchedEffect(active?.id, active?.version) { runtimeError = null }
    LaunchedEffect(previewDark) {
        if (active != null) core.update(Event.RefreshArtifactPreview(previewDark))
    }

    Column(modifier) {
        if (active == null) {
            if (artifacts.items.isEmpty()) {
                Box(Modifier.fillMaxSize().padding(Space.xl), contentAlignment = Alignment.Center) {
                    EmptyState(
                        Icons.Outlined.AutoAwesome,
                        "No artifacts yet",
                        "HTML dashboards, React components, SVG graphics, and Mermaid diagrams from this thread appear here.",
                    )
                }
            } else {
                LazyColumn(
                    Modifier.fillMaxSize(),
                    contentPadding = PaddingValues(Space.m),
                    verticalArrangement = Arrangement.spacedBy(Space.s),
                ) {
                    items(artifacts.items, key = { it.id }) { item ->
                        ArtifactCard(
                            title = item.title,
                            kind = item.kind,
                            version = item.latestVersion,
                            streaming = item.streaming,
                            hostedUrl = item.hostedUrl,
                            onOpen = {
                                threadId?.let { tid ->
                                    core.update(Event.OpenArtifact(tid, item.id, null, previewDark))
                                }
                            },
                        )
                    }
                }
            }
            return@Column
        }

        // Toolbar row: artifact picker, version picker, theme, sandbox, actions
        var artifactMenu by remember { mutableStateOf(false) }
        var versionMenu by remember { mutableStateOf(false) }
        var actionMenu by remember { mutableStateOf(false) }

        Row(
            Modifier.fillMaxWidth().padding(horizontal = Space.m, vertical = Space.xs),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.xs),
        ) {
            if (artifacts.items.size > 1) {
                Box(Modifier.weight(1f)) {
                    TextButton(onClick = { artifactMenu = true }) {
                        Text(active.title, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        Icon(Icons.Outlined.ArrowDropDown, null, modifier = Modifier.size(18.dp))
                    }
                    DropdownMenu(expanded = artifactMenu, onDismissRequest = { artifactMenu = false }) {
                        artifacts.items.forEach { item ->
                            DropdownMenuItem(
                                text = { Text(item.title, maxLines = 1, overflow = TextOverflow.Ellipsis) },
                                leadingIcon = { Icon(artifactIcon(item.kind), null) },
                                onClick = {
                                    artifactMenu = false
                                    threadId?.let { tid -> core.update(Event.OpenArtifact(tid, item.id, null, previewDark)) }
                                },
                            )
                        }
                    }
                }
            } else {
                Text(
                    active.title,
                    style = MaterialTheme.typography.titleSmall,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f),
                )
            }

            if (active.versions.size > 1) {
                Box {
                    AssistChip(
                        onClick = { versionMenu = true },
                        label = { Text("v${active.version}") },
                    )
                    DropdownMenu(expanded = versionMenu, onDismissRequest = { versionMenu = false }) {
                        active.versions.asReversed().forEach { ver ->
                            val latest = ver.version == active.versions.lastOrNull()?.version
                            val label = if (latest) "v${ver.version} (Latest)" else "v${ver.version}"
                            DropdownMenuItem(
                                text = { Text(if (ver.summary.isEmpty()) label else "$label — ${ver.summary}") },
                                onClick = {
                                    versionMenu = false
                                    core.update(Event.SelectArtifactVersion(ver.version, previewDark))
                                },
                            )
                        }
                    }
                }
            }

            IconButton(onClick = { forceTheme = !previewDark }) {
                Icon(if (previewDark) Icons.Outlined.DarkMode else Icons.Outlined.LightMode, "Toggle preview theme")
            }

            Box {
                IconButton(onClick = { actionMenu = true }) { Icon(Icons.Outlined.MoreVert, "Artifact actions") }
                DropdownMenu(expanded = actionMenu, onDismissRequest = { actionMenu = false }) {
                    if (active.source.isNotEmpty()) {
                        DropdownMenuItem(
                            text = { Text("Copy source") },
                            leadingIcon = { Icon(Icons.Outlined.ContentCopy, null) },
                            onClick = { actionMenu = false; copyText(context, active.source) },
                        )
                    }
                    if (active.kind in setOf("html", "svg")) {
                        DropdownMenuItem(
                            text = { Text(if (interactive) "Switch to static preview" else "Enable offline interactivity") },
                            onClick = { actionMenu = false; interactive = !interactive; runtimeError = null },
                        )
                    }
                    active.hostedUrl?.let { hosted ->
                        DropdownMenuItem(
                            text = { Text("Open on claude.ai") },
                            leadingIcon = { Icon(Icons.Outlined.OpenInBrowser, null) },
                            onClick = { actionMenu = false; runCatching { uriHandler.openUri(hosted) } },
                        )
                    }
                    if (jumpToMessage != null && active.itemId.isNotEmpty()) {
                        DropdownMenuItem(
                            text = { Text("Jump to message") },
                            onClick = { actionMenu = false; jumpToMessage(active.itemId.substringBefore(':')) },
                        )
                    }
                    DropdownMenuItem(
                        text = { Text("Reload preview") },
                        leadingIcon = { Icon(Icons.Outlined.Refresh, null) },
                        onClick = { actionMenu = false; runtimeError = null; core.update(Event.RefreshArtifactPreview(previewDark)) },
                    )
                }
            }
        }

        // Preview / Code Tabs
        val codeMode = active.mode == "code"
        TabRow(selectedTabIndex = if (codeMode) 1 else 0) {
            Tab(selected = !codeMode, onClick = { core.update(Event.SetArtifactViewMode("preview")) }, text = { Text("Preview") })
            Tab(selected = codeMode, onClick = { core.update(Event.SetArtifactViewMode("code")) }, text = { Text("Code") })
        }

        // Body
        Box(Modifier.weight(1f).fillMaxWidth()) {
            when {
                active.streaming && active.source.isEmpty() -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(Space.s)) {
                        CircularProgressIndicator()
                        Text("Generating ${active.title}…", color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                }
                active.loading -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    CircularProgressIndicator()
                }
                active.error != null -> Box(Modifier.fillMaxSize().padding(Space.l), contentAlignment = Alignment.Center) {
                    Text(active.error!!, color = MaterialTheme.colorScheme.error)
                }
                active.truncated -> Box(Modifier.fillMaxSize().padding(Space.l), contentAlignment = Alignment.Center) {
                    Text("This artifact is too large to preview.")
                }
                codeMode || active.streaming -> {
                    SelectionContainer(
                        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).horizontalScroll(rememberScrollState()).padding(Space.m),
                    ) {
                        Text(highlighted(active.spans, active.source), fontFamily = Mono, softWrap = false)
                    }
                }
                else -> ArtifactPreviewBody(active, interactive, previewDark) { runtimeError = it }
            }
        }

        // Runtime error banner ("Fix with Claude")
        runtimeError?.let { err ->
            if (!codeMode) {
                Surface(color = MaterialTheme.colorScheme.errorContainer, modifier = Modifier.fillMaxWidth()) {
                    Row(
                        Modifier.padding(horizontal = Space.m, vertical = Space.xs),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(Space.s),
                    ) {
                        Text(
                            err,
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onErrorContainer,
                            maxLines = 2,
                            overflow = TextOverflow.Ellipsis,
                            modifier = Modifier.weight(1f),
                        )
                        FilledTonalButton(onClick = {
                            core.update(Event.AskAboutArtifact(active.id, "Fix this runtime error in the preview:\n$err"))
                            runtimeError = null
                            close?.invoke()
                        }) { Text("Fix with Claude") }
                    }
                }
            }
        }

        // Quick-iterate bar
        Row(
            Modifier.fillMaxWidth().padding(horizontal = Space.m, vertical = Space.s),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.s),
        ) {
            OutlinedTextField(
                value = quickPrompt,
                onValueChange = { quickPrompt = it },
                placeholder = { Text("Ask Claude to update this artifact…") },
                singleLine = true,
                modifier = Modifier.weight(1f),
            )
            IconButton(
                enabled = quickPrompt.isNotBlank(),
                onClick = {
                    val text = quickPrompt.trim()
                    if (text.isNotEmpty()) {
                        core.update(Event.AskAboutArtifact(active.id, text))
                        quickPrompt = ""
                        close?.invoke()
                    }
                },
            ) {
                Icon(Icons.AutoMirrored.Outlined.Send, "Send update request")
            }
        }
    }
}

@Composable
private fun ArtifactPreviewBody(
    active: ActiveArtifactView,
    interactive: Boolean,
    dark: Boolean,
    onError: (String) -> Unit,
) {
    val uriHandler = LocalUriHandler.current
    when (active.kind) {
        "mermaid" -> {
            val url = active.previewUrl
            if (url != null) {
                key(url) { ServerArtifactWebView(url, onError) }
            } else {
                DiagramPreview(active.source)
            }
        }
        "markdown" -> {
            SelectionContainer(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(Space.l)) {
                Text(active.source, style = MaterialTheme.typography.bodyLarge)
            }
        }
        "hosted" -> {
            if (active.source.isNotEmpty()) {
                SandboxedArtifactHost("html", active.source, interactive, dark, onError)
            } else {
                Box(Modifier.fillMaxSize().padding(Space.l), contentAlignment = Alignment.Center) {
                    Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(Space.m)) {
                        Text("This artifact is hosted on claude.ai.", style = MaterialTheme.typography.bodyLarge)
                        active.hostedUrl?.let { url ->
                            Button(onClick = { runCatching { uriHandler.openUri(url) } }) { Text("Open on claude.ai") }
                        }
                    }
                }
            }
        }
        "react" -> {
            val url = active.previewUrl
            if (url != null) {
                key(url) { ServerArtifactWebView(url, onError) }
            } else {
                Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
            }
        }
        "html", "svg" -> {
            val bundleUrl = if (active.kind == "html" && active.path != null) active.previewUrl else null
            if (bundleUrl != null) {
                key(bundleUrl) { ServerArtifactWebView(bundleUrl, onError) }
            } else {
                SandboxedArtifactHost(active.kind, active.source, interactive, dark, onError)
            }
        }
        else -> {
            SelectionContainer(
                Modifier.fillMaxSize().verticalScroll(rememberScrollState()).horizontalScroll(rememberScrollState()).padding(Space.m),
            ) {
                Text(highlighted(active.spans, active.source), fontFamily = Mono, softWrap = false)
            }
        }
    }
}

@Composable
private fun SandboxedArtifactHost(kind: String, source: String, interactive: Boolean, dark: Boolean, onError: (String) -> Unit) {
    key(kind, source, interactive, dark) {
        val document by produceState<String?>(null, kind, source, interactive, dark) {
            value = withContext(Dispatchers.Default) { artifactPreviewDocument(kind, source, interactive, dark) }
        }
        val page = document
        when {
            page == null -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
            page.isEmpty() -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { Text("This file is too large to preview.") }
            else -> OfflineHtmlWebView(page, interactive, onError)
        }
    }
}

private class ArtifactErrorBridge(private val onError: (String) -> Unit) {
    @JavascriptInterface
    fun onError(message: String?, line: Int) {
        val text = message?.trim().orEmpty()
        if (text.isEmpty()) return
        val suffix = if (line > 0) " (line $line)" else ""
        onError("$text$suffix")
    }
}

@SuppressLint("SetJavaScriptEnabled")
@Composable
private fun OfflineHtmlWebView(document: String, interactive: Boolean, onError: (String) -> Unit = {}) {
    var failed by remember { mutableStateOf(false) }
    val latestError by rememberUpdatedState(onError)
    if (failed) {
        Column(Modifier.fillMaxSize(), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.Center) {
            Text("Preview unavailable")
            TextButton(onClick = { failed = false }) { Text("Try again") }
        }
        return
    }
    AndroidView(
        factory = { context ->
            WebView(context).apply {
                settings.javaScriptEnabled = interactive
                settings.javaScriptCanOpenWindowsAutomatically = false
                settings.setSupportMultipleWindows(false)
                settings.allowFileAccess = false
                settings.allowContentAccess = false
                settings.domStorageEnabled = false
                settings.blockNetworkLoads = true
                if (interactive) {
                    addJavascriptInterface(ArtifactErrorBridge { msg -> post { latestError(msg) } }, "DomovoiArtifact")
                }
                webViewClient = object : WebViewClient() {
                    override fun onRenderProcessGone(view: WebView, detail: RenderProcessGoneDetail): Boolean {
                        failed = true
                        return true
                    }

                    override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean {
                        val url = request.url.toString()
                        if (request.isForMainFrame) return interactive || !url.startsWith("about:blank#")
                        return url != "about:srcdoc" && !url.startsWith("about:srcdoc#")
                    }

                    override fun shouldInterceptRequest(view: WebView, request: WebResourceRequest): WebResourceResponse? {
                        return when (request.url.scheme) {
                            "about", "data", "blob" -> null
                            else -> WebResourceResponse("text/plain", "utf-8", ByteArrayInputStream(byteArrayOf()))
                        }
                    }
                }
                loadDataWithBaseURL(null, document, "text/html", "utf-8", null)
            }
        },
        modifier = Modifier.fillMaxSize(),
        onRelease = { it.destroy() },
    )
}

@SuppressLint("SetJavaScriptEnabled")
@Composable
private fun ServerArtifactWebView(url: String, onError: (String) -> Unit) {
    val parsed = remember(url) { Uri.parse(url) }
    val latestError by rememberUpdatedState(onError)
    var failed by remember { mutableStateOf(false) }
    if (failed) {
        Column(Modifier.fillMaxSize(), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.Center) {
            Text("Preview unavailable")
            TextButton(onClick = { failed = false }) { Text("Try again") }
        }
        return
    }
    AndroidView(
        factory = { context ->
            WebView(context).apply {
                settings.javaScriptEnabled = true
                settings.javaScriptCanOpenWindowsAutomatically = false
                settings.setSupportMultipleWindows(false)
                settings.allowFileAccess = false
                settings.allowContentAccess = false
                settings.domStorageEnabled = false
                addJavascriptInterface(ArtifactErrorBridge { msg -> post { latestError(msg) } }, "DomovoiArtifact")
                webViewClient = object : WebViewClient() {
                    override fun onRenderProcessGone(view: WebView, detail: RenderProcessGoneDetail): Boolean {
                        failed = true
                        return true
                    }

                    private fun allowed(target: Uri): Boolean {
                        if (target.scheme in setOf("about", "data", "blob")) return true
                        return target.host == parsed.host && target.port == parsed.port && (target.path?.startsWith("/api/assets/") == true)
                    }

                    override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean = !allowed(request.url)

                    override fun shouldInterceptRequest(view: WebView, request: WebResourceRequest): WebResourceResponse? {
                        return if (allowed(request.url)) null else WebResourceResponse("text/plain", "utf-8", ByteArrayInputStream(byteArrayOf()))
                    }
                }
                loadUrl(url)
            }
        },
        modifier = Modifier.fillMaxSize(),
        onRelease = { it.destroy() },
    )
}

// ---- Legacy compatibility helpers ----

@Composable
fun HtmlArtifactButton(source: String, title: String = "HTML artifact") {
    var opened by remember(source) { mutableStateOf(false) }
    TextButton(onClick = { opened = true }) { Text("Preview") }
    if (opened) {
        Dialog(onDismissRequest = { opened = false }, properties = DialogProperties(usePlatformDefaultWidth = false)) {
            Surface(Modifier.fillMaxSize()) {
                HtmlArtifactPreview(title, source) { opened = false }
            }
        }
    }
}

@Composable
private fun HtmlArtifactPreview(title: String, source: String, close: () -> Unit) {
    var sourceVisible by remember(source) { mutableStateOf(false) }
    var interactive by remember(source) { mutableStateOf(true) }
    val dark = isSystemInDarkTheme()
    val context = LocalContext.current
    Column(Modifier.fillMaxSize().safeDrawingPadding().padding(Space.m)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(title, style = MaterialTheme.typography.titleLarge, maxLines = 1, modifier = Modifier.weight(1f))
            TextButton(onClick = close) { Text("Done") }
        }
        TabRow(selectedTabIndex = if (sourceVisible) 1 else 0) {
            Tab(selected = !sourceVisible, onClick = { sourceVisible = false }, text = { Text("Preview") })
            Tab(selected = sourceVisible, onClick = { sourceVisible = true }, text = { Text("Source") })
        }
        if (sourceVisible) {
            TextButton(onClick = { copyText(context, source) }) { Text("Copy source") }
            SelectionContainer(Modifier.verticalScroll(rememberScrollState()).horizontalScroll(rememberScrollState())) {
                Text(source, fontFamily = Mono, softWrap = false)
            }
        } else {
            TextButton(onClick = { interactive = !interactive }) {
                Text(if (interactive) "Switch to static preview" else "Enable interactivity")
            }
            SandboxedArtifactHost("html", source, interactive, dark) {}
        }
    }
}

@Composable
fun ArtifactGallery(core: Core, thread: ThreadPageView, close: () -> Unit, jump: (String) -> Unit) {
    val panel by core.view.panel.watch()
    Dialog(
        onDismissRequest = close,
        properties = DialogProperties(usePlatformDefaultWidth = false),
    ) {
        Surface(Modifier.fillMaxSize()) {
            Column(Modifier.safeDrawingPadding()) {
                Row(
                    Modifier.fillMaxWidth().padding(horizontal = Space.m, vertical = Space.xs),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text("Artifacts", style = MaterialTheme.typography.titleLarge, modifier = Modifier.weight(1f))
                    TextButton(onClick = close) { Text("Done") }
                }
                ArtifactPane(core, panel.artifacts, Modifier.weight(1f), close = close, jumpToMessage = jump)
            }
        }
    }
}
