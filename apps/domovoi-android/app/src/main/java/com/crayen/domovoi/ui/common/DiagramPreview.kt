package com.crayen.domovoi.ui.common

import android.graphics.BitmapFactory
import androidx.compose.foundation.Image
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.ui.theme.Mono
import com.crayen.domovoi.ui.theme.Space
import java.net.HttpURLConnection
import java.net.URL
import java.security.MessageDigest
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** A completed Mermaid block, drawn by the server (the core asks for it with
 *  `LoadDiagram`); its source when it can't be drawn. */
@Composable
fun DiagramPreview(source: String) {
    val dark = MaterialTheme.colorScheme.surface.luminance() < 0.5f
    val core = LocalCore.current
    // One per source and theme.
    val key = remember(source, dark) {
        val digest = MessageDigest.getInstance("SHA-256").digest(source.toByteArray()).joinToString("") { "%02x".format(it) }
        "${if (dark) "dark" else "light"}-$digest"
    }
    val diagrams = core?.view?.diagrams?.watch()?.value.orEmpty()
    val drawn = diagrams.firstOrNull { it.key == key }
    var image by remember(key) { mutableStateOf<ImageBitmap?>(null) }
    var loadFailed by remember(key) { mutableStateOf(false) }
    var sourceVisible by remember(source) { mutableStateOf(false) }
    var expanded by remember(source) { mutableStateOf(false) }
    val context = LocalContext.current
    // Asked for on appearing, and again when the core says so (the link
    // expired, or the connection came back).
    LaunchedEffect(key, drawn?.loadKey) {
        core?.update(Event.LoadDiagram(key, source, dark))
    }
    val url = drawn?.url
    LaunchedEffect(url) {
        if (url == null) return@LaunchedEffect
        loadFailed = false
        val loaded = withContext(Dispatchers.IO) {
            runCatching {
                val connection = URL(url).openConnection() as HttpURLConnection
                try {
                    if (connection.responseCode != 200) null
                    else connection.inputStream.use { BitmapFactory.decodeStream(it) }?.asImageBitmap()
                } finally {
                    connection.disconnect()
                }
            }.getOrNull()
        }
        if (loaded != null) image = loaded else loadFailed = true
    }
    val failed = core == null || drawn?.failed == true || (loadFailed && image == null)
    val offline = drawn?.offline == true && image == null
    Surface(shape = MaterialTheme.shapes.small, color = MaterialTheme.colorScheme.surfaceContainerLow) {
        Column(Modifier.fillMaxWidth().padding(Space.s)) {
            Row {
                if (image != null) TextButton(onClick = { expanded = true }) { Text("Open diagram") }
                TextButton(onClick = { sourceVisible = !sourceVisible }) { Text(if (sourceVisible) "View diagram" else "View source") }
                TextButton(onClick = { copyText(context, source) }) { Text("Copy source") }
            }
            when {
                sourceVisible || failed || offline -> {
                    if (failed) Text("This diagram couldn’t be rendered.")
                    else if (offline) Text("The diagram is drawn once the server is connected.")
                    Text(source, fontFamily = Mono, softWrap = false, modifier = Modifier.horizontalScroll(rememberScrollState()))
                }
                image != null -> Image(requireNotNull(image), "Mermaid diagram", Modifier.fillMaxWidth().heightIn(max = 360.dp))
                else -> CircularProgressIndicator(Modifier.size(24.dp))
            }
        }
    }
    val diagram = image
    if (expanded && diagram != null) {
        Dialog(onDismissRequest = { expanded = false }, properties = DialogProperties(usePlatformDefaultWidth = false)) {
            var zoom by remember { mutableFloatStateOf(1f) }
            Surface(Modifier.fillMaxSize()) {
                Column(Modifier.padding(Space.m)) {
                    Row {
                        TextButton(onClick = { zoom = (zoom / 1.4f).coerceAtLeast(1f) }) { Text("Zoom out") }
                        TextButton(onClick = { zoom = (zoom * 1.4f).coerceAtMost(4f) }) { Text("Zoom in") }
                        TextButton(onClick = { zoom = 1f }) { Text("Fit") }
                        TextButton(onClick = { expanded = false }) { Text("Done") }
                    }
                    BoxWithConstraints(Modifier.fillMaxSize()) {
                        val width = maxWidth
                        Box(Modifier.fillMaxSize().horizontalScroll(rememberScrollState()).verticalScroll(rememberScrollState())) {
                            Image(diagram, "Mermaid diagram", Modifier.width(width * zoom).aspectRatio(diagram.width.toFloat() / diagram.height))
                        }
                    }
                }
            }
        }
    }
}
