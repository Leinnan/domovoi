package com.crayen.domovoi.ui.common

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.ContentCopy
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.withLink
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.crayen.domovoi.app.CodeSpan
import com.crayen.domovoi.app.MarkdownBlock
import com.crayen.domovoi.ui.theme.LocalTints
import com.crayen.domovoi.ui.theme.Mono
import com.crayen.domovoi.ui.theme.Space

/** The core's highlight kinds as colours (`CodeHighlight.swift`). */
@Composable
fun highlightColor(kind: String): Color? {
    val dark = MaterialTheme.colorScheme.surface.luminance() < 0.5f
    val tints = LocalTints.current
    return when (kind) {
        "keyword" -> if (dark) Color(0xFFFF7AB2) else Color(0xFFAD3DA4)
        "string" -> if (dark) Color(0xFFFFA14F) else Color(0xFFC4501E)
        "comment" -> MaterialTheme.colorScheme.outline
        "number" -> if (dark) Color(0xFF8FB8FF) else Color(0xFF1C4FD6)
        "type" -> if (dark) Color(0xFF6ED6CA) else Color(0xFF0B7A70)
        "function" -> if (dark) Color(0xFFB5A3FF) else Color(0xFF5B3CC4)
        "added" -> tints.success
        "removed" -> MaterialTheme.colorScheme.error
        else -> null
    }
}

private fun Color.luminance(): Float = 0.2126f * red + 0.7152f * green + 0.0722f * blue

/** `spans` as styled text, up to `maxLines` lines; `fallback` when there are none. */
@Composable
fun highlighted(spans: List<CodeSpan>, fallback: String, maxLines: Int? = null): AnnotatedString {
    val colors = spans.map { it.kind }.distinct().associateWith { highlightColor(it) }
    return remember(spans, fallback, maxLines, colors) {
        buildAnnotatedString {
            if (spans.isEmpty()) {
                append(if (maxLines == null) fallback else fallback.lineSequence().take(maxLines).joinToString("\n"))
                return@buildAnnotatedString
            }
            var lines = 0
            for (span in spans) {
                var piece = span.text
                if (maxLines != null) {
                    val cut = StringBuilder()
                    for (ch in piece) {
                        if (ch == '\n') { lines++; if (lines >= maxLines) break }
                        cut.append(ch)
                    }
                    piece = cut.toString()
                }
                val color = colors[span.kind]
                if (color != null || span.kind == "comment") {
                    withStyle(SpanStyle(color = color ?: Color.Unspecified, fontStyle = if (span.kind == "comment") FontStyle.Italic else FontStyle.Normal)) { append(piece) }
                } else append(piece)
                if (maxLines != null && lines >= maxLines) break
            }
        }
    }
}

/**
 * Inline markdown (`**bold**`, `*italic*`, `` `code` ``, `~~strike~~`,
 * `[text](url)` and bare links) as styled text.
 */
fun inlineMarkdown(text: String, codeBackground: Color, linkColor: Color): AnnotatedString = buildAnnotatedString {
    var i = 0
    val n = text.length
    fun startsWith(token: String, at: Int) = text.startsWith(token, at)
    while (i < n) {
        val c = text[i]
        when {
            c == '\\' && i + 1 < n && text[i + 1] in "\\`*_~[]()#" -> { append(text[i + 1]); i += 2 }
            c == '`' -> {
                val ticks = text.drop(i).takeWhile { it == '`' }.length
                val close = text.indexOf("`".repeat(ticks), i + ticks)
                if (close < 0) { append(text.substring(i, i + ticks)); i += ticks }
                else {
                    withStyle(SpanStyle(fontFamily = Mono, background = codeBackground, fontSize = 0.92.em())) { append(text.substring(i + ticks, close).trim()) }
                    i = close + ticks
                }
            }
            startsWith("**", i) || startsWith("__", i) -> {
                val token = text.substring(i, i + 2)
                val close = text.indexOf(token, i + 2)
                if (close < 0) { append(token); i += 2 }
                else { withStyle(SpanStyle(fontWeight = FontWeight.SemiBold)) { append(inlineMarkdown(text.substring(i + 2, close), codeBackground, linkColor)) }; i = close + 2 }
            }
            startsWith("~~", i) -> {
                val close = text.indexOf("~~", i + 2)
                if (close < 0) { append("~~"); i += 2 }
                else { withStyle(SpanStyle(textDecoration = TextDecoration.LineThrough)) { append(inlineMarkdown(text.substring(i + 2, close), codeBackground, linkColor)) }; i = close + 2 }
            }
            (c == '*' || c == '_') && i + 1 < n && !text[i + 1].isWhitespace() && (i == 0 || !text[i - 1].isLetterOrDigit()) -> {
                val close = text.indexOf(c, i + 1)
                if (close < 0 || text[close - 1].isWhitespace()) { append(c); i++ }
                else { withStyle(SpanStyle(fontStyle = FontStyle.Italic)) { append(inlineMarkdown(text.substring(i + 1, close), codeBackground, linkColor)) }; i = close + 1 }
            }
            c == '[' -> {
                val mid = text.indexOf("](", i)
                val end = if (mid > 0) text.indexOf(')', mid) else -1
                if (mid < 0 || end < 0 || text.substring(i, mid).contains('\n')) { append(c); i++ }
                else {
                    val label = text.substring(i + 1, mid)
                    val url = text.substring(mid + 2, end)
                    withLink(LinkAnnotation.Url(url, TextLinkStyles(SpanStyle(color = linkColor, textDecoration = TextDecoration.Underline)))) {
                        append(inlineMarkdown(label, codeBackground, linkColor))
                    }
                    i = end + 1
                }
            }
            startsWith("https://", i) || startsWith("http://", i) -> {
                var end = i
                while (end < n && !text[end].isWhitespace() && text[end] !in "<>\"") end++
                while (end > i && text[end - 1] in ".,;:!?)") end--
                val url = text.substring(i, end)
                withLink(LinkAnnotation.Url(url, TextLinkStyles(SpanStyle(color = linkColor)))) { append(url) }
                i = end
            }
            else -> { append(c); i++ }
        }
    }
}

private fun Double.em() = androidx.compose.ui.unit.TextUnit(this.toFloat(), androidx.compose.ui.unit.TextUnitType.Em)

@Composable
fun rememberInline(text: String): AnnotatedString {
    val code = MaterialTheme.colorScheme.surfaceContainerHighest
    val link = MaterialTheme.colorScheme.primary
    return remember(text, code, link) { inlineMarkdown(text, code, link) }
}

/** The core's markdown blocks; a caret after the last while it streams. */
@Composable
fun MarkdownBlocks(blocks: List<MarkdownBlock>, modifier: Modifier = Modifier, streaming: Boolean = false) {
    Column(modifier, verticalArrangement = Arrangement.spacedBy(Space.s + Space.xxs)) {
        var i = 0
        while (i < blocks.size) {
            val block = blocks[i]
            if (block.kind == "table_row") {
                val start = i
                while (i < blocks.size && blocks[i].kind == "table_row") i++
                MarkdownTable(blocks.subList(start, i))
                continue
            }
            MarkdownBlockView(block, caret = streaming && i == blocks.lastIndex)
            i++
        }
    }
}

@Composable
private fun MarkdownBlockView(block: MarkdownBlock, caret: Boolean) {
    val body = MaterialTheme.typography.bodyLarge
    val text = rememberInline(block.text)
    val shown = if (caret) buildAnnotatedString {
        append(text)
        withStyle(SpanStyle(color = MaterialTheme.colorScheme.primary)) { append(" ▍") }
    } else text
    when (block.kind) {
        "heading" -> Text(
            shown,
            style = when {
                block.level <= 1u -> MaterialTheme.typography.titleLarge
                block.level == 2u -> MaterialTheme.typography.titleMedium
                else -> MaterialTheme.typography.titleSmall
            },
            fontWeight = FontWeight.SemiBold,
            modifier = Modifier.padding(top = Space.xs),
        )
        "code" -> CodeBlock(block.text, block.language, block.spans)
        "list_item" -> Row(Modifier.padding(start = Space.l * block.level.toInt()), horizontalArrangement = Arrangement.spacedBy(Space.s)) {
            Text(if (block.ordered) "${block.number}." else "•", style = body, color = MaterialTheme.colorScheme.onSurfaceVariant)
            Text(shown, style = body)
        }
        "quote" -> Row(Modifier.height(IntrinsicSize.Min), horizontalArrangement = Arrangement.spacedBy(Space.s)) {
            Box(Modifier.width(3.dp).fillMaxHeight().background(MaterialTheme.colorScheme.outlineVariant, MaterialTheme.shapes.small))
            Text(shown, style = body, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        "rule" -> HorizontalDivider(Modifier.padding(vertical = Space.xs))
        else -> Text(shown, style = body)
    }
}

@Composable
private fun MarkdownTable(rows: List<MarkdownBlock>) {
    val border = MaterialTheme.colorScheme.outlineVariant
    Box(
        Modifier.horizontalScroll(rememberScrollState())
            .border(0.5.dp, border, MaterialTheme.shapes.small),
    ) {
        Column {
            rows.forEachIndexed { index, row ->
                val bg = when {
                    row.header -> MaterialTheme.colorScheme.surfaceContainerHigh
                    index % 2 == 0 -> MaterialTheme.colorScheme.surfaceContainerLow
                    else -> Color.Transparent
                }
                Row(Modifier.background(bg)) {
                    row.cells.forEach { cell ->
                        Text(
                            rememberInline(cell),
                            style = MaterialTheme.typography.bodyMedium,
                            fontWeight = if (row.header) FontWeight.SemiBold else FontWeight.Normal,
                            modifier = Modifier.widthIn(min = 64.dp, max = 260.dp).padding(horizontal = Space.s + Space.xxs, vertical = Space.xs + Space.xxs),
                        )
                    }
                }
            }
        }
    }
}

/** A fenced code block: highlighted, folded when long, with a copy button. */
@Composable
fun CodeBlock(code: String, language: String, spans: List<CodeSpan>) {
    val context = LocalContext.current
    val lineCount = remember(code) { code.count { it == '\n' } + 1 }
    var expanded by rememberSaveable(code.hashCode()) { mutableStateOf(false) }
    val folded = lineCount > 24 && !expanded
    Surface(color = MaterialTheme.colorScheme.surfaceContainerHigh, shape = MaterialTheme.shapes.small, modifier = Modifier.fillMaxWidth()) {
        Column {
            Row(Modifier.fillMaxWidth().padding(start = Space.m), verticalAlignment = Alignment.CenterVertically) {
                Text(language.ifEmpty { "code" }, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.weight(1f))
                IconButton(onClick = { copyText(context, code) }) { Icon(Icons.Outlined.ContentCopy, "Copy code", modifier = Modifier.width(18.dp)) }
            }
            Text(
                highlighted(spans, code, if (folded) 24 else null),
                fontFamily = Mono,
                fontSize = 13.sp,
                lineHeight = 18.sp,
                softWrap = false,
                modifier = Modifier.horizontalScroll(rememberScrollState()).padding(start = Space.m, end = Space.m, bottom = Space.m),
            )
            if (lineCount > 24) {
                TextButton(onClick = { expanded = !expanded }, modifier = Modifier.padding(start = Space.xs)) {
                    Text(if (expanded) "Show less" else "Show all $lineCount lines")
                }
            }
        }
    }
}
