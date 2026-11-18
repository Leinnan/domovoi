package com.crayen.domovoi.ui.terminal

import android.content.Context
import android.text.InputType
import android.view.KeyEvent
import android.view.View
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputMethodManager
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectTransformGestures
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowLeft
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowRight
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.ContentPaste
import androidx.compose.material.icons.outlined.Keyboard
import androidx.compose.material.icons.outlined.KeyboardArrowDown
import androidx.compose.material.icons.outlined.KeyboardArrowUp
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material.icons.outlined.Terminal
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.InputChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.app.TerminalLineView
import com.crayen.domovoi.core.Core
import com.crayen.domovoi.ui.common.EmptyState
import com.crayen.domovoi.ui.common.clipboardText
import com.crayen.domovoi.ui.common.watch
import com.crayen.domovoi.ui.theme.Mono
import com.crayen.domovoi.ui.theme.Space
import com.crayen.domovoi.ui.tools.ToolScaffold

/** xterm's 256-colour palette (16 ANSI, 6×6×6 cube, 24 greys) and true colour (`TerminalColors.swift`). */
fun terminalColor(value: Int): Color? {
    if (value < 0) return null
    if (value >= 0x1000000) {
        val rgb = value and 0xFFFFFF
        return Color((rgb shr 16) and 0xFF, (rgb shr 8) and 0xFF, rgb and 0xFF)
    }
    val ansi = arrayOf(
        floatArrayOf(0f, 0f, 0f), floatArrayOf(0.80f, 0.19f, 0.19f), floatArrayOf(0.05f, 0.74f, 0.47f), floatArrayOf(0.90f, 0.90f, 0.06f),
        floatArrayOf(0.14f, 0.45f, 0.78f), floatArrayOf(0.74f, 0.25f, 0.74f), floatArrayOf(0.07f, 0.66f, 0.80f), floatArrayOf(0.90f, 0.90f, 0.90f),
        floatArrayOf(0.40f, 0.40f, 0.40f), floatArrayOf(0.95f, 0.30f, 0.30f), floatArrayOf(0.14f, 0.82f, 0.55f), floatArrayOf(0.96f, 0.96f, 0.26f),
        floatArrayOf(0.23f, 0.56f, 0.92f), floatArrayOf(0.84f, 0.44f, 0.84f), floatArrayOf(0.16f, 0.72f, 0.86f), floatArrayOf(1f, 1f, 1f),
    )
    if (value < 16) ansi[value].let { return Color(it[0], it[1], it[2]) }
    if (value < 232) {
        val n = value - 16
        fun level(v: Int) = if (v == 0) 0f else (v * 40 + 55) / 255f
        return Color(level(n / 36), level((n / 6) % 6), level(n % 6))
    }
    val gray = ((value - 232) * 10 + 8) / 255f
    return Color(gray, gray, gray)
}

private val TerminalBackground = Color(0xFF111316)
private val TerminalForeground = Color(0xFFE3E5E8)

private fun line(line: TerminalLineView): AnnotatedString = buildAnnotatedString {
    for (span in line.spans) {
        var fg = terminalColor(span.fg)
        var bg = terminalColor(span.bg)
        if (span.inverse) { val t = fg; fg = bg ?: TerminalBackground; bg = t ?: TerminalForeground }
        withStyle(
            SpanStyle(
                color = fg ?: TerminalForeground,
                background = bg ?: Color.Unspecified,
                fontWeight = if (span.bold) FontWeight.Bold else null,
                fontStyle = if (span.italic) FontStyle.Italic else null,
                textDecoration = if (span.underline) TextDecoration.Underline else null,
            ),
        ) { append(span.text) }
    }
}

/**
 * The server's terminals. The core emulates the terminal; this draws its
 * grid, sends keys (soft keyboard, hardware keyboard, the extra-keys row)
 * and the size that fits.
 */
@Composable
fun TerminalScreen(core: Core, onBack: (() -> Unit)?) {
    val panel by core.view.panel.watch()
    val terminal = panel.terminal
    val screen = terminal.active
    val context = LocalContext.current
    var scale by rememberSaveable { mutableFloatStateOf(1f) }
    var ctrl by remember { mutableStateOf(false) }
    var menu by remember { mutableStateOf(false) }
    var input by remember { mutableStateOf<TerminalInputView?>(null) }
    val fontSize = (13f * scale).sp
    val measurer = rememberTextMeasurer()
    val density = LocalDensity.current
    val cell = remember(fontSize) { measurer.measure("M", TextStyle(fontFamily = Mono, fontSize = fontSize)).size }
    var viewport by remember { mutableStateOf(IntSize.Zero) }
    val sendKey: (String, String) -> Unit = { key, text ->
        core.update(Event.TerminalKey(key, text, ctrl, false))
        if (ctrl) ctrl = false
    }
    val currentSend by rememberUpdatedState(sendKey)

    LaunchedEffect(Unit) { core.update(Event.ShowPanel("terminal")) }
    LaunchedEffect(viewport, cell, screen?.id) {
        val s = screen ?: return@LaunchedEffect
        if (viewport.width == 0 || cell.width == 0) return@LaunchedEffect
        val pad = with(density) { 12.dp.roundToPx() }
        val cols = maxOf(20, (viewport.width - pad) / cell.width).toUInt()
        val rows = maxOf(4, (viewport.height - pad) / cell.height).toUInt()
        if (cols != s.cols || rows != s.rows) core.update(Event.TerminalResize(rows, cols))
    }

    ToolScaffold("Terminal", onBack, actions = {
        IconButton(onClick = { input?.showKeyboard() }) { Icon(Icons.Outlined.Keyboard, "Keyboard") }
        IconButton(onClick = { core.update(Event.NewTerminal) }) { Icon(Icons.Outlined.Add, "New terminal") }
        Box {
            IconButton(onClick = { menu = true }) { Icon(Icons.Outlined.MoreVert, "Terminal actions") }
            DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                DropdownMenuItem(text = { Text("Clear") }, onClick = { menu = false; core.update(Event.ClearTerminal) })
                DropdownMenuItem(text = { Text("Larger text") }, enabled = scale < 2f, onClick = { scale = (scale + 0.1f).coerceAtMost(2f) })
                DropdownMenuItem(text = { Text("Smaller text") }, enabled = scale > 0.6f, onClick = { scale = (scale - 0.1f).coerceAtLeast(0.6f) })
                if (scale != 1f) DropdownMenuItem(text = { Text("Default size") }, onClick = { scale = 1f })
                screen?.let { s ->
                    HorizontalDivider()
                    DropdownMenuItem(text = { Text("Close terminal", color = MaterialTheme.colorScheme.error) }, onClick = { menu = false; core.update(Event.CloseTerminal(s.id)) })
                }
            }
        }
    }) { padding ->
        Column(Modifier.fillMaxSize().padding(top = padding.calculateTopPadding()).navigationBarsPadding().imePadding()) {
            if (terminal.tabs.size > 1) {
                Row(Modifier.horizontalScroll(rememberScrollState()).padding(horizontal = Space.m, vertical = Space.xs), horizontalArrangement = Arrangement.spacedBy(Space.s)) {
                    terminal.tabs.forEach { tab ->
                        FilterChip(selected = tab.active, onClick = { core.update(Event.SelectTerminal(tab.id)) }, label = { Text(tab.label + if (tab.running) "" else " (exited)") })
                    }
                }
            }
            Box(
                Modifier.weight(1f).fillMaxWidth().background(TerminalBackground)
                    .onSizeChanged { viewport = it }
                    .pointerInput(Unit) { detectTransformGestures { _, _, zoom, _ -> if (zoom != 1f) scale = (scale * zoom).coerceIn(0.6f, 2f) } },
            ) {
                if (screen == null) {
                    EmptyState(Icons.Outlined.Terminal, "No terminal", "Open one to run commands on the server.") {
                        TextButton(onClick = { core.update(Event.NewTerminal) }) { Text("New terminal") }
                    }
                } else {
                    Box(Modifier.padding(6.dp)) {
                        Column {
                            screen.lines.forEach { l ->
                                Text(line(l), fontFamily = Mono, fontSize = fontSize, lineHeight = fontSize, softWrap = false, maxLines = 1, style = TextStyle(color = TerminalForeground))
                            }
                        }
                        if (screen.cursorVisible && !screen.exited) {
                            with(density) {
                                Box(
                                    Modifier.offset(x = (screen.cursorCol.toInt() * cell.width).toDp(), y = (screen.cursorRow.toInt() * cell.height).toDp())
                                        .size(cell.width.toDp(), cell.height.toDp())
                                        .background(TerminalForeground.copy(alpha = 0.55f)),
                                )
                            }
                        }
                    }
                    if (screen.exited) {
                        Surface(color = MaterialTheme.colorScheme.surfaceContainerHigh, shape = MaterialTheme.shapes.small, modifier = Modifier.align(Alignment.BottomCenter).padding(Space.m)) {
                            Text(screen.status.ifEmpty { "Exited" }, modifier = Modifier.padding(horizontal = Space.m, vertical = Space.xs))
                        }
                    }
                }
                // The keyboard target: taps show the keyboard; keys go to the terminal.
                AndroidView(
                    factory = { ctx ->
                        TerminalInputView(ctx) { key, text -> currentSend(key, text) }.also {
                            it.sendWith = { text, c, a -> core.update(Event.TerminalKey("char", text, c, a)) }
                            input = it
                        }
                    },
                    modifier = Modifier.fillMaxSize(),
                )
            }
            ExtraKeys(
                ctrl = ctrl,
                onCtrl = { ctrl = !ctrl },
                send = { key -> sendKey(key, "") },
                paste = { clipboardText(context)?.let { core.update(Event.TerminalPaste(it)) } },
            )
        }
    }
}

/** Esc, a sticky Ctrl, Tab, arrows and Paste: what a phone keyboard lacks. */
@Composable
private fun ExtraKeys(ctrl: Boolean, onCtrl: () -> Unit, send: (String) -> Unit, paste: () -> Unit) {
    Surface(color = MaterialTheme.colorScheme.surfaceContainer) {
        Row(Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = Space.s, vertical = Space.xs), horizontalArrangement = Arrangement.spacedBy(Space.xs), verticalAlignment = Alignment.CenterVertically) {
            TextButton(onClick = { send("escape") }) { Text("Esc") }
            FilterChip(selected = ctrl, onClick = onCtrl, label = { Text("Ctrl") })
            TextButton(onClick = { send("tab") }) { Text("Tab") }
            IconButton(onClick = { send("left") }) { Icon(Icons.AutoMirrored.Outlined.KeyboardArrowLeft, "Left") }
            IconButton(onClick = { send("down") }) { Icon(Icons.Outlined.KeyboardArrowDown, "Down") }
            IconButton(onClick = { send("up") }) { Icon(Icons.Outlined.KeyboardArrowUp, "Up") }
            IconButton(onClick = { send("right") }) { Icon(Icons.AutoMirrored.Outlined.KeyboardArrowRight, "Right") }
            IconButton(onClick = paste) { Icon(Icons.Outlined.ContentPaste, "Paste") }
        }
    }
}

/**
 * An invisible view that takes the keyboard. It asks for raw keys
 * (`TYPE_NULL`), so the soft keyboard sends key events without
 * autocorrection; text an input method commits anyway is sent as typed.
 */
class TerminalInputView(context: Context, private val send: (key: String, text: String) -> Unit) : View(context) {
    init {
        isFocusable = true
        isFocusableInTouchMode = true
        setOnClickListener { showKeyboard() }
    }

    fun showKeyboard() {
        requestFocus()
        context.getSystemService(InputMethodManager::class.java)?.showSoftInput(this, InputMethodManager.SHOW_IMPLICIT)
    }

    override fun onCheckIsTextEditor() = true

    override fun onCreateInputConnection(outAttrs: EditorInfo): InputConnection {
        outAttrs.inputType = InputType.TYPE_NULL
        outAttrs.imeOptions = EditorInfo.IME_FLAG_NO_FULLSCREEN or EditorInfo.IME_FLAG_NO_EXTRACT_UI
        return object : BaseInputConnection(this, false) {
            override fun commitText(text: CharSequence, newCursorPosition: Int): Boolean {
                text.toString().forEach { c -> if (c == '\n') send("enter", "") else send("char", c.toString()) }
                return true
            }
            override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
                repeat(beforeLength.coerceAtLeast(1)) { send("backspace", "") }
                return true
            }
        }
    }

    override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean {
        val key = when (keyCode) {
            KeyEvent.KEYCODE_ENTER, KeyEvent.KEYCODE_NUMPAD_ENTER -> "enter"
            KeyEvent.KEYCODE_DEL -> "backspace"
            KeyEvent.KEYCODE_FORWARD_DEL -> "delete"
            KeyEvent.KEYCODE_TAB -> if (event.isShiftPressed) "backtab" else "tab"
            KeyEvent.KEYCODE_ESCAPE -> "escape"
            KeyEvent.KEYCODE_DPAD_UP -> "up"
            KeyEvent.KEYCODE_DPAD_DOWN -> "down"
            KeyEvent.KEYCODE_DPAD_LEFT -> "left"
            KeyEvent.KEYCODE_DPAD_RIGHT -> "right"
            KeyEvent.KEYCODE_MOVE_HOME -> "home"
            KeyEvent.KEYCODE_MOVE_END -> "end"
            KeyEvent.KEYCODE_PAGE_UP -> "pageup"
            KeyEvent.KEYCODE_PAGE_DOWN -> "pagedown"
            else -> null
        }
        if (key != null) { send(key, ""); return true }
        // Ctrl and Alt combinations from a hardware keyboard.
        val ch = event.getUnicodeChar(event.metaState and (KeyEvent.META_CTRL_MASK or KeyEvent.META_ALT_MASK).inv())
        if (ch != 0) {
            val text = String(Character.toChars(ch))
            if (event.isCtrlPressed || event.isAltPressed) {
                sendWith(text, event.isCtrlPressed, event.isAltPressed)
            } else send("char", text)
            return true
        }
        return super.onKeyDown(keyCode, event)
    }

    /** Ctrl/Alt from a hardware keyboard bypass the sticky Ctrl key. */
    var sendWith: (String, Boolean, Boolean) -> Unit = { text, _, _ -> send("char", text) }
}
