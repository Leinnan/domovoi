//! Markdown to render-ready blocks (pulldown-cmark). Shells render each
//! block natively; inline markup inside a block stays markdown source, which
//! SwiftUI's `AttributedString(markdown:)` handles.

use crate::view::MarkdownBlock;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use std::ops::Range;

fn block(kind: &str) -> MarkdownBlock {
    MarkdownBlock { kind: kind.into(), ..Default::default() }
}

/// Inline source collected between a block's start and end.
#[derive(Default)]
struct Capture {
    range: Option<Range<usize>>,
}

impl Capture {
    fn add(&mut self, r: Range<usize>) {
        self.range = Some(match self.range.take() {
            Some(cur) => cur.start.min(r.start)..cur.end.max(r.end),
            None => r,
        });
    }

    fn take(&mut self, src: &str) -> String {
        self.range.take().map(|r| src[r].trim().to_owned()).unwrap_or_default()
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
    let prev = prev.filter(|p| src.starts_with(&p.text));
    let base = prev.map_or(0, |p| p.text.len());
    let (tail, boundary) = parse_marking(&src[base..]);
    let mut blocks = prev.map(|p| p.blocks.clone()).unwrap_or_default();
    let head = blocks.len();
    blocks.extend(tail);
    let next = match boundary {
        Some((at, count)) => Some(Prefix { text: src[..base + at].to_owned(), blocks: blocks[..head + count].to_vec() }),
        None => prev.cloned(),
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
    let mut item_depth = 0usize;
    let mut quote_depth = 0usize;
    let mut code: Option<MarkdownBlock> = None;
    let mut heading: Option<u32> = None;
    let mut row: Option<(Vec<String>, bool)> = None;
    let mut in_head = false;
    let mut cell = Capture::default();

    let flush_item = |cap: &mut Capture, lists: &mut Vec<(bool, u64)>, out: &mut Vec<MarkdownBlock>| {
        let text = cap.take(src);
        if text.is_empty() {
            return;
        }
        let mut b = block("list_item");
        b.text = text;
        b.level = lists.len().saturating_sub(1) as u32;
        if let Some((ordered, n)) = lists.last_mut() {
            b.ordered = *ordered;
            b.number = *n as u32;
            *n += 1;
        }
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
                    c.spans = crate::highlight::highlight(&c.text, &c.language);
                    out.push(c);
                }
                _ => {}
            }
            continue;
        }
        match event {
            Event::Start(Tag::CodeBlock(kind)) => {
                if item_depth > 0 {
                    flush_item(&mut cap, &mut lists, &mut out);
                }
                let mut c = block("code");
                if let CodeBlockKind::Fenced(info) = kind {
                    c.language = info.split_whitespace().next().unwrap_or_default().to_owned();
                }
                code = Some(c);
            }
            Event::Start(Tag::Heading { level, .. }) => heading = Some(level as u32),
            Event::End(TagEnd::Heading(_)) => {
                let mut b = block("heading");
                b.level = heading.take().unwrap_or(1);
                b.text = cap.take(src);
                out.push(b);
            }
            Event::Start(Tag::List(start)) => {
                if item_depth > 0 {
                    flush_item(&mut cap, &mut lists, &mut out);
                }
                lists.push((start.is_some(), start.unwrap_or(1)));
            }
            Event::End(TagEnd::List(_)) => {
                lists.pop();
            }
            Event::Start(Tag::Item) => item_depth += 1,
            Event::End(TagEnd::Item) => {
                flush_item(&mut cap, &mut lists, &mut out);
                item_depth -= 1;
            }
            Event::Start(Tag::BlockQuote(_)) => quote_depth += 1,
            Event::End(TagEnd::BlockQuote(_)) => quote_depth -= 1,
            Event::End(TagEnd::Paragraph) if item_depth == 0 => {
                let text = cap.take(src);
                if !text.is_empty() {
                    let mut b = block(if quote_depth > 0 { "quote" } else { "paragraph" });
                    b.text = text;
                    out.push(b);
                }
            }
            Event::Rule => out.push(block("rule")),
            Event::Start(Tag::TableHead) => {
                in_head = true;
                row = Some((vec![], true));
            }
            Event::End(TagEnd::TableHead) => {
                in_head = false;
                if let Some((cells, header)) = row.take() {
                    out.push(MarkdownBlock { kind: "table_row".into(), cells, header, ..Default::default() });
                }
            }
            Event::Start(Tag::TableRow) => row = Some((vec![], in_head)),
            Event::End(TagEnd::TableRow) => {
                if let Some((cells, header)) = row.take() {
                    out.push(MarkdownBlock { kind: "table_row".into(), cells, header, ..Default::default() });
                }
            }
            Event::Start(Tag::TableCell) => cell = Capture::default(),
            Event::End(TagEnd::TableCell) => {
                let text = cell.take(src);
                if let Some((cells, _)) = row.as_mut() {
                    cells.push(text);
                }
            }
            Event::Text(_) | Event::Code(_) | Event::InlineMath(_) | Event::Html(_) | Event::InlineHtml(_) | Event::SoftBreak | Event::HardBreak | Event::FootnoteReference(_) | Event::TaskListMarker(_) => {
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
