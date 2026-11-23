package com.crayen.domovoi.ui.theme

import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Shapes
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp

/** Spacing on a 4 dp grid (`Theme.swift` `Space`). */
object Space {
    val xxs = 2.dp
    val xs = 4.dp
    val s = 8.dp
    val m = 12.dp
    val l = 16.dp
    val xl = 24.dp
    val xxl = 32.dp
    /** The widest a column of reading text gets. */
    val column = 840.dp
}

/** Colours by meaning, beside the Material roles (`Theme.swift` `Tint`). */
@Immutable
data class Tints(
    val plan: Color,
    val onPlan: Color,
    val planContainer: Color,
    val caution: Color,
    val success: Color,
    val question: Color,
    /** Claude Code, Codex, Pi and Antigravity badges. */
    val claude: Color,
    val codex: Color,
    val pi: Color,
    val antigravity: Color,
)

private val LightTints = Tints(
    plan = Color(0xFF7A4FD0), onPlan = Color.White, planContainer = Color(0xFFEDE3FF),
    caution = Color(0xFFB45F06), success = Color(0xFF2E7D32), question = Color(0xFF1E66D0),
    claude = Color(0xFFC2622D), codex = Color(0xFF0F8A7E), pi = Color(0xFFC2185B), antigravity = Color(0xFF3B5BDB),
)
private val DarkTints = Tints(
    plan = Color(0xFFC6AEFF), onPlan = Color(0xFF2C1060), planContainer = Color(0xFF3C2A63),
    caution = Color(0xFFFFB870), success = Color(0xFF8AD48F), question = Color(0xFF9CC2FF),
    claude = Color(0xFFF0A273), codex = Color(0xFF6ED6CA), pi = Color(0xFFF48FB1), antigravity = Color(0xFF9DB2FF),
)

val LocalTints = staticCompositionLocalOf { LightTints }

/** Code, commands and the terminal. */
val Mono = FontFamily.Monospace

// The fallback scheme (Android 11 and earlier, or dynamic colour off): a
// deep teal, Domovoi's accent.
private val LightScheme = lightColorScheme(
    primary = Color(0xFF00696E), onPrimary = Color.White,
    primaryContainer = Color(0xFF9CF0F5), onPrimaryContainer = Color(0xFF002022),
    secondary = Color(0xFF4A6365), secondaryContainer = Color(0xFFCCE8E9),
    tertiary = Color(0xFF4F5E7D), tertiaryContainer = Color(0xFFD7E2FF),
)
private val DarkScheme = darkColorScheme(
    primary = Color(0xFF80D4D9), onPrimary = Color(0xFF003739),
    primaryContainer = Color(0xFF004F53), onPrimaryContainer = Color(0xFF9CF0F5),
    secondary = Color(0xFFB0CBCD), secondaryContainer = Color(0xFF324B4D),
    tertiary = Color(0xFFB7C6EA), tertiaryContainer = Color(0xFF374764),
)

val DomovoiShapes = Shapes(
    extraSmall = RoundedCornerShape(6.dp),
    small = RoundedCornerShape(8.dp),
    medium = RoundedCornerShape(16.dp),
    large = RoundedCornerShape(20.dp),
    extraLarge = RoundedCornerShape(28.dp),
)

@Composable
fun DomovoiTheme(dark: Boolean = isSystemInDarkTheme(), dynamic: Boolean = true, content: @Composable () -> Unit) {
    val context = LocalContext.current
    val scheme: ColorScheme = when {
        dynamic && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S -> if (dark) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
        dark -> DarkScheme
        else -> LightScheme
    }
    CompositionLocalProvider(LocalTints provides if (dark) DarkTints else LightTints) {
        MaterialTheme(colorScheme = scheme, shapes = DomovoiShapes, content = content)
    }
}
