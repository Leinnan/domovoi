//! Markdown to render-ready blocks (pulldown-cmark). Shells render each
//! block and its styled inline runs natively.

use crate::view::{MarkdownBlock, MarkdownRun};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use std::ops::Range;

fn block(kind: &str) -> MarkdownBlock {
    MarkdownBlock { kind: kind.into(), ..Default::default() }
}

/// Inline source collected between a block's start and end.
#[derive(Default)]
struct Capture {
    range: Option<Range<usize>>,
    runs: Vec<MarkdownRun>,
    task: Option<bool>,
}

impl Capture {
    fn add(&mut self, r: Range<usize>) {
        self.range = Some(match self.range.take() {
            Some(cur) => cur.start.min(r.start)..cur.end.max(r.end),
            None => r,
        });
    }

    fn take(&mut self, src: &str) -> (String, Vec<MarkdownRun>) {
        let text = self.range.take().map(|r| src[r].trim().to_owned()).unwrap_or_default();
        let mut runs = std::mem::take(&mut self.runs);
        if let [run] = runs.as_slice()
            && !run.bold && !run.italic && !run.strike && !run.code && run.link.is_none()
            && run.workspace_link.is_none() && run.text == text
        {
            runs.clear();
        }
        (text, runs)
    }

    fn run(&mut self, text: &str, style: &MarkdownRun, code: bool) {
        if !code && style.link.is_none() && style.workspace_link.is_none() && text.contains("://") {
            static LINKS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
                regex::Regex::new(r#"(?i)https?://[^\s<>"']+"#).expect("valid URL pattern")
            });
            let mut start = 0;
            for matched in LINKS.find_iter(text) {
                let url = matched.as_str().trim_end_matches(['.', ',', ':', ';', '!', '?', ')', ']']);
                let end = matched.start() + url.len();
                if end == matched.start() {
                    continue;
                }
                self.append(&text[start..matched.start()], style, false);
                let mut linked = style.clone();
                linked.link = Some(url.to_owned());
                self.append(url, &linked, false);
                start = end;
            }
            self.append(&text[start..], style, false);
        } else {
            self.append(text, style, code);
        }
    }

    fn append(&mut self, text: &str, style: &MarkdownRun, code: bool) {
        if text.is_empty() {
            return;
        }
        let mut run = style.clone();
        run.code = code;
        // Merge adjacent events with the same attributes without dropping whitespace.
        if let Some(last) = self.runs.last_mut()
            && last.bold == run.bold && last.italic == run.italic && last.strike == run.strike
            && last.code == run.code && last.link == run.link && last.workspace_link == run.workspace_link
        {
            last.text.push_str(text);
        } else {
            run.text.push_str(text);
            self.runs.push(run);
        }
    }
}

pub fn parse(src: &str) -> Vec<MarkdownBlock> {
    parse_marking(src).0
}

/// The part of a streaming message that can no longer change: its text and
/// blocks up to a boundary after which more text cannot alter them.
#[derive(Clone, Debug, Default)]
pub struct Prefix {
    text: String,
    blocks: Vec<MarkdownBlock>,
}

/// `parse(src)`, reusing `prev` when `src` extends its text: only what comes
/// after the prefix is parsed again. Returns the blocks and the prefix to keep
/// for the next, longer version of the text.
pub fn parse_streaming(prev: Option<&Prefix>, src: &str) -> (Vec<MarkdownBlock>, Option<Prefix>) {
    // A definition appended to the tail can resolve an earlier bracketed
    // reference. Keep such blocks in the part that is reparsed.
    let prev = prev.filter(|p| src.starts_with(&p.text));
    let base = prev.map_or(0, |p| p.text.len());
    let (tail, boundary) = parse_marking(&src[base..]);
    let mut blocks = prev.map(|p| p.blocks.clone()).unwrap_or_default();
    let head = blocks.len();
    blocks.extend(tail);
    let next = match boundary {
        Some((at, count)) if !src[base..base + at].contains('[') => Some(Prefix { text: src[..base + at].to_owned(), blocks: blocks[..head + count].to_vec() }),
        _ => prev.cloned(),
    };
    (blocks, next)
}

/// Parses `src`, and finds the last point where it can be cut so that
/// parsing the two halves apart gives the same blocks: the start of a
/// top-level block's line, after a blank line, following a block that a blank
/// line closes for good (a paragraph, heading, fenced code, table, quote or
/// rule; not a list or indented code, which can continue past one). Returns
/// the byte offset and how many blocks come before it.
fn parse_marking(src: &str) -> (Vec<MarkdownBlock>, Option<(usize, usize)>) {
    if crate::artifacts::is_html_document(src) {
        let mut document = block("html_document");
        document.text = src.to_owned();
        document.language = "html".into();
        return (vec![document], None);
    }
    if crate::artifacts::is_svg_document(src) {
        let mut document = block("svg_document");
        document.text = src.to_owned();
        document.language = "svg".into();
        return (vec![document], None);
    }
    let mut out = vec![];
    let mut boundary = None;
    // Nesting of block and inline tags; 0 between top-level blocks.
    let mut depth = 0usize;
    // The last closed top-level block: whether a blank line ends it for good.
    let mut closed: Option<bool> = None;
    let mut top_kind_safe = false;
    let mut cap = Capture::default();
    // (ordered, next number) per open list
    let mut lists: Vec<(bool, u64)> = vec![];
    let mut item_started = Vec::new();
    let mut styles = vec![MarkdownRun::default()];
    let mut alignments = Vec::new();
    let mut fence_end = 0;
    let mut cell_runs = Vec::new();
    let mut quote_depth = 0usize;
    let mut code: Option<MarkdownBlock> = None;
    let mut heading: Option<u32> = None;
    let mut row: Option<(Vec<String>, bool)> = None;
    let mut in_head = false;
    let mut cell = Capture::default();

    let flush_item = |cap: &mut Capture, lists: &mut Vec<(bool, u64)>, started: &mut Vec<bool>, quote_depth: usize, out: &mut Vec<MarkdownBlock>| {
        let (text, runs) = cap.take(src);
        let task = cap.task.take();
        if text.is_empty() && task.is_none() {
            return;
        }
        let Some(continuation) = started.last_mut() else { return };
        let mut b = block("list_item");
        b.text = text;
        b.runs = runs;
        b.level = lists.len().saturating_sub(1) as u32;
        b.quote_depth = quote_depth as u32;
        b.task_checked = task;
        b.continuation = *continuation;
        if let Some((ordered, n)) = lists.last_mut() {
            b.ordered = *ordered;
            if !*continuation {
                b.number = *n as u32;
                *n += 1;
            }
        }
        *continuation = true;
        out.push(b);
    };

    for (event, range) in Parser::new_ext(src, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS).into_offset_iter() {
        match &event {
            Event::Start(tag) => {
                if depth == 0 {
                    if closed == Some(true)
                        && let Some(at) = cut_before(src, range.start)
                    {
                        boundary = Some((at, out.len()));
                    }
                    top_kind_safe = matches!(
                        tag,
                        Tag::Paragraph | Tag::Heading { .. } | Tag::Table(_) | Tag::BlockQuote(_) | Tag::CodeBlock(CodeBlockKind::Fenced(_))
                    );
                }
                depth += 1;
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    closed = Some(top_kind_safe);
                }
            }
            Event::Rule if depth == 0 => {
                if closed == Some(true)
                    && let Some(at) = cut_before(src, range.start)
                {
                    boundary = Some((at, out.len()));
                }
                closed = Some(true);
            }
            _ if depth == 0 => closed = Some(false),
            _ => {}
        }
        if let Some(c) = code.as_mut() {
            match event {
                Event::Text(t) => c.text.push_str(&t),
                Event::End(TagEnd::CodeBlock) => {
                    let mut c = code.take().expect("inside a code block");
                    if c.text.ends_with('\n') {
                        c.text.pop();
                    }
                    c.fence_closed = fence_end > 0 && has_closing_fence(&src[range.start..fence_end]);
                    if !(c.fence_closed && c.language.eq_ignore_ascii_case("mermaid")) {
                        c.spans = crate::highlight::highlight(&c.text, &c.language);
                    }
                    out.push(c);
                }
                _ => {}
            }
            continue;
        }
        let capture = if row.is_some() { &mut cell } else { &mut cap };
        let style = styles.last().expect("base inline style");
        match &event {
            Event::Start(tag @ (Tag::Strong | Tag::Emphasis | Tag::Strikethrough | Tag::Link { .. } | Tag::Image { .. })) => {
                let mut next = style.clone();
                match tag {
                    Tag::Strong => next.bold = true,
                    Tag::Emphasis => next.italic = true,
                    Tag::Strikethrough => next.strike = true,
                    Tag::Link { dest_url, .. } if dest_url.starts_with("https://") || dest_url.starts_with("http://") || dest_url.starts_with("mailto:") => {
                        next.link = Some(dest_url.to_string());
                    }
                    Tag::Link { dest_url, .. } => {
                        next.workspace_link = crate::artifacts::artifact_link_path(dest_url);
                    }
                    _ => {}
                }
                styles.push(next);
            }
            Event::End(TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough | TagEnd::Link | TagEnd::Image) => {
                styles.pop();
            }
            Event::Text(t) | Event::InlineHtml(t) => capture.run(t, style, false),
            Event::Code(t) => capture.run(t, style, true),
            Event::SoftBreak => capture.run(" ", style, false),
            Event::HardBreak => capture.run("\n", style, false),
            Event::TaskListMarker(checked) => capture.task = Some(*checked),
            _ => {}
        }
        match event {
            Event::Start(Tag::CodeBlock(kind)) => {
                if !item_started.is_empty() {
                    flush_item(&mut cap, &mut lists, &mut item_started, quote_depth, &mut out);
                }
                let mut c = block("code");
                fence_end = 0;
                c.quote_depth = quote_depth as u32;
                if let CodeBlockKind::Fenced(info) = kind {
                    fence_end = range.end;
                    c.language = info.split_whitespace().next().unwrap_or_default().to_owned();
                }
                code = Some(c);
            }
            Event::Start(Tag::Heading { level, .. }) => heading = Some(level as u32),
            Event::End(TagEnd::Heading(_)) => {
                let mut b = block("heading");
                b.level = heading.take().unwrap_or(1);
                (b.text, b.runs) = cap.take(src);
                b.quote_depth = quote_depth as u32;
                out.push(b);
            }
            Event::Start(Tag::List(start)) => {
                if !item_started.is_empty() {
                    flush_item(&mut cap, &mut lists, &mut item_started, quote_depth, &mut out);
                }
                lists.push((start.is_some(), start.unwrap_or(1)));
            }
            Event::End(TagEnd::List(_)) => {
                lists.pop();
            }
            Event::Start(Tag::Item) => item_started.push(false),
            Event::End(TagEnd::Item) => {
                flush_item(&mut cap, &mut lists, &mut item_started, quote_depth, &mut out);
                item_started.pop();
            }
            Event::Start(Tag::BlockQuote(_)) => quote_depth += 1,
            Event::End(TagEnd::Paragraph) if item_started.is_empty() => {
                let (text, runs) = cap.take(src);
                if !text.is_empty() {
                    let mut b = block(if quote_depth > 0 { "quote" } else { "paragraph" });
                    b.text = text;
                    b.runs = runs;
                    b.quote_depth = quote_depth as u32;
                    out.push(b);
                }
            }
            Event::End(TagEnd::Paragraph) => flush_item(&mut cap, &mut lists, &mut item_started, quote_depth, &mut out),
            Event::End(TagEnd::BlockQuote(_)) => quote_depth -= 1,
            Event::Html(html) => {
                let mut b = block("html_source");
                b.text = html.into_string();
                out.push(b);
            }
            Event::Rule => out.push(block("rule")),
            Event::Start(Tag::Table(columns)) => alignments = columns.into_iter().map(|align| match align {
                pulldown_cmark::Alignment::Right => "right",
                pulldown_cmark::Alignment::Center => "center",
                _ => "left",
            }.to_owned()).collect(),
            Event::Start(Tag::TableHead) => {
                in_head = true;
                row = Some((vec![], true));
            }
            Event::End(TagEnd::TableHead) => {
                in_head = false;
                if let Some((cells, header)) = row.take() {
                    out.push(MarkdownBlock { kind: "table_row".into(), cells, header, cell_runs: std::mem::take(&mut cell_runs), alignments: alignments.clone(), ..Default::default() });
                }
            }
            Event::Start(Tag::TableRow) => row = Some((vec![], in_head)),
            Event::End(TagEnd::TableRow) => {
                if let Some((cells, header)) = row.take() {
                    out.push(MarkdownBlock { kind: "table_row".into(), cells, header, cell_runs: std::mem::take(&mut cell_runs), alignments: alignments.clone(), ..Default::default() });
                }
            }
            Event::Start(Tag::TableCell) => cell = Capture::default(),
            Event::End(TagEnd::TableCell) => {
                let (text, runs) = cell.take(src);
                cell_runs.push(runs);
                if let Some((cells, _)) = row.as_mut() {
                    cells.push(text);
                }
            }
            Event::Text(_) | Event::Code(_) | Event::InlineMath(_) | Event::InlineHtml(_) | Event::SoftBreak | Event::HardBreak | Event::FootnoteReference(_) => {
                if row.is_some() {
                    cell.add(range);
                } else {
                    cap.add(range);
                }
            }
            Event::Start(Tag::Emphasis | Tag::Strong | Tag::Strikethrough | Tag::Link { .. } | Tag::Image { .. }) => {
                if row.is_some() {
                    cell.add(range);
                } else {
                    cap.add(range);
                }
            }
            _ => {}
        }
    }
    if let Some(c) = code.take() {
        out.push(c);
    }
    (out, boundary)
}

/// pulldown closes an unterminated fence at EOF too. Check the actual source.
fn has_closing_fence(source: &str) -> bool {
    let mut lines = source.lines();
    let Some(open) = lines.next() else { return false };
    let open = open.trim_start();
    let Some(marker @ ('`' | '~')) = open.chars().next() else { return false };
    let length = open.chars().take_while(|c| *c == marker).count();
    let Some(close) = lines.rfind(|line| !line.trim().is_empty()) else { return false };
    let close = close.trim_matches(|c: char| c.is_whitespace() || c == '>');
    close.len() >= length && close.chars().all(|c| c == marker)
}

/// The start of the line holding `start`, if a blank line separates it from
/// the text before.
fn cut_before(src: &str, start: usize) -> Option<usize> {
    let line = src[..start].rfind('\n').map_or(0, |i| i + 1);
    let content = src[..line].trim_end().len();
    (line > 0 && src[content..line].matches('\n').count() >= 2).then_some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_cover_common_markdown() {
        let md = "# Title\n\nSome **bold** and `code`.\n\n- one\n- two with [link](http://x)\n  1. nested\n\n```rust\nfn main() {}\n```\n\n> quoted\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n---\n";
        let b = parse(md);
        let kinds: Vec<&str> = b.iter().map(|b| b.kind.as_str()).collect();
        assert_eq!(kinds, ["heading", "paragraph", "list_item", "list_item", "list_item", "code", "quote", "table_row", "table_row", "rule"]);
        assert_eq!(b[0].text, "Title");
        assert_eq!(b[1].text, "Some **bold** and `code`.");
        assert_eq!(b[3].text, "two with [link](http://x)");
        assert_eq!((b[4].ordered, b[4].level, b[4].number), (true, 1, 1));
        assert_eq!((b[5].language.as_str(), b[5].text.as_str()), ("rust", "fn main() {}"));
        assert_eq!(b[7].cells, ["a", "b"]);
        assert!(b[7].header && !b[8].header);
    }

    #[test]
    fn unterminated_code_fence_still_renders() {
        let b = parse("text\n\n```py\nprint(1)");
        assert_eq!(b.last().unwrap().kind, "code");
        assert_eq!(b.last().unwrap().text, "print(1)");
    }

    #[test]
    fn styled_runs_and_block_metadata() {
        let blocks = parse("**bold *and italic*** and [a reference][ref]  \nnext\n\n[ref]: https://example.com\n\n- [x] done\n\n  continued\n\n> > nested\n\n| left | right |\n|:---|---:|\n| a | `b` |");
        assert!(blocks[0].runs.iter().any(|r| r.bold && r.italic && r.text == "and italic"));
        assert!(blocks[0].runs.iter().any(|r| r.link.as_deref() == Some("https://example.com")));
        assert!(blocks[0].runs.iter().any(|r| r.text.contains('\n')));
        assert_eq!(blocks[1].task_checked, Some(true));
        assert!(blocks[2].continuation);
        assert_eq!(blocks[3].quote_depth, 2);
        assert_eq!(blocks[4].alignments, ["left", "right"]);
        assert!(blocks[5].cell_runs[1][0].code);
    }

    #[test]
    fn only_actual_closing_fences_are_marked() {
        assert!(parse("```mermaid\ngraph LR\nA --> B\n```")[0].fence_closed);
        assert!(parse("> ~~~html\n> <p>Hi</p>\n> ~~~")[0].fence_closed);
        assert!(!parse("```mermaid\ngraph LR\nA --> B")[0].fence_closed);
    }

    const DOCS: &[&str] = &[
        "# Plan\n\nFirst a paragraph with **bold**,\nwrapped over lines.\n\n```rust\nfn main() {\n\n    println!(\"hi\");\n}\n```\n\nThen text.\n\n## Steps\n\n1. one\n2. two\n\n   still two\n3. three\n   - nested\n\n     nested para\n\nAfter the list.\n\n4. a new list from four\n\n> quoted\n> more\n\n> another quote\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n---\n\n    indented code\n\n    still code\n\nEnd [ref].\n\n[ref]: http://example.com\n",
        "Intro\n\n- a\n- b\n\n- loose c\n\nText\n===\n\nPara\n\n---\n\n```\nunterminated\n\nfence",
        "* x\n\n  ```sh\n  ls\n\n  pwd\n  ```\n\n* y\n\nDone. Héllo — ünïcode ✓\n\n<div>\nhtml\n\n</div>\n\nTail",
    ];

    #[test]
    fn streaming_parse_equals_full_parse_at_every_length() {
        for doc in DOCS {
            let mut prefix: Option<Prefix> = None;
            let cuts: Vec<usize> = (0..=doc.len()).filter(|&i| doc.is_char_boundary(i)).collect();
            for &cut in &cuts {
                let text = &doc[..cut];
                let (blocks, next) = parse_streaming(prefix.as_ref(), text);
                assert_eq!(blocks, parse(text), "at {cut} of {doc:?}");
                prefix = next;
            }
            // Something was reused by the end.
            assert!(prefix.is_some_and(|p| !p.text.is_empty()), "{doc:?}");
        }
    }

    #[test]
    fn streaming_parse_starts_over_when_text_is_replaced() {
        let (_, p) = parse_streaming(None, "one\n\ntwo\n\nthree");
        let (blocks, _) = parse_streaming(p.as_ref(), "other\n\ntext");
        assert_eq!(blocks, parse("other\n\ntext"));
    }
}
