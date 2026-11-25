package com.crayen.domovoi.ui.common

import android.provider.Settings
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.lerp
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import com.crayen.domovoi.app.ActivityView
import com.crayen.domovoi.app.CompanionLook
import com.crayen.domovoi.app.ConnectionView
import com.crayen.domovoi.app.EnvironmentView
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.core.Core
import com.crayen.domovoi.ui.theme.Space
import kotlinx.coroutines.delay
import kotlin.math.PI
import kotlin.math.abs
import kotlin.math.floor
import kotlin.math.max
import kotlin.math.min
import kotlin.math.pow
import kotlin.math.sin

/** What the companion shows (`companion.rs` `Mood`, `CompanionMood.swift`). */
enum class CompanionMood(val label: String) {
    Offline("Offline"), Attention("Needs you"), Working("Working"), Finished("Finished"), Failed("Failed"), Idle("Idle"),
}

/**
 * A short reaction when a turn ends: happy for 1.2 s, a wince for 1.5 s (a
 * wince isn't cut short by a "finished" right after). Only a new beat on the
 * same server reacts, so connecting or switching servers stays quiet.
 */
class CompanionReaction {
    private var server = ""
    private var seq = 0uL
    var current: Pair<CompanionMood, Long>? = null
        private set

    /** After a view: the reaction to show, and until when (Unix ms). */
    fun observe(server: String, activity: ActivityView, nowMs: Long): Pair<CompanionMood, Long>? {
        val fresh = this.server == server && seq != activity.beatSeq && seq != 0uL
        this.server = server
        seq = activity.beatSeq
        if (!fresh) return null
        val wincing = current?.let { (mood, until) -> mood == CompanionMood.Failed && until > nowMs } == true
        val (mood, ms) = when {
            activity.beat == "failed" -> CompanionMood.Failed to 1500L
            activity.beat == "finished" && !wincing -> CompanionMood.Finished to 1200L
            else -> return null
        }
        return (mood to nowMs + ms).also { current = it }
    }
}

/** The server's mood: what its threads do, with a brief reaction to a turn that just ended. */
@Composable
fun companionMood(connection: ConnectionView): CompanionMood {
    val reaction = remember { CompanionReaction() }
    var shown by remember { mutableStateOf<CompanionMood?>(null) }
    val activity = connection.activity
    LaunchedEffect(connection.activeEnvironmentId, activity.beatSeq) {
        val now = System.currentTimeMillis()
        reaction.observe(connection.activeEnvironmentId, activity, now)
        val (mood, until) = reaction.current?.takeIf { it.second > now } ?: run { shown = null; return@LaunchedEffect }
        shown = mood
        delay(until - now)
        shown = null
    }
    return when {
        connection.state != "connected" -> CompanionMood.Offline
        activity.attention > 0u -> CompanionMood.Attention
        activity.working > 0u -> CompanionMood.Working
        else -> shown ?: CompanionMood.Idle
    }
}

/** Animations are off in the system settings. */
@Composable
private fun reducedMotion(): Boolean {
    val resolver = LocalContext.current.contentResolver
    return remember(resolver) { Settings.Global.getFloat(resolver, Settings.Global.ANIMATOR_DURATION_SCALE, 1f) == 0f }
}

private val Ink = Color(0.06f, 0.08f, 0.15f)
private val Gray = Color(0.52f, 0.53f, 0.55f)

/**
 * Domovoi's companion for a server (`companion.rs`, `CompanionPainter.swift`):
 * a round body in its colour, eyes that blink and look about, and a mood
 * from what its threads do. A tap pokes it; with `follow` on, its eyes
 * follow a finger held on it.
 */
@Composable
fun CompanionAvatar(
    look: CompanionLook,
    mood: CompanionMood,
    size: Dp,
    modifier: Modifier = Modifier,
    description: String? = null,
    /** Drawn at rest, without animation (small swatches). */
    still: Boolean = false,
    onPoke: (() -> Unit)? = null,
) {
    val dark = isSystemInDarkTheme()
    val body = projectColor(look.body)
    val pupil = if (look.pupil == "ink") Ink else projectColor(look.pupil)
    val resting = still || reducedMotion()
    val time = remember { mutableFloatStateOf(0f) }
    val moodSince = remember(mood) { mutableFloatStateOf(time.floatValue) }
    var poked by remember { mutableStateOf(false) }
    var touch by remember { mutableStateOf<Offset?>(null) }
    if (!resting && mood != CompanionMood.Offline) {
        // About 25 frames a second, paused with the app out of sight.
        LaunchedEffect(Unit) {
            val start = withFrameNanos { it }
            while (true) {
                delay(40)
                withFrameNanos { time.floatValue = (it - start) / 1e9f }
            }
        }
    }
    LaunchedEffect(poked) {
        if (poked) {
            delay(600)
            poked = false
        }
    }
    var m = modifier.size(size)
    if (description != null) m = m.semantics { contentDescription = "$description, ${mood.label.lowercase()}" }
    if (onPoke != null || !resting) {
        m = m.pointerInput(look.follow, onPoke) {
            awaitEachGesture {
                val down = awaitFirstDown()
                if (look.follow) touch = down.position
                var up = false
                while (!up) {
                    val event = awaitPointerEvent()
                    val change = event.changes.firstOrNull() ?: break
                    if (look.follow) touch = change.position
                    up = !change.pressed
                }
                touch = null
                poked = true
                onPoke?.invoke()
            }
        }
    }
    Canvas(m) {
        drawCompanion(
            look, mood, body, pupil, dark,
            t = if (resting) 0f else time.floatValue,
            since = if (resting) 0f else time.floatValue - moodSince.floatValue,
            poked = poked, touch = touch, still = resting,
        )
    }
}

private fun DrawScope.drawCompanion(
    look: CompanionLook, mood: CompanionMood, bodyColor: Color, pupilColor: Color, dark: Boolean,
    t: Float, since: Float, poked: Boolean, touch: Offset?, still: Boolean,
) {
    val s = min(size.width, size.height) / 100f
    val ox = (size.width - 100f * s) / 2f
    val oy = (size.height - 100f * s) / 2f
    val energy = when (look.energy) { "calm" -> 0.7f; "lively" -> 1.3f; else -> 1f }
    val offline = mood == CompanionMood.Offline

    // How it moves: a bob while working, a hop when finished, a shake when failed.
    var lift = 0f
    var shake = 0f
    if (!still) when (mood) {
        CompanionMood.Working -> lift = sin(t * energy * 6f) * 1.7f
        CompanionMood.Finished -> lift = sin(min(1f, since / 1.2f) * PI.toFloat()).pow(2) * 8f
        CompanionMood.Failed -> shake = sin(since * 30f) * 3f * max(0f, 1f - since)
        CompanionMood.Attention -> lift = sin(min(1f, (since % 3.8f) / 0.43f) * PI.toFloat()) * 4f
        else -> Unit
    }
    fun pt(x: Float, y: Float) = Offset(ox + (50f + x + shake) * s, oy + (55f + y - lift) * s)

    val body = if (offline) Gray.copy(alpha = 0.66f) else bodyColor
    val tint = lerp(body, Color.Black, 0.20f)
    val perk = when {
        mood == CompanionMood.Attention -> 4f
        mood == CompanionMood.Working -> sin(t * 3f * energy) * 2f + 1.5f
        poked -> 4.5f
        else -> 0f
    }

    // Ears
    when (look.ears) {
        "round" -> for (side in listOf(-1f, 1f)) {
            val c = pt(side * 28f, -24f - perk)
            drawCircle(tint, 11.5f * s, c)
            drawCircle(lerp(body, Color.White, 0.36f), 7f * s, c)
        }
        "pointy" -> for (side in listOf(-1f, 1f)) {
            val ear = Path().apply {
                pt(side * 14f, -27f).let { moveTo(it.x, it.y) }
                quad(pt(side * 29f, -48f - perk), pt(side * 34f, -40f - perk))
                quad(pt(side * 39f, -29f), pt(side * 36f, -17f))
                close()
            }
            drawPath(ear, tint)
        }
    }

    // Antenna
    if (look.antenna) {
        val ax = perk * 0.65f - 3f
        val ay = -43f - perk * 0.2f
        val stem = Path().apply {
            pt(0f, -32f).let { moveTo(it.x, it.y) }
            quad(pt(3f, ay + 5f), pt(ax, ay))
        }
        drawPath(stem, tint, style = Stroke(width = 3.6f * s, cap = StrokeCap.Round))
        drawCircle(tint, 5.5f * s, pt(ax, ay))
        val glow = when (mood) {
            CompanionMood.Working -> 0.5f + 0.25f * sin(t * energy * 6f)
            CompanionMood.Attention -> 0.75f
            CompanionMood.Finished -> 0.85f
            else -> 0.35f
        }
        drawCircle(lerp(body, Color.White, glow), 3.8f * s, pt(ax, ay - 0.6f))
    }

    // Body
    val center = pt(0f, 0f)
    val radius = 38.5f * s
    drawCircle(body, radius, center)
    drawCircle(Color.White.copy(alpha = if (dark) 0.16f else 0.28f), radius * 0.45f, pt(-11f, -14f))
    drawCircle(Color.White.copy(alpha = 0.30f), radius, center, style = Stroke(1.6f * s))

    // Where it looks: a finger held on it, else a drift (idle) or a sweep (working).
    val gaze = when {
        offline -> Offset.Zero
        look.follow && touch != null -> Offset(
            ((touch.x - center.x) / (size.width * 0.5f)).coerceIn(-1f, 1f),
            ((touch.y - center.y) / (size.height * 0.5f)).coerceIn(-1f, 1f),
        )
        still -> Offset.Zero
        mood == CompanionMood.Working -> Offset(sin(t * energy * 1.6f) * 0.8f, 0.34f)
        mood == CompanionMood.Idle -> Offset(sin(t * 1.1f) * 0.32f, sin(t * 0.7f) * 0.18f)
        else -> Offset.Zero
    }

    val (ew, eh) = when (look.eyeShape) {
        "oval" -> 15f to 20f
        "pill" -> 13f to 21f
        "sleepy" -> 17f to 13f
        else -> 17f to 17f
    }
    val pupilRatio = when (look.pupilSize) { "small" -> 0.44f; "large" -> 0.68f; else -> 0.56f }
    // A blink now and then, at the look's rate (`CompanionPose.swift`).
    val interval = when (look.blink) { "rare" -> 8f; "often" -> 2.5f; else -> 4.5f }
    val cycle = floor(t / interval)
    val blinkAt = interval * (0.4f + 0.3f * sin(cycle * 8.3f))
    val openness = if (still || offline) 1f else 1f - max(0f, 1f - abs(t % interval - blinkAt) / 0.11f)
    val line = Stroke(width = 3f * s, cap = StrokeCap.Round)

    for (side in listOf(-1f, 1f)) {
        val ex = side * 14f
        val ey = 1f
        when {
            offline -> drawPath(arc(pt(ex - 7f, ey), pt(ex, ey + 7f), pt(ex + 7f, ey)), Ink.copy(alpha = 0.8f), style = line)
            mood == CompanionMood.Failed -> drawPath(
                Path().apply {
                    pt(ex - 5f, ey - 5f).let { moveTo(it.x, it.y) }
                    pt(ex + 5f, ey + 5f).let { lineTo(it.x, it.y) }
                    pt(ex + 5f, ey - 5f).let { moveTo(it.x, it.y) }
                    pt(ex - 5f, ey + 5f).let { lineTo(it.x, it.y) }
                },
                Ink, style = line,
            )
            mood == CompanionMood.Finished || poked -> drawPath(arc(pt(ex - 7f, ey), pt(ex, ey - 10f), pt(ex + 7f, ey)), Ink, style = line)
            openness < 0.2f -> drawPath(arc(pt(ex - 7f, ey), pt(ex, ey + 3f), pt(ex + 7f, ey)), Ink, style = line)
            else -> {
                val h = eh * openness
                drawRoundRect(
                    Color.White.copy(alpha = 0.98f), topLeft = pt(ex - ew / 2f, ey - h / 2f), size = Size(ew * s, h * s),
                    cornerRadius = CornerRadius(min(ew, h) * 0.5f * s),
                )
                val d = min(ew, h) * pupilRatio
                val px = ex + gaze.x * (ew - d) * 0.4f
                val py = ey + gaze.y * max(0f, h - d) * 0.4f
                drawCircle(pupilColor, d * 0.5f * s, pt(px, py))
                drawCircle(Color.White, d * 0.14f * s, pt(px - d * 0.17f, py - d * 0.19f))
            }
        }

        if (look.eyebrows && !offline) {
            val level = ey - eh * 0.5f - 5f - perk * 0.45f
            val tilt = when (mood) { CompanionMood.Working -> 2f; CompanionMood.Attention -> -2f; else -> 0f }
            drawPath(
                arc(pt(ex - 4f, level + tilt * side * 0.5f), pt(ex, level - 2.6f), pt(ex + 4f, level - tilt * side * 0.5f)),
                Ink.copy(alpha = 0.72f), style = Stroke(width = 2f * s, cap = StrokeCap.Round),
            )
        }
    }
}

private fun Path.quad(control: Offset, end: Offset) = quadraticTo(control.x, control.y, end.x, end.y)

private fun arc(from: Offset, control: Offset, to: Offset) = Path().apply {
    moveTo(from.x, from.y)
    quad(control, to)
}

// ---- editor ----

private val eyeShapes = listOf("round", "oval", "pill", "sleepy")

/**
 * A server's companion, changed as it is picked (`CompanionEditor.swift`):
 * body and pupil colours, eyes, extras and personality, with a preview of
 * each mood. Reset goes back to the look derived from the server's key.
 */
@Composable
fun CompanionEditor(core: Core, environmentId: String, onDismiss: () -> Unit) {
    val connection by core.view.connection.watch()
    val server: EnvironmentView = connection.environments.firstOrNull { it.id == environmentId } ?: run {
        LaunchedEffect(Unit) { onDismiss() }
        return
    }
    // The core folds a burst of edits into one view: the last pick shows meanwhile.
    var pending by remember(environmentId) { mutableStateOf<CompanionLook?>(null) }
    LaunchedEffect(server.companion) { if (pending == server.companion.look) pending = null }
    LaunchedEffect(pending) {
        if (pending != null) {
            delay(500)
            pending = null
        }
    }
    val look = pending ?: server.companion.look
    fun set(next: CompanionLook) {
        pending = next
        core.update(Event.SetCompanion(server.id, next))
    }
    var preview by remember { mutableStateOf(CompanionMood.Idle) }

    Dialog(onDismissRequest = onDismiss, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Surface(Modifier.padding(horizontal = Space.l).widthIn(max = 480.dp).fillMaxWidth(), shape = MaterialTheme.shapes.extraLarge) {
            Column(Modifier.padding(Space.l).heightIn(max = 640.dp)) {
                Text("Companion for ${server.label}", style = MaterialTheme.typography.titleLarge, maxLines = 2, overflow = TextOverflow.Ellipsis)
                Column(
                    Modifier.weight(1f, fill = false).verticalScroll(rememberScrollState()).padding(vertical = Space.s),
                    verticalArrangement = Arrangement.spacedBy(Space.s),
                ) {
                    Box(Modifier.fillMaxWidth().padding(vertical = Space.s), contentAlignment = Alignment.Center) {
                        CompanionAvatar(look, preview, 112.dp, description = "Preview", onPoke = {})
                    }
                    val moods = listOf(CompanionMood.Idle, CompanionMood.Working, CompanionMood.Attention, CompanionMood.Finished, CompanionMood.Failed)
                    SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
                        moods.forEachIndexed { i, mood ->
                            SegmentedButton(
                                selected = preview == mood, onClick = { preview = mood },
                                shape = SegmentedButtonDefaults.itemShape(i, moods.size), icon = {},
                            ) { Text(mood.label, maxLines = 1, style = MaterialTheme.typography.labelSmall) }
                        }
                    }

                    Heading("Body")
                    Swatches(look.body, ink = false) { set(look.copy(body = it)) }

                    Heading("Eyes")
                    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                        eyeShapes.forEach { shape ->
                            val selected = look.eyeShape == shape
                            Column(
                                Modifier
                                    .background(if (selected) MaterialTheme.colorScheme.secondaryContainer else Color.Transparent, MaterialTheme.shapes.small)
                                    .clickable(role = Role.RadioButton) { set(look.copy(eyeShape = shape)) }
                                    .padding(Space.xs),
                                horizontalAlignment = Alignment.CenterHorizontally,
                            ) {
                                CompanionAvatar(look.copy(eyeShape = shape), CompanionMood.Idle, 36.dp, still = true)
                                Text(shape.replaceFirstChar { it.uppercase() }, style = MaterialTheme.typography.labelSmall)
                            }
                        }
                    }
                    Choices("Pupil size", listOf("small", "medium", "large"), look.pupilSize) { set(look.copy(pupilSize = it)) }
                    Text("Pupil colour", style = MaterialTheme.typography.bodyMedium)
                    Swatches(look.pupil, ink = true) { set(look.copy(pupil = it)) }

                    Heading("Extras")
                    Toggle("Antenna", look.antenna) { set(look.copy(antenna = it)) }
                    Choices("Ears", listOf("none", "round", "pointy"), look.ears) { set(look.copy(ears = it)) }
                    Toggle("Eyebrows", look.eyebrows) { set(look.copy(eyebrows = it)) }

                    Heading("Personality")
                    Choices("Energy", listOf("calm", "normal", "lively"), look.energy) { set(look.copy(energy = it)) }
                    Choices("Blink rate", listOf("rare", "normal", "often"), look.blink) { set(look.copy(blink = it)) }
                    Toggle("Eyes follow touches", look.follow) { set(look.copy(follow = it)) }
                }
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                    if (server.companion.custom) TextButton(onClick = { pending = null; core.update(Event.ResetCompanion(server.id)) }) { Text("Reset to default") }
                    TextButton(onClick = onDismiss) { Text("Done") }
                }
            }
        }
    }
}

@Composable
private fun Heading(text: String) {
    Text(text, style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.primary, modifier = Modifier.padding(top = Space.s))
}

@Composable
private fun Toggle(label: String, on: Boolean, set: (Boolean) -> Unit) {
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        Text(label, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f))
        Switch(checked = on, onCheckedChange = set)
    }
}

@Composable
private fun Choices(label: String, keys: List<String>, value: String, set: (String) -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(Space.xxs)) {
        Text(label, style = MaterialTheme.typography.bodyMedium)
        SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
            keys.forEachIndexed { i, key ->
                SegmentedButton(selected = value == key, onClick = { set(key) }, shape = SegmentedButtonDefaults.itemShape(i, keys.size)) {
                    Text(key.replaceFirstChar { it.uppercase() }, maxLines = 1)
                }
            }
        }
    }
}

/** The project colours (and, for pupils, ink) as round swatches. */
@Composable
private fun Swatches(value: String, ink: Boolean, set: (String) -> Unit) {
    FlowRow(horizontalArrangement = Arrangement.spacedBy(Space.s), verticalArrangement = Arrangement.spacedBy(Space.s)) {
        val keys = if (ink) listOf("ink") + projectColors else projectColors
        keys.forEach { key ->
            val selected = value == key
            Box(
                Modifier
                    .size(30.dp)
                    .border(if (selected) 2.dp else 1.dp, if (selected) MaterialTheme.colorScheme.onSurface else MaterialTheme.colorScheme.outlineVariant, CircleShape)
                    .padding(3.dp)
                    .background(if (key == "ink") Ink else projectColor(key), CircleShape)
                    .clickable(role = Role.RadioButton) { set(key) }
                    .semantics { contentDescription = if (selected) "$key, selected" else key },
            )
        }
    }
}
