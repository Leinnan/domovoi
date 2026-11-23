package com.crayen.domovoi.ui.common

import androidx.compose.foundation.background
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.AccountTree
import androidx.compose.material.icons.outlined.AutoAwesome
import androidx.compose.material.icons.outlined.Build
import androidx.compose.material.icons.outlined.Cloud
import androidx.compose.material.icons.outlined.Code
import androidx.compose.material.icons.outlined.Computer
import androidx.compose.material.icons.outlined.Dns
import androidx.compose.material.icons.outlined.Folder
import androidx.compose.material.icons.outlined.Games
import androidx.compose.material.icons.outlined.Image
import androidx.compose.material.icons.outlined.Language
import androidx.compose.material.icons.outlined.Layers
import androidx.compose.material.icons.outlined.LocalShipping
import androidx.compose.material.icons.outlined.Memory
import androidx.compose.material.icons.outlined.Message
import androidx.compose.material.icons.outlined.MusicNote
import androidx.compose.material.icons.outlined.PhoneAndroid
import androidx.compose.material.icons.outlined.RocketLaunch
import androidx.compose.material.icons.outlined.Science
import androidx.compose.material.icons.outlined.Shield
import androidx.compose.material.icons.outlined.ShoppingBag
import androidx.compose.material.icons.outlined.Source
import androidx.compose.material.icons.outlined.Storage
import androidx.compose.material.icons.outlined.Terminal
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import coil3.compose.SubcomposeAsyncImage
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.app.ProjectIconView
import com.crayen.domovoi.core.Core

internal val projectSymbols = linkedMapOf(
    "folder-code" to Icons.Outlined.Folder, "code-2" to Icons.Outlined.Code,
    "terminal" to Icons.Outlined.Terminal, "globe-2" to Icons.Outlined.Language,
    "server" to Icons.Outlined.Dns, "database" to Icons.Outlined.Storage,
    "bot" to Icons.Outlined.Memory, "sparkles" to Icons.Outlined.AutoAwesome,
    "smartphone" to Icons.Outlined.PhoneAndroid, "monitor" to Icons.Outlined.Computer,
    "cloud-cog" to Icons.Outlined.Cloud, "package" to Icons.Outlined.LocalShipping,
    "book-open" to Icons.Outlined.Source, "flask-conical" to Icons.Outlined.Science,
    "shield-check" to Icons.Outlined.Shield, "rocket" to Icons.Outlined.RocketLaunch,
    "gamepad-2" to Icons.Outlined.Games, "music" to Icons.Outlined.MusicNote,
    "image" to Icons.Outlined.Image, "shopping-bag" to Icons.Outlined.ShoppingBag,
    "git-branch" to Icons.Outlined.AccountTree, "workflow" to Icons.Outlined.AccountTree,
    "wrench" to Icons.Outlined.Build, "layers-3" to Icons.Outlined.Layers,
)
private val projectPalette = linkedMapOf(
    "gray" to (0x4b5563 to 0x9ca3af), "red" to (0x991b1b to 0xfca5a5), "orange" to (0x9a3412 to 0xfdba74),
    "amber" to (0x92400e to 0xfbbf24), "yellow" to (0x854d0e to 0xfacc15), "lime" to (0x3f6212 to 0xa3e635),
    "green" to (0x166534 to 0x4ade80), "emerald" to (0x065f46 to 0x34d399), "teal" to (0x115e59 to 0x2dd4bf),
    "cyan" to (0x155e75 to 0x22d3ee), "sky" to (0x075985 to 0x38bdf8), "blue" to (0x1e40af to 0x60a5fa),
    "indigo" to (0x4338ca to 0xa5b4fc), "violet" to (0x6d28d9 to 0xc4b5fd), "purple" to (0x7e22ce to 0xd8b4fe),
    "fuchsia" to (0xa21caf to 0xe879f9), "pink" to (0x9d174d to 0xf9a8d4), "rose" to (0x9f1239 to 0xfda4af),
)
internal val projectColors = projectPalette.keys.toList()

@Composable
internal fun projectColor(key: String): Color {
    val (light, dark) = projectPalette[key] ?: projectPalette.getValue("gray")
    return Color(0xff000000 or (if (isSystemInDarkTheme()) dark else light).toLong())
}

@Composable
fun ProjectIconTile(icon: ProjectIconView, core: Core, size: Dp = 28.dp) {
    val loadKey = icon.loadKey
    LaunchedEffect(loadKey) {
        if (loadKey != null) core.update(Event.LoadProjectIcon(icon.projectId))
    }
    val tint = projectColor(icon.color)
    val glyph: @Composable () -> Unit = {
        Box(Modifier.fillMaxSize().background(tint.copy(alpha = if (isSystemInDarkTheme()) 0.16f else 0.1f)), contentAlignment = Alignment.Center) {
            when (icon.kind) {
                "symbol" -> Icon(
                    if (icon.symbol == "message-square-dashed") Icons.Outlined.Message else projectSymbols[icon.symbol] ?: Icons.Outlined.Folder,
                    contentDescription = null, modifier = Modifier.size(size * 0.55f), tint = tint,
                )
                "emoji" -> Text(icon.text, fontSize = (size.value * 0.66f).sp, maxLines = 1)
                else -> Text(
                    icon.text, color = tint, fontSize = (size.value * 0.43f).sp,
                    fontFamily = FontFamily.Monospace, fontWeight = FontWeight.Bold, maxLines = 1,
                )
            }
        }
    }
    Box(Modifier.size(size).clip(RoundedCornerShape(size * 0.25f))) {
        if (icon.imageUrl == null) glyph()
        else SubcomposeAsyncImage(
            model = icon.imageUrl, contentDescription = null, contentScale = ContentScale.Fit,
            modifier = Modifier.fillMaxSize(), loading = { glyph() }, error = { glyph() },
        )
    }
}
