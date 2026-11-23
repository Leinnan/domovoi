//! How a tool call reads in the transcript: a verb in the tense of its
//! status, what it worked on, a detail, and what opening it shows. Every
//! provider names and shapes its tools its own way (Claude's `Read`, a shell
//! `sed -n`, Antigravity's "File …", Codex's `view_image`); the look is
//! decided here, once, for every shell.

#![expect(clippy::disallowed_types, reason = "a tool call's input and output are whatever shape each tool defines (t3code `Schema.Unknown`)")]

use crate::view::{DiffFileView, FieldView, ItemView, LinkView, MarkdownBlock};
use domovoi_model::typed::{CommandExecutionItem, DynamicToolItem, FileSearchItem, TurnItem, TurnItemStatus, WebSearchItem};
use serde_json::Value;
use std::sync::Arc;

/// The most of a title shown (the shells cut it to a line anyway).
const TITLE_CHARS: usize = 160;
/// The most of one field's value shown while the row is not expanded.
const FIELD_CHARS: usize = 4000;
/// Lines of a file shown before "Show All".
const FILE_LINES: usize = 200;
/// A step that took at least this long says so.
const SLOW_MS: i64 = 10_000;

/// What a row needs besides its item.
#[derive(Clone, Copy, Default)]
pub(crate) struct Ctx<'a> {
    /// The thread's folder: paths inside it are shown relative to it.
    pub cwd: &'a str,
    /// Show everything (the user asked for all of it).
    pub expanded: bool,
    /// How long the step took, once it finished (0: unknown or running).
    pub duration_ms: i64,
}

fn running(status: &TurnItemStatus) -> bool {
    *status == TurnItemStatus::Running
}

fn tense(running: bool, now: &str, done: &str) -> String {
    if running { now } else { done }.to_owned()
}

/// The last part of a path.
pub(crate) fn file_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or(trimmed)
}

/// `path` relative to `cwd` when inside it.
fn relative<'a>(path: &'a str, cwd: &str) -> &'a str {
    let cwd = cwd.trim_end_matches('/');
    if cwd.is_empty() {
        return path;
    }
    match path.strip_prefix(cwd) {
        Some("") => ".",
        Some(rest) if rest.starts_with('/') => &rest[1..],
        _ => path,
    }
}

/// The folder a file is in, relative to `cwd` (empty: `cwd` itself).
fn folder(path: &str, cwd: &str) -> String {
    let rel = relative(path, cwd);
    match rel.trim_end_matches('/').rsplit_once('/') {
        Some((dir, _)) if !dir.is_empty() => dir.to_owned(),
        Some(_) => "/".to_owned(),
        None => String::new(),
    }
}

fn join(parts: &[&str], sep: &str) -> String {
    parts.iter().filter(|p| !p.is_empty()).copied().collect::<Vec<_>>().join(sep)
}

fn first_line(text: &str) -> &str {
    text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default()
}

fn clip(text: &str, max: usize) -> String {
    domovoi_model::typed::clip_chars(text, max)
}

/// "1m 12s", "14s".
fn duration_label(ms: i64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}h {}m", s / 3600, s % 3600 / 60)
    } else if s >= 60 {
        format!("{}m {}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

/// "1,234".
fn count_label(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{} {}", count_label(n), if n == 1 { one } else { many })
}

/// Text the agent was told but the user wasn't meant to read.
fn without_reminders(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains("<system-reminder>") {
        return text.into();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<system-reminder>") {
        out.push_str(&rest[..start]);
        match rest[start..].find("</system-reminder>") {
            Some(end) => rest = &rest[start + end + "</system-reminder>".len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.truncate(out.trim_end().len());
    out.into()
}

pub(crate) fn is_image_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".heic", ".tif", ".tiff"].iter().any(|ext| lower.ends_with(ext))
}

fn str_of<'a>(input: Option<&'a Value>, keys: &[&str]) -> Option<&'a str> {
    let input = input?;
    keys.iter().find_map(|k| input.get(*k).and_then(Value::as_str)).map(str::trim).filter(|s| !s.is_empty())
}

fn num_of(input: Option<&Value>, keys: &[&str]) -> Option<u64> {
    let input = input?;
    keys.iter().find_map(|k| input.get(*k).and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))))
}

/// The first lines of `text`, and whether there were more.
fn head(text: &str, expanded: bool) -> (String, bool) {
    if expanded {
        return (text.to_owned(), false);
    }
    match text.match_indices('\n').nth(FILE_LINES - 1) {
        Some((at, _)) if at + 1 < text.len() => (format!("{}\n…", &text[..at]), true),
        _ => (text.to_owned(), false),
    }
}

/// A file's text as a highlighted code block.
fn code_block(text: &str, path: &str) -> Arc<Vec<MarkdownBlock>> {
    let language = crate::highlight::language_for_path(path);
    let spans = crate::highlight::highlight(text, &language);
    Arc::new(vec![MarkdownBlock { kind: "code".into(), text: text.to_owned(), language, fence_closed: true, spans, ..Default::default() }])
}

/// Output shown at its end (as commands' output is), cut until expanded.
fn set_output(v: &mut ItemView, text: &str, ctx: Ctx) {
    if text.trim().is_empty() {
        return;
    }
    if ctx.expanded {
        v.output = Some(text.into());
    } else {
        let (shown, cut) = crate::present::tail(text);
        v.output = Some(shown.into());
        v.truncated |= cut;
    }
}

/// Text from Claude's `Read` (`cat -n` lines: "    12\tcode" or "12→code"):
/// the code without its line numbers, and the first and last numbers.
fn numbered_lines(text: &str) -> Option<(String, u64, u64)> {
    let mut code = String::with_capacity(text.len());
    let (mut first, mut last) = (None, 0);
    for line in text.lines() {
        let trimmed = line.trim_start();
        let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
        let rest = &trimmed[digits..];
        let body = rest.strip_prefix('\t').or_else(|| rest.strip_prefix('→'));
        let (Some(body), true) = (body, digits > 0) else {
            // A note after the file (an empty file's, a truncation's) ends it.
            if first.is_none() {
                return None;
            }
            break;
        };
        let n: u64 = trimmed[..digits].parse().ok()?;
        first.get_or_insert(n);
        last = n;
        code.push_str(body);
        code.push('\n');
    }
    code.pop();
    first.map(|f| (code, f, last))
}

fn range_label(from: u64, to: u64) -> String {
    if from == to { format!("line {from}") } else { format!("lines {from}–{to}") }
}

/// A file read: Claude's `Read`, a shell `cat`/`sed -n`, Antigravity's "File".
fn read(v: &mut ItemView, path: &str, range: Option<(u64, u64)>, text: Option<&str>, running: bool, ctx: Ctx) {
    v.kind = "tool".into();
    v.icon = "read".into();
    v.verb = tense(running, "Reading", "Read");
    v.title = clip(file_name(path), TITLE_CHARS);
    v.mono = true;
    let text = text.map(without_reminders).filter(|t| !t.trim().is_empty());
    let numbered = text.as_deref().and_then(numbered_lines);
    let shown = match (&numbered, range) {
        (Some((_, from, to)), _) if *from > 1 => Some(range_label(*from, *to)),
        (Some((_, _, to)), Some(_)) => Some(range_label(1, *to)),
        (Some((_, _, to)), None) => Some(plural(*to as usize, "line", "lines")),
        (None, Some((from, to))) => Some(range_label(from, to)),
        (None, None) => None,
    };
    v.subtitle = join(&[&folder(path, ctx.cwd), shown.as_deref().unwrap_or_default()], " · ");
    let body = numbered.map(|(code, ..)| code).or_else(|| text.map(|t| t.into_owned()));
    if let Some(body) = body.filter(|b| !b.trim().is_empty()) {
        let (shown, cut) = head(&body, ctx.expanded);
        v.blocks = code_block(&shown, path);
        v.truncated = cut;
        v.expandable = true;
    }
}

/// An image the agent looked at; the shell shows it from `images`.
fn image(v: &mut ItemView, verb: (&str, &str), path: &str, detail: Option<&str>, running: bool, ctx: Ctx) {
    v.kind = "tool".into();
    v.icon = "image".into();
    v.verb = tense(running, verb.0, verb.1);
    v.title = clip(file_name(path), TITLE_CHARS);
    v.mono = true;
    v.subtitle = join(&[&folder(path, ctx.cwd), detail.unwrap_or_default()], " · ");
    v.text = String::new();
}

fn search(v: &mut ItemView, pattern: &str, scope: &str, output: Option<&str>, running: bool, ctx: Ctx) {
    v.kind = "tool".into();
    v.icon = "search".into();
    v.verb = tense(running, "Searching for", "Searched for");
    v.title = clip(pattern, TITLE_CHARS);
    v.mono = true;
    let output = output.map(without_reminders).filter(|o| !o.trim().is_empty());
    let no_matches = output.as_deref().is_some_and(|o| o.trim().trim_end_matches('.').eq_ignore_ascii_case("no matches found"));
    let output = output.filter(|_| !no_matches);
    if no_matches {
        v.badge = "No matches".into();
        v.badge_tone = "neutral".into();
    }
    let found = output.as_deref().map(|o| o.lines().filter(|l| !l.trim().is_empty()).count());
    let found = found.map(|n| plural(n, "result", "results")).unwrap_or_default();
    let scope = if scope.is_empty() { String::new() } else { format!("in {}", relative(scope, ctx.cwd)) };
    v.subtitle = join(&[&scope, &found], " · ");
    if let Some(o) = output {
        set_output(v, &o, ctx);
        v.expandable = true;
    }
}

fn list(v: &mut ItemView, what: &str, output: Option<&str>, running: bool, ctx: Ctx) {
    v.kind = "tool".into();
    v.icon = "list".into();
    v.verb = tense(running, "Listing", "Listed");
    v.title = clip(relative(what, ctx.cwd), TITLE_CHARS);
    v.mono = true;
    let output = output.map(without_reminders).filter(|o| !o.trim().is_empty());
    if let Some(o) = output {
        v.subtitle = plural(o.lines().filter(|l| !l.trim().is_empty()).count(), "entry", "entries");
        set_output(v, &o, ctx);
        v.expandable = true;
    }
}

// ---- shell commands ----

/// `/bin/zsh -lc "…"`, `bash -c '…'`: the command inside.
pub(crate) fn unwrap_shell(command: &str) -> String {
    let trimmed = command.trim();
    let words = split_words(trimmed);
    if let Some(words) = &words
        && words.len() == 3
        && matches!(file_name(&words[0]), "sh" | "bash" | "zsh" | "fish")
        && matches!(words[1].as_str(), "-c" | "-lc" | "-ic" | "-lic")
    {
        return words[2].trim().to_owned();
    }
    trimmed.to_owned()
}

/// Shell words, unquoted; `None` when the text holds anything but plain
/// words (an unclosed quote, `$(`, a backtick).
fn split_words(text: &str) -> Option<Vec<String>> {
    let mut words = vec![];
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => word.push(c),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => {
                            let next = chars.next()?;
                            if !matches!(next, '"' | '\\' | '$' | '`') {
                                word.push('\\');
                            }
                            word.push(next);
                        }
                        '`' => return None,
                        '$' if chars.peek() == Some(&'(') => return None,
                        c => word.push(c),
                    }
                }
            }
            '\\' => {
                in_word = true;
                word.push(chars.next()?);
            }
            '`' => return None,
            '$' if chars.peek() == Some(&'(') => return None,
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    Some(words)
}

/// A shell command split at `&&`, `;`, `||` and `|` outside quotes; `None`
/// when it redirects, runs in the background or holds more than one line.
fn split_command(command: &str) -> Option<Vec<(String, char)>> {
    let mut parts = vec![];
    let mut current = String::new();
    let mut quote = None;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => {
                quote = None;
                current.push(c);
            }
            (Some('"'), '\\') => {
                current.push(c);
                current.extend(chars.next());
            }
            (Some(_), c) => current.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                current.push(c);
            }
            (None, '\\') => {
                current.push(c);
                current.extend(chars.next());
            }
            // Output thrown away (`2>/dev/null`, `2>&1`) changes nothing.
            (None, '>') if current.ends_with(['2', '&']) || current.ends_with(' ') || current.is_empty() => {
                let rest: String = chars.clone().collect();
                let target = rest.trim_start();
                let skip = ["/dev/null", "&1", "&2"].iter().find(|t| target.starts_with(**t))?;
                let used = rest.len() - target.len() + skip.len();
                for _ in 0..rest[..used].chars().count() {
                    chars.next();
                }
                if current.ends_with(['2', '&']) {
                    current.pop();
                }
            }
            (None, '\n' | '>' | '<') => return None,
            (None, '&') if chars.peek() == Some(&'&') => {
                chars.next();
                parts.push((std::mem::take(&mut current), '&'));
            }
            (None, '&') => return None,
            (None, '|') if chars.peek() == Some(&'|') => {
                chars.next();
                parts.push((std::mem::take(&mut current), '&'));
            }
            (None, '|') => parts.push((std::mem::take(&mut current), '|')),
            (None, ';') => parts.push((std::mem::take(&mut current), ';')),
            (None, c) => current.push(c),
        }
    }
    if quote.is_some() {
        return None;
    }
    parts.push((current, ';'));
    Some(parts)
}

/// What a shell command that only looks around does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Explore {
    /// Files read (`cat`, `head`, `sed -n 'a,bp'`), with the lines asked for.
    Read { paths: Vec<String>, range: Option<(u64, u64)> },
    /// A text search (`grep`, `rg`), in these places.
    Search { pattern: String, paths: Vec<String> },
    /// A folder or file listing (`ls`, `find`, `rg --files`).
    List { paths: Vec<String> },
    /// Several looks of different kinds in one command (`cat a; rg x src`).
    Mixed { reads: Vec<String>, patterns: Vec<String>, lists: Vec<String> },
}

/// Programs that only shape what came before them in a pipeline.
fn is_filter(program: &str) -> bool {
    matches!(program, "head" | "tail" | "wc" | "sort" | "uniq" | "cut" | "tr" | "column" | "nl" | "cat" | "less" | "more" | "grep" | "rg" | "awk" | "sed" | "jq" | "fmt" | "fold" | "xargs")
}

/// Options of `grep` and `rg` that take a value.
fn search_flag_takes_value(flag: &str) -> bool {
    matches!(
        flag,
        "-A" | "-B" | "-C" | "-m" | "-g" | "-t" | "-T" | "-M" | "-j" | "-d" | "-D" | "--glob" | "--iglob" | "--type" | "--type-not" | "--max-count"
            | "--context" | "--after-context" | "--before-context" | "--max-columns" | "--threads" | "--include" | "--exclude" | "--exclude-dir"
            | "--type-add" | "--max-depth" | "--maxdepth" | "--sort" | "--sortr" | "--color" | "--colors" | "--encoding" | "--engine"
    )
}

/// `sed -n '12,80p'` → (12, 80); `'5p'` → (5, 5).
fn sed_range(script: &str) -> Option<(u64, u64)> {
    let script = script.strip_suffix('p')?;
    match script.split_once(',') {
        Some((a, b)) => Some((a.trim().parse().ok()?, b.trim().parse().ok()?)),
        None => script.trim().parse().ok().map(|n| (n, n)),
    }
}

/// One simple command that looks around, from its words.
fn explore_words(words: &[String]) -> Option<Explore> {
    let (program, args) = words.split_first()?;
    let program = file_name(program);
    let operands = |args: &[String]| -> Vec<String> { args.iter().filter(|a| !a.starts_with('-') || a.as_str() == "-").cloned().collect() };
    match program {
        "cat" | "bat" | "nl" | "less" | "more" => {
            let paths = operands(args);
            (!paths.is_empty()).then_some(Explore::Read { paths, range: None })
        }
        "head" | "tail" => {
            let mut paths = vec![];
            let mut count = None;
            let mut it = args.iter();
            while let Some(a) = it.next() {
                if a == "-n" || a == "-c" {
                    count = it.next().and_then(|n| n.trim_start_matches('+').parse::<u64>().ok());
                } else if let Some(n) = a.strip_prefix("-n").or_else(|| a.strip_prefix('-')).and_then(|n| n.parse::<u64>().ok()) {
                    count = Some(n);
                } else if !a.starts_with('-') {
                    paths.push(a.clone());
                }
            }
            let range = (program == "head").then_some(count).flatten().map(|n| (1, n));
            (!paths.is_empty()).then_some(Explore::Read { paths, range })
        }
        "sed" => {
            // Only `sed -n 'RANGEp' FILE…`: anything else may edit.
            let mut quiet = false;
            let mut script = None;
            let mut paths = vec![];
            let mut it = args.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "-n" | "--quiet" | "--silent" => quiet = true,
                    "-e" => script = it.next().cloned(),
                    a if a.starts_with('-') => return None,
                    _ if script.is_none() => script = Some(a.clone()),
                    _ => paths.push(a.clone()),
                }
            }
            let range = sed_range(script.as_deref()?.trim());
            (quiet && range.is_some() && !paths.is_empty()).then_some(Explore::Read { paths, range })
        }
        "grep" | "egrep" | "fgrep" | "rg" | "ag" | "ack" => {
            let listing = program == "rg" && args.iter().any(|a| a == "--files");
            let mut pattern = None;
            let mut paths = vec![];
            let mut it = args.iter();
            let mut options_done = false;
            while let Some(a) = it.next() {
                if !options_done && a == "--" {
                    options_done = true;
                } else if !options_done && (a == "-e" || a == "--regexp") {
                    pattern = it.next().cloned();
                } else if !options_done && a.starts_with('-') && a.len() > 1 {
                    // `-E` is grep's extended syntax but rg's encoding (which takes a value).
                    if search_flag_takes_value(a) || (program == "rg" && a == "-E") {
                        it.next();
                    }
                } else if pattern.is_none() && !listing {
                    pattern = Some(a.clone());
                } else {
                    paths.push(a.clone());
                }
            }
            if listing {
                return Some(Explore::List { paths });
            }
            Some(Explore::Search { pattern: pattern?, paths })
        }
        "ls" | "tree" | "eza" | "exa" => Some(Explore::List { paths: operands(args) }),
        "find" | "fd" => {
            // `find` acts with `-exec`/`-delete`: not just a look.
            if args.iter().any(|a| matches!(a.as_str(), "-exec" | "-execdir" | "-delete" | "-ok" | "-x" | "--exec" | "-X" | "--exec-batch")) {
                return None;
            }
            let paths = if program == "find" { args.iter().take_while(|a| !a.starts_with('-') && !a.starts_with('(') && *a != "!").cloned().collect() } else { vec![] };
            Some(Explore::List { paths })
        }
        _ => None,
    }
}

/// What a shell command does, when all it does is look: read files, search
/// text, list folders. `cd DIR &&` before it and filters piped after it
/// (`| head`) leave it a look; anything that may change something does not.
pub(crate) fn explore(command: &str) -> Option<Explore> {
    let parts = split_command(command)?;
    let mut found: Vec<Explore> = vec![];
    let mut after_pipe = false;
    for (part, sep) in &parts {
        let words = split_words(part.trim())?;
        if words.is_empty() {
            after_pipe = *sep == '|';
            continue;
        }
        if after_pipe {
            // A filter shapes the output; it looks at no file of its own.
            if !is_filter(file_name(&words[0])) || file_name(&words[0]) == "xargs" {
                return None;
            }
        } else if matches!(words[0].as_str(), "cd" | "pwd" | "echo" | "printf" | "true") {
            // Moving into a folder, or a label between looks.
        } else {
            found.push(explore_words(&words)?);
        }
        after_pipe = *sep == '|';
    }
    let mut all = found.clone().into_iter();
    let first = all.next()?;
    let merged = all.try_fold(first, |acc, next| match (acc, next) {
        (Explore::Read { mut paths, .. }, Explore::Read { paths: more, .. }) => {
            paths.extend(more);
            Some(Explore::Read { paths, range: None })
        }
        (Explore::List { mut paths }, Explore::List { paths: more }) => {
            paths.extend(more);
            Some(Explore::List { paths })
        }
        (Explore::Search { pattern, mut paths }, Explore::Search { pattern: other, paths: more }) if pattern == other => {
            paths.extend(more);
            Some(Explore::Search { pattern, paths })
        }
        _ => None,
    });
    merged.or_else(|| {
        let (mut reads, mut patterns, mut lists) = (vec![], vec![], vec![]);
        for look in found {
            match look {
                Explore::Read { paths, .. } => reads.extend(paths),
                Explore::Search { pattern, .. } => patterns.push(pattern),
                Explore::List { paths } => lists.extend(if paths.is_empty() { vec![".".to_owned()] } else { paths }),
                Explore::Mixed { .. } => {}
            }
        }
        Some(Explore::Mixed { reads, patterns, lists })
    })
}

/// "a.rs, b.rs +2".
fn names(paths: &[String]) -> String {
    let mut seen: Vec<&str> = vec![];
    for p in paths {
        let name = file_name(p);
        if !seen.contains(&name) {
            seen.push(name);
        }
    }
    let shown = seen.iter().take(3).copied().collect::<Vec<_>>().join(", ");
    if seen.len() > 3 { format!("{shown} +{}", seen.len() - 3) } else { shown }
}

/// A command without the `cd DIR &&` it starts with: the line shows what it
/// does (the full command is shown when opened).
fn without_cd(command: &str) -> &str {
    let trimmed = command.trim_start();
    if let Some(rest) = trimmed.strip_prefix("cd ")
        && let Some(at) = rest.find("&&")
        && split_words(&rest[..at]).is_some_and(|w| w.len() == 1)
    {
        let after = rest[at + 2..].trim_start();
        if !after.is_empty() {
            return after;
        }
    }
    trimmed
}

/// Claude reports a failed command's code as the first line of its output.
fn exit_line(output: &str) -> Option<(i64, &str)> {
    let rest = output.strip_prefix("Exit code ")?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let code = rest[..digits].parse().ok()?;
    let after = &rest[digits..];
    if !(after.is_empty() || after.starts_with('\n')) {
        return None;
    }
    Some((code, after.strip_prefix('\n').unwrap_or(after)))
}

/// A shell command's row.
pub(crate) fn command(v: &mut ItemView, c: &CommandExecutionItem, ctx: Ctx) {
    let live = running(&c.status);
    let inner = unwrap_shell(&c.input);
    let mut exit = c.exit_code;
    let mut output = c.output.as_deref().map(without_reminders).unwrap_or_default().into_owned();
    if let Some((code, rest)) = exit_line(&output) {
        exit = Some(code);
        output = rest.to_owned();
    }
    let multi_line = inner.contains('\n');
    match explore(&inner) {
        Some(Explore::Read { paths, range }) => {
            read(v, paths.first().map_or("", String::as_str), range, Some(&output), live, ctx);
            if paths.len() > 1 {
                v.title = names(&paths);
                v.subtitle = String::new();
                v.blocks = Arc::default();
                v.truncated = false;
                set_output(v, &output, ctx);
            }
        }
        Some(Explore::Search { pattern, paths }) => {
            let scope = paths.iter().map(|p| relative(p, ctx.cwd)).collect::<Vec<_>>().join(", ");
            search(v, &pattern, &scope, Some(&output), live, ctx);
            // grep and rg end with 1 when nothing matched: not a failure.
            if exit == Some(1) && (output.trim().is_empty() || v.badge == "No matches") {
                exit = Some(0);
                v.status = "completed".into();
                v.badge = "No matches".into();
                v.badge_tone = "neutral".into();
            }
        }
        Some(Explore::List { paths }) => {
            let here = if ctx.cwd.is_empty() { "." } else { file_name(ctx.cwd) };
            let what = if paths.is_empty() { here.to_owned() } else { paths.iter().map(|p| relative(p, ctx.cwd)).collect::<Vec<_>>().join(", ") };
            list(v, &what, Some(&output), live, ctx);
        }
        Some(Explore::Mixed { reads, patterns, lists }) => {
            v.kind = "tool".into();
            v.icon = if reads.is_empty() { "search" } else { "read" }.into();
            v.verb = tense(live, "Exploring", "Explored");
            let quoted: Vec<String> = patterns.iter().map(|p| format!("\"{}\"", clip(p, 40))).collect();
            let listed: Vec<String> = lists.iter().map(|p| relative(p, ctx.cwd).to_owned()).collect();
            v.title = clip(&join(&[&names(&reads), &quoted.join(", "), &names(&listed)], ", "), TITLE_CHARS);
            v.mono = true;
            let mut parts = vec![];
            if !reads.is_empty() {
                parts.push(plural(reads.len(), "file", "files"));
            }
            if !patterns.is_empty() {
                parts.push(plural(patterns.len(), "search", "searches"));
            }
            if !lists.is_empty() {
                parts.push(plural(lists.len(), "listing", "listings"));
            }
            v.subtitle = parts.join(", ");
            set_output(v, &output, ctx);
        }
        None => {
            v.kind = "command".into();
            v.icon = "command".into();
            v.verb = tense(live, "Running", "Ran");
            v.title = clip(first_line(without_cd(&inner)), TITLE_CHARS);
            v.mono = true;
            set_output(v, &output, ctx);
            v.expandable = v.output.is_some() || multi_line;
        }
    }
    // The command as it was run, when the line shows something else.
    if v.kind != "command" || multi_line || v.title != c.input.trim() {
        v.text = inner;
        v.expandable = true;
    }
    if v.kind != "command" {
        v.kind = "command".into();
    }
    v.exit_code = exit;
    match exit {
        Some(code) if code != 0 => {
            v.badge = format!("exit {code}");
            v.badge_tone = "failed".into();
        }
        _ if c.status == TurnItemStatus::Failed && v.badge.is_empty() => {
            v.badge = "failed".into();
            v.badge_tone = "failed".into();
        }
        _ => {}
    }
    slow(v, ctx);
}

/// A long step says how long it took.
fn slow(v: &mut ItemView, ctx: Ctx) {
    if ctx.duration_ms >= SLOW_MS {
        let took = duration_label(ctx.duration_ms);
        v.badge = if v.badge.is_empty() { took } else { format!("{} · {took}", v.badge) };
    }
}

// ---- tool calls ----

/// `mcp__server__tool` as (server, tool).
fn mcp_name(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    Some((server, tool))
}

/// "file_path" → "file path"; "maxResults" → "max results".
fn label(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 4);
    let mut prev_lower = false;
    for c in key.chars() {
        if c == '_' || c == '-' {
            out.push(' ');
            prev_lower = false;
        } else if c.is_uppercase() && prev_lower {
            out.push(' ');
            out.extend(c.to_lowercase());
            prev_lower = false;
        } else {
            out.extend(c.to_lowercase());
            prev_lower = c.is_lowercase() || c.is_ascii_digit();
        }
    }
    out
}

fn looks_like_code(text: &str) -> bool {
    text.contains('/') && !text.contains(' ') || text.contains('\n') || text.contains("::") || text.contains("()") || text.contains('{')
}

/// A tool's arguments, each as a line (`skip`: those its title shows).
fn fields(input: Option<&Value>, skip: &[&str], ctx: Ctx) -> Vec<FieldView> {
    let max = if ctx.expanded { usize::MAX } else { FIELD_CHARS };
    let value_text = |value: &Value| -> Option<(String, bool)> {
        Some(match value {
            Value::Null => return None,
            Value::String(s) if s.trim().is_empty() => return None,
            Value::String(s) => (clip(s, max), looks_like_code(s)),
            Value::Bool(b) => (if *b { "yes" } else { "no" }.to_owned(), false),
            Value::Number(n) => (n.to_string(), false),
            Value::Array(items) if items.is_empty() => return None,
            Value::Array(items) if items.iter().all(Value::is_string) && items.len() <= 12 => {
                (items.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "), false)
            }
            other => (clip(&serde_json::to_string_pretty(other).unwrap_or_default(), max), true),
        })
    };
    match input {
        Some(Value::Object(map)) => map
            .iter()
            .filter(|(k, _)| !skip.contains(&k.as_str()) && !k.starts_with('_'))
            .filter_map(|(k, val)| value_text(val).map(|(value, mono)| FieldView { label: label(k), value, mono }))
            .collect(),
        Some(Value::Null) | None => vec![],
        Some(other) => value_text(other).map(|(value, mono)| vec![FieldView { label: "input".into(), value, mono }]).unwrap_or_default(),
    }
}

/// An MCP result's text parts (`{content: [{type: "text", text}, …]}`).
fn mcp_text(output: &Value) -> Option<String> {
    let parts = output.get("content")?.as_array()?;
    let texts: Vec<&str> = parts.iter().filter(|p| p.get("type").and_then(Value::as_str) == Some("text")).filter_map(|p| p.get("text")?.as_str()).collect();
    Some(texts.join("\n"))
}

/// The images in a tool's result (MCP `{type: "image", data, mimeType}` parts).
pub(crate) fn output_image_count(output: Option<&Value>) -> usize {
    output
        .and_then(|o| o.get("content"))
        .and_then(Value::as_array)
        .map_or(0, |parts| parts.iter().filter(|p| p.get("type").and_then(Value::as_str) == Some("image")).count())
}

/// A tool's result as text: a string as it is (JSON in it laid out), an MCP
/// result's text, anything else laid out as JSON.
fn output_text(output: Option<&Value>) -> String {
    let pretty_json = |s: &str| -> Option<String> {
        let t = s.trim();
        if !(t.starts_with('{') && t.ends_with('}') || t.starts_with('[') && t.ends_with(']')) {
            return None;
        }
        let parsed: Value = serde_json::from_str(t).ok()?;
        serde_json::to_string_pretty(&parsed).ok()
    };
    match output {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => {
            let s = without_reminders(s);
            pretty_json(&s).unwrap_or_else(|| s.into_owned())
        }
        Some(other) => match mcp_text(other) {
            Some(text) => pretty_json(&text).unwrap_or(text),
            None => serde_json::to_string_pretty(other).unwrap_or_default(),
        },
    }
}

/// Claude's web search: "Web search results for query: …", a `Links: [...]`
/// line, then what the search found in words.
fn web_links(output: &str) -> (Vec<LinkView>, String) {
    let mut links = vec![];
    let mut rest = vec![];
    for line in output.lines() {
        if let Some(json) = line.trim().strip_prefix("Links: ")
            && let Ok(Value::Array(items)) = serde_json::from_str::<Value>(json)
        {
            links.extend(items.iter().filter_map(|i| {
                let url = i.get("url")?.as_str()?.to_owned();
                Some(link(i.get("title").and_then(Value::as_str).unwrap_or_default(), &url, ""))
            }));
            continue;
        }
        if line.starts_with("Web search results for query:") {
            continue;
        }
        rest.push(line);
    }
    (links, rest.join("\n").trim().to_owned())
}

fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    host.strip_prefix("www.").unwrap_or(host)
}

fn link(title: &str, url: &str, snippet: &str) -> LinkView {
    let host = host_of(url).to_owned();
    LinkView { title: if title.trim().is_empty() { host.clone() } else { title.trim().to_owned() }, url: url.to_owned(), host, snippet: snippet.trim().to_owned() }
}

fn web_search_look(v: &mut ItemView, query: &str, links: Vec<LinkView>, summary: &str, running: bool, ctx: Ctx) {
    v.kind = "tool".into();
    v.icon = "web".into();
    v.verb = tense(running, "Searching the web for", "Searched the web for");
    v.title = clip(query, TITLE_CHARS);
    if !links.is_empty() {
        v.subtitle = plural(links.len(), "result", "results");
    }
    v.links = Arc::new(links);
    set_output(v, summary, ctx);
    v.expandable = !v.links.is_empty() || v.output.is_some();
}

/// "Fetched URL (HTTP 200 OK, text/html, 87601 characters, …)." → "HTTP 200 · 87,601 characters".
fn fetch_summary(header: &str) -> Option<String> {
    let inside = header.split_once("(HTTP ")?.1;
    let mut parts = inside.split(", ");
    let status = parts.next()?.split_whitespace().next()?;
    let size = parts.find_map(|p| p.trim_end_matches([')', '.']).strip_suffix(" characters").and_then(|n| n.trim().parse::<usize>().ok()));
    Some(match size {
        Some(n) => format!("HTTP {status} · {}", plural(n, "character", "characters")),
        None => format!("HTTP {status}"),
    })
}

/// "in 20 min", "in 45s".
fn delay_label(seconds: u64) -> String {
    if seconds >= 3600 {
        format!("in {}h {}m", seconds / 3600, seconds % 3600 / 60)
    } else if seconds >= 60 {
        format!("in {} min", seconds / 60)
    } else {
        format!("in {seconds}s")
    }
}

/// What the tool's arguments say it works on (a path, a command, a query).
fn target(input: Option<&Value>) -> Option<(&'static str, String)> {
    for key in ["file_path", "path", "Path", "AbsolutePath", "command", "CommandLine", "pattern", "url", "Url", "query", "Query", "name", "description", "title", "id"] {
        if let Some(text) = input.and_then(|i| i.get(key)).and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty()) {
            return Some((key, text.to_owned()));
        }
    }
    None
}

/// A tool row that only says what it did: opening it shows its arguments
/// and result if it has any beyond what the line says.
fn simple(v: &mut ItemView, icon: &str, verb: String, title: &str, input: Option<&Value>, skip: &[&str], output: &str, ctx: Ctx) {
    v.kind = "tool".into();
    v.icon = icon.into();
    v.verb = verb;
    v.title = clip(title, TITLE_CHARS);
    v.fields = Arc::new(fields(input, skip, ctx));
    set_output(v, output, ctx);
    v.expandable = !v.fields.is_empty() || v.output.is_some();
}

/// A file edit sent as a tool call (Claude's subagents' `Edit`/`Write`).
fn edit_tool(v: &mut ItemView, name: &str, input: &Value, failed_output: Option<&str>, running: bool, ctx: Ctx) -> bool {
    let Some(path) = input.get("file_path").and_then(Value::as_str) else { return false };
    let (old, new) = match name {
        "Write" => (String::new(), input.get("content").and_then(Value::as_str).unwrap_or_default().to_owned()),
        "MultiEdit" => {
            let edits = input.get("edits").and_then(Value::as_array).cloned().unwrap_or_default();
            let pick = |k: &str| edits.iter().filter_map(|e| e.get(k)?.as_str()).collect::<Vec<_>>().join("\n");
            (pick("old_string"), pick("new_string"))
        }
        _ => (
            input.get("old_string").and_then(Value::as_str).unwrap_or_default().to_owned(),
            input.get("new_string").and_then(Value::as_str).unwrap_or_default().to_owned(),
        ),
    };
    let diff = crate::diff::diff_texts(path, &old, &new);
    file_change_look(v, path, Some(&diff), name == "Write", failed_output, running, ctx);
    v.diff = Some(Arc::new(diff));
    true
}

/// The line of a file change: "Edited foo.rs" with "+12 −3".
pub(crate) fn file_change_look(v: &mut ItemView, path: &str, diff: Option<&DiffFileView>, created: bool, error: Option<&str>, running: bool, ctx: Ctx) {
    v.kind = "file_change".into();
    v.icon = "edit".into();
    let deleted = diff.is_some_and(|d| d.change == "deleted");
    let created = created || diff.is_some_and(|d| d.change == "added");
    v.verb = match (running, created, deleted) {
        (true, true, _) => "Creating",
        (true, _, true) => "Deleting",
        (true, ..) => "Editing",
        (false, true, _) => "Created",
        (false, _, true) => "Deleted",
        _ => "Edited",
    }
    .into();
    v.title = clip(file_name(path), TITLE_CHARS);
    v.mono = true;
    v.subtitle = folder(path, ctx.cwd);
    if let Some(d) = diff.filter(|d| d.additions + d.deletions > 0) {
        v.badge = format!("+{} −{}", d.additions, d.deletions);
    }
    if let Some(error) = error.map(str::trim).filter(|e| !e.is_empty()) {
        v.subtitle = join(&[&v.subtitle, &clip(first_line(error), 120)], " · ");
        v.badge_tone = "failed".into();
        if v.badge.is_empty() {
            v.badge = "failed".into();
        }
        v.text = error.to_owned();
    }
    v.expandable = diff.is_some_and(|d| !d.hunks.is_empty()) || !v.text.is_empty();
}

/// A tool call that has no record of its own.
pub(crate) fn dynamic(v: &mut ItemView, t: &DynamicToolItem, ctx: Ctx) {
    let live = running(&t.status);
    let failed = t.status == TurnItemStatus::Failed;
    let name = t.tool_name.as_deref().or(t.title.as_deref()).unwrap_or("Tool").trim();
    let input = t.input.as_ref();
    let output = output_text(t.output.as_ref());
    let s = |keys: &[&str]| str_of(input, keys);
    match name {
        "Read" | "read_file" | "ReadFile" | "view_file" | "NotebookRead" | "read" => {
            let path = s(&["file_path", "path", "notebook_path", "Path", "AbsolutePath", "filePath"]).or(t.viewed_image_path.as_deref()).unwrap_or_default();
            if is_image_path(path) || t.viewed_image_path.is_some() {
                image(v, ("Viewing", "Viewed"), t.viewed_image_path.as_deref().unwrap_or(path), image_detail(&output).as_deref(), live, ctx);
            } else {
                let from = num_of(input, &["offset", "StartLine", "start_line"]);
                let to = num_of(input, &["EndLine", "end_line"]).or_else(|| Some(from.unwrap_or(1) + num_of(input, &["limit"])? - 1));
                read(v, path, from.zip(to), Some(&output), live, ctx);
            }
        }
        "view_image" | "ViewImage" => {
            let path = t.viewed_image_path.as_deref().or(s(&["path", "file_path"])).unwrap_or_default();
            image(v, ("Viewing", "Viewed"), path, None, live, ctx);
        }
        _ if name.starts_with("File ") => {
            // Antigravity names a file read by its title.
            let path = s(&["Path", "AbsolutePath", "path", "file_path"]).unwrap_or(&name[5..]);
            if is_image_path(path) || t.viewed_image_path.is_some() {
                image(v, ("Viewing", "Viewed"), t.viewed_image_path.as_deref().unwrap_or(path), None, live, ctx);
            } else {
                let range = num_of(input, &["StartLine", "start_line"]).zip(num_of(input, &["EndLine", "end_line"]));
                read(v, path, range, Some(&output), live, ctx);
            }
        }
        "Grep" | "grep" | "grep_search" | "search" | "Search" | "codebase_search" | "FileSearch" => {
            let pattern = s(&["pattern", "query", "Query", "SearchPattern", "regex"]).unwrap_or_default();
            let scope = s(&["path", "SearchPath", "Directory", "glob", "include"]).unwrap_or_default();
            search(v, pattern, scope, Some(&output), live, ctx);
        }
        "Glob" | "glob" | "LS" | "ls" | "list_dir" | "list" | "find_by_name" => {
            let what = s(&["pattern", "path", "DirectoryPath", "SearchDirectory", "Pattern"]).unwrap_or(".");
            list(v, what, Some(&output), live, ctx);
        }
        "WebSearch" | "web_search" | "search_web" => {
            let (links, summary) = web_links(&output);
            web_search_look(v, s(&["query", "Query", "q"]).unwrap_or_default(), links, &summary, live, ctx);
        }
        "WebFetch" | "web_fetch" | "read_url_content" | "fetch" => {
            let url = s(&["url", "Url", "URL"]).unwrap_or_default();
            let (header, body) = output.split_once('\n').unwrap_or((&output, ""));
            let summary = fetch_summary(header);
            let shown_body = if summary.is_some() { body.trim_start() } else { output.as_str() };
            simple(v, "fetch", tense(live, "Fetching", "Fetched"), url.split_once("://").map_or(url, |(_, r)| r), input, &["url", "Url", "URL"], shown_body, ctx);
            v.subtitle = summary.unwrap_or_default();
            v.mono = true;
        }
        "ToolSearch" => {
            let query = s(&["query"]).unwrap_or_default();
            let tools = query.strip_prefix("select:").map_or_else(|| query.to_owned(), |list| list.split(',').map(str::trim).collect::<Vec<_>>().join(", "));
            let verb = if query.starts_with("select:") { tense(live, "Loading", "Loaded") } else { tense(live, "Looking up tools for", "Looked up tools for") };
            simple(v, "tool", verb, &tools, None, &[], &output, ctx);
        }
        "Skill" => {
            let shown = if output.trim_start().starts_with("Launching skill") { "" } else { output.as_str() };
            simple(v, "skill", tense(live, "Using skill", "Used skill"), s(&["skill", "name"]).unwrap_or_default(), input, &["skill", "name"], shown, ctx);
        }
        "AskUserQuestion" => {
            let question = input
                .and_then(|i| i.get("questions"))
                .and_then(Value::as_array)
                .and_then(|q| q.first())
                .and_then(|q| q.get("question"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            simple(v, "question", tense(live, "Asking", "Asked"), question, None, &[], &output, ctx);
        }
        "EnterPlanMode" => simple(v, "tool", tense(live, "Entering plan mode", "Entered plan mode"), "", None, &[], "", ctx),
        "SendMessage" => simple(v, "agent", tense(live, "Messaging", "Messaged"), s(&["to", "recipient"]).unwrap_or_default(), input, &["to", "recipient"], &output, ctx),
        "TaskStop" | "KillShell" | "KillBash" => {
            simple(v, "agent", tense(live, "Stopping", "Stopped"), s(&["task_id", "shell_id", "id"]).unwrap_or_default(), None, &[], &output, ctx);
        }
        "TaskOutput" | "BashOutput" => {
            simple(v, "agent", tense(live, "Checking output of", "Checked output of"), s(&["task_id", "bash_id", "shell_id", "id"]).unwrap_or_default(), None, &[], &output, ctx);
        }
        "ListAgents" => simple(v, "agent", tense(live, "Listing agents", "Listed agents"), "", None, &[], &output, ctx),
        "TaskCreate" => simple(v, "tool", tense(live, "Adding task", "Added task"), s(&["subject", "title", "description"]).unwrap_or_default(), None, &[], "", ctx),
        "TaskUpdate" => {
            let what = s(&["subject", "status", "taskId", "task_id"]).unwrap_or_default();
            simple(v, "tool", tense(live, "Updating task", "Updated task"), what, None, &[], "", ctx);
        }
        "TaskList" | "TaskGet" => simple(v, "tool", tense(live, "Checking tasks", "Checked tasks"), "", None, &[], "", ctx),
        "Monitor" => {
            simple(v, "wait", tense(live, "Watching", "Watched"), s(&["description", "command"]).unwrap_or_default(), input, &["description"], &output, ctx);
        }
        "ScheduleWakeup" => {
            let when = num_of(input, &["delaySeconds"]).map(delay_label).unwrap_or_default();
            simple(v, "wait", tense(live, "Scheduling a wakeup", "Scheduled a wakeup"), &when, input, &["delaySeconds", "prompt"], "", ctx);
        }
        "sleep" => {
            let when = num_of(input, &["durationMs"]).map(|ms| duration_label(ms as i64)).unwrap_or_default();
            simple(v, "wait", tense(live, "Waiting", "Waited"), &when, None, &[], "", ctx);
        }
        "imageGeneration" | "generate_image" | "image_generation" => {
            let prompt = s(&["revisedPrompt", "prompt", "Prompt"]).unwrap_or_default();
            simple(v, "image", tense(live, "Generating image", "Generated image"), &clip(first_line(prompt), 120), input, &["savedPath", "path", "result"], "", ctx);
        }
        "Edit" | "Write" | "MultiEdit" if input.is_some_and(|i| edit_tool(v, name, i, failed.then_some(output.as_str()), live, ctx)) => {}
        _ if mcp_name(name).is_some() => {
            let (server, tool) = mcp_name(name).unwrap_or_default();
            simple(v, "mcp", tense(live, "Calling", "Called"), &tool.replace('_', " "), input, &[], &output, ctx);
            v.subtitle = server.replace('_', " ");
        }
        _ => {
            let shown = target(input);
            let skip: Vec<&str> = shown.iter().map(|(key, _)| *key).collect();
            let title = shown.as_ref().map(|(_, t)| first_line(t)).unwrap_or_default();
            simple(v, "tool", name.to_owned(), title, input, &skip, &output, ctx);
            v.mono = shown.is_some_and(|(key, _)| !matches!(key, "description" | "title" | "name"));
        }
    }
    if failed {
        if v.badge.is_empty() {
            v.badge = "failed".into();
        }
        v.badge_tone = "failed".into();
        if v.subtitle.is_empty() && !output.trim().is_empty() {
            v.subtitle = clip(first_line(&output), 120);
        }
        if v.output.is_none() && v.kind == "tool" {
            set_output(v, &output, ctx);
            v.expandable |= v.output.is_some();
        }
    }
    slow(v, ctx);
}

/// "Image · 1200×800" from what an image read answered, when it says.
fn image_detail(output: &str) -> Option<String> {
    let t = output.trim();
    t.starts_with("Image").then(|| clip(first_line(t), 60))
}

pub(crate) fn web_search(v: &mut ItemView, w: &WebSearchItem, ctx: Ctx) {
    let query = w.patterns.as_ref().map(|p| p.join(" · ")).unwrap_or_default();
    let links = w
        .results
        .iter()
        .flatten()
        .filter_map(|r| r.url.as_deref().map(|url| link(r.title.as_deref().unwrap_or_default(), url, r.snippet.as_deref().unwrap_or_default())))
        .collect();
    let query = if query.is_empty() { w.title.clone().unwrap_or_default() } else { query };
    web_search_look(v, &query, links, "", running(&w.status), ctx);
}

pub(crate) fn file_search(v: &mut ItemView, f: &FileSearchItem, ctx: Ctx) {
    let lines: Vec<String> = f
        .results
        .iter()
        .flatten()
        .map(|r| {
            let at = match (r.line, r.column) {
                (Some(l), Some(c)) => format!(":{l}:{c}"),
                (Some(l), None) => format!(":{l}"),
                _ => String::new(),
            };
            let preview = r.preview.as_deref().map(|p| format!("  {}", p.trim())).unwrap_or_default();
            format!("{}{at}{preview}", relative(&r.file_name, ctx.cwd))
        })
        .collect();
    let output = lines.join("\n");
    search(v, f.pattern.as_deref().unwrap_or_default(), "", (!output.is_empty()).then_some(output.as_str()), running(&f.status), ctx);
}

/// An `mcp_tool_call` item (a type this build's schema does not list).
pub(crate) fn mcp_call(v: &mut ItemView, name: &str, input: Option<&Value>, output: Option<&Value>, status: &TurnItemStatus, ctx: Ctx) {
    let live = running(status);
    let (server, tool) = mcp_name(name).unwrap_or(("", name));
    simple(v, "mcp", tense(live, "Calling", "Called"), &tool.replace('_', " "), input, &[], &output_text(output), ctx);
    v.subtitle = server.replace('_', " ");
    if *status == TurnItemStatus::Failed {
        v.badge = "failed".into();
        v.badge_tone = "failed".into();
    }
}

// ---- images ----

/// An image a step showed, and how to ask the server for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ImageSource {
    /// A file on the server's machine (`media-file`).
    File(String),
    /// The `index`th image in the item's result (`tool-output-image`).
    Output(usize),
}

impl ImageSource {
    /// Its key in `Model::asset_urls`.
    pub(crate) fn key(&self, item_id: &str) -> String {
        match self {
            Self::File(path) => format!("media:{path}"),
            Self::Output(index) => format!("tool:{item_id}:{index}"),
        }
    }

    /// The `assets.createUrl` resource for it.
    pub(crate) fn resource(&self, thread_id: &str, item_id: &str) -> Value {
        match self {
            Self::File(path) => serde_json::json!({ "_tag": "media-file", "threadId": thread_id, "path": path }),
            Self::Output(index) => serde_json::json!({ "_tag": "tool-output-image", "threadId": thread_id, "itemId": item_id, "index": index }),
        }
    }

    /// The name shown with it.
    pub(crate) fn name(&self) -> String {
        match self {
            Self::File(path) => file_name(path).to_owned(),
            Self::Output(index) => format!("image-{}", index + 1),
        }
    }
}

/// The images a step looked at or made (at most a few per item).
pub(crate) fn image_sources(item: &TurnItem, cwd: &str) -> Vec<ImageSource> {
    let absolute = |path: &str| -> Option<String> {
        let path = path.trim();
        if path.is_empty() || path.contains('\n') {
            return None;
        }
        if path.starts_with('/') || path.as_bytes().get(1) == Some(&b':') {
            return Some(path.to_owned());
        }
        (!cwd.is_empty()).then(|| format!("{}/{}", cwd.trim_end_matches('/'), path.trim_start_matches("./")))
    };
    let outputs = |output: Option<&Value>| (0..output_image_count(output).min(4)).map(ImageSource::Output);
    match item {
        TurnItem::DynamicTool(t) => {
            let name = t.tool_name.as_deref().unwrap_or_default();
            let mut found = vec![];
            if let Some(path) = t.viewed_image_path.as_deref().and_then(absolute) {
                found.push(ImageSource::File(path));
            } else if matches!(name, "Read" | "read_file" | "view_image" | "ViewImage" | "read") || name.starts_with("File ") {
                let path = str_of(t.input.as_ref(), &["file_path", "path", "Path", "AbsolutePath"]);
                if let Some(path) = path.filter(|p| is_image_path(p)).and_then(absolute) {
                    found.push(ImageSource::File(path));
                }
            } else if matches!(name, "imageGeneration" | "generate_image" | "image_generation") {
                if let Some(path) = str_of(t.input.as_ref(), &["savedPath", "path"]).and_then(absolute) {
                    found.push(ImageSource::File(path));
                }
            }
            found.extend(outputs(t.output.as_ref()));
            found
        }
        TurnItem::Other(unknown) if unknown.item_type() == "mcp_tool_call" => outputs(unknown.record().get("output")).collect(),
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(body: Value) -> TurnItem {
        let mut v = json!({
            "id": "i1", "threadId": "t", "runId": "r", "nodeId": null, "providerThreadId": null,
            "providerTurnId": null, "nativeItemRef": null, "parentItemId": null, "ordinal": 1,
            "status": "completed", "title": null, "startedAt": null, "completedAt": null, "updatedAt": "2026-01-01T00:00:00.000Z",
        });
        for (k, val) in body.as_object().expect("an object") {
            v[k] = val.clone();
        }
        serde_json::from_value(v).expect("a turn item")
    }

    fn row(body: Value) -> ItemView {
        let it = item(body);
        let mut v = ItemView { status: it.status().as_str().to_owned(), ..Default::default() };
        let ctx = Ctx { cwd: "/Users/me/app", ..Default::default() };
        match &it {
            TurnItem::CommandExecution(c) => command(&mut v, c, ctx),
            TurnItem::DynamicTool(t) => dynamic(&mut v, t, ctx),
            TurnItem::WebSearch(w) => web_search(&mut v, w, ctx),
            _ => panic!("not a tool"),
        }
        v
    }

    fn line(v: &ItemView) -> String {
        join(&[&v.verb, &v.title, &v.subtitle, &v.badge], " | ")
    }

    #[test]
    fn shell_reads_searches_and_listings_are_named_for_what_they_do() {
        let cases = [
            ("sed -n '1,135p' src/main.rs", Some(Explore::Read { paths: vec!["src/main.rs".into()], range: Some((1, 135)) })),
            ("cd /x && sed -n 40,80p a.rs | nl", Some(Explore::Read { paths: vec!["a.rs".into()], range: Some((40, 80)) })),
            ("cat a.rs b.rs", Some(Explore::Read { paths: vec!["a.rs".into(), "b.rs".into()], range: None })),
            ("head -n 50 README.md", Some(Explore::Read { paths: vec!["README.md".into()], range: Some((1, 50)) })),
            ("rg -n 'fn main' src --glob '*.rs'", Some(Explore::Search { pattern: "fn main".into(), paths: vec!["src".into()] })),
            ("grep -rn -A 3 \"TODO\" apps | head -20", Some(Explore::Search { pattern: "TODO".into(), paths: vec!["apps".into()] })),
            ("rg --files -g '*.swift' apps", Some(Explore::List { paths: vec!["apps".into()] })),
            ("ls -la src", Some(Explore::List { paths: vec!["src".into()] })),
            ("find . -name '*.rs'", Some(Explore::List { paths: vec![".".into()] })),
            ("sed -i 's/a/b/' x.rs", None),
            ("cat a.rs > b.rs", None),
            ("find . -name '*.tmp' -delete", None),
            ("cargo test", None),
            ("grep x a | xargs rm", None),
            ("echo $(cat a)", None),
            ("ls -la snapshots/mac 2>/dev/null | head -40", Some(Explore::List { paths: vec!["snapshots/mac".into()] })),
            ("rg x src 2>&1 | head", Some(Explore::Search { pattern: "x".into(), paths: vec!["src".into()] })),
            ("grep -E 'a|b' log.txt", Some(Explore::Search { pattern: "a|b".into(), paths: vec!["log.txt".into()] })),
            ("cat a.rs > /tmp/b", None),
            ("cd /x && sed -n 1,60p a.rs; echo ---; grep -rn 'fn b' src", Some(Explore::Mixed { reads: vec!["a.rs".into()], patterns: vec!["fn b".into()], lists: vec![] })),
        ];
        for (command, expected) in cases {
            assert_eq!(explore(command), expected, "{command}");
        }
        assert_eq!(unwrap_shell("/bin/zsh -lc \"sed -n '1,135p' justfile\""), "sed -n '1,135p' justfile");
        assert_eq!(unwrap_shell("bash -c 'ls'"), "ls");
    }

    #[test]
    fn commands_read_as_one_line() {
        let read = row(json!({ "type": "command_execution", "input": "/bin/zsh -lc \"sed -n '1,3p' src/lib.rs\"", "output": "a\nb\nc", "exitCode": 0 }));
        assert_eq!(line(&read), "Read | lib.rs | src · lines 1–3");
        assert_eq!((read.kind.as_str(), read.icon.as_str()), ("command", "read"));
        assert_eq!(read.text, "sed -n '1,3p' src/lib.rs", "the command as run, when opened");
        assert_eq!(read.blocks[0].language, "rs");

        let none = row(json!({ "type": "command_execution", "status": "failed", "input": "rg -n needle /Users/me/app/src", "output": "", "exitCode": 1 }));
        assert_eq!(line(&none), "Searched for | needle | in src | No matches");
        assert_eq!((none.status.as_str(), none.badge_tone.as_str()), ("completed", "neutral"));

        let failed = row(json!({ "type": "command_execution", "status": "failed", "input": "cargo test", "output": "Exit code 101\nerror: test failed", "exitCode": 1 }));
        assert_eq!(line(&failed), "Ran | cargo test | exit 101");
        assert_eq!(failed.output.as_deref(), Some("error: test failed"));
        assert!(failed.expandable && failed.text.is_empty());

        let moved = row(json!({ "type": "command_execution", "input": "cd /Users/me/app && cargo build", "output": "ok", "exitCode": 0 }));
        assert_eq!((moved.title.as_str(), moved.text.as_str()), ("cargo build", "cd /Users/me/app && cargo build"));

        let quiet = row(json!({ "type": "command_execution", "input": "mkdir -p out", "output": "", "exitCode": 0 }));
        assert!(!quiet.expandable, "nothing more to show");
    }

    #[test]
    fn tools_say_what_they_did_without_json() {
        let read = row(json!({ "type": "dynamic_tool", "toolName": "Read", "input": { "file_path": "/Users/me/app/src/present.rs", "offset": 60, "limit": 3 }, "output": "    60\tfn a() {\n    61\t}\n    62\t" }));
        assert_eq!(line(&read), "Read | present.rs | src · lines 60–62");
        assert_eq!(read.blocks[0].text, "fn a() {\n}\n");

        let png = row(json!({ "type": "dynamic_tool", "toolName": "Read", "input": { "file_path": "/tmp/shot.png" }, "output": "" }));
        assert_eq!((png.icon.as_str(), png.verb.as_str(), png.title.as_str()), ("image", "Viewed", "shot.png"));
        assert_eq!(image_sources(&item(json!({ "type": "dynamic_tool", "toolName": "Read", "input": { "file_path": "/tmp/shot.png" } })), ""), vec![ImageSource::File("/tmp/shot.png".into())]);

        let skill = row(json!({ "type": "dynamic_tool", "toolName": "Skill", "input": { "skill": "dataviz" }, "output": "Launching skill: dataviz" }));
        assert_eq!(line(&skill), "Used skill | dataviz");
        assert!(!skill.expandable);

        let loaded = row(json!({ "type": "dynamic_tool", "toolName": "ToolSearch", "input": { "query": "select:Monitor,WebSearch", "max_results": 2 }, "output": "" }));
        assert_eq!(line(&loaded), "Loaded | Monitor, WebSearch");
        assert!(!loaded.expandable);

        let web = row(json!({ "type": "dynamic_tool", "toolName": "WebSearch", "input": { "query": "rust crux" },
            "output": "Web search results for query: \"rust crux\"\n\nLinks: [{\"title\":\"Crux\",\"url\":\"https://www.github.com/redbadger/crux\"}]\n\nCrux is a framework." }));
        assert_eq!(line(&web), "Searched the web for | rust crux | 1 result");
        assert_eq!((web.links[0].host.as_str(), web.output.as_deref()), ("github.com", Some("Crux is a framework.")));

        let fetch = row(json!({ "type": "dynamic_tool", "toolName": "WebFetch", "input": { "url": "https://docs.rs/x", "prompt": "What is x?" },
            "output": "Fetched https://docs.rs/x (HTTP 200 OK, text/html, 4503 characters).\nx is a crate." }));
        assert_eq!(line(&fetch), "Fetched | docs.rs/x | HTTP 200 · 4,503 characters");
        assert_eq!(fetch.fields[0].label, "prompt");

        let mcp = row(json!({ "type": "dynamic_tool", "toolName": "mcp__t3__t3_thread_list", "input": { "limit": 8 }, "output": "{\"threads\":[]}" }));
        assert_eq!(line(&mcp), "Called | t3 thread list | t3");
        assert_eq!(mcp.fields.as_slice(), &[FieldView { label: "limit".into(), value: "8".into(), mono: false }]);
        assert_eq!(mcp.output.as_deref(), Some("{\n  \"threads\": []\n}"));

        let agy = row(json!({ "type": "dynamic_tool", "toolName": "File apps/x/Theme.swift", "input": { "Path": "/Users/me/app/apps/x/Theme.swift", "StartLine": 1, "EndLine": 100 } }));
        assert_eq!(line(&agy), "Read | Theme.swift | apps/x · lines 1–100");
        assert!(!agy.expandable, "no content came");

        let wait = row(json!({ "type": "dynamic_tool", "toolName": "ScheduleWakeup", "input": { "delaySeconds": 1200, "reason": "CI", "prompt": "check" } }));
        assert_eq!(line(&wait), "Scheduled a wakeup | in 20 min");
        assert_eq!(wait.fields.len(), 1, "the reason, not the prompt");

        let other = row(json!({ "type": "dynamic_tool", "toolName": "Frobnicate", "input": { "path": "/Users/me/app/a.txt", "level": 3 } }));
        assert_eq!(line(&other), "Frobnicate | /Users/me/app/a.txt");
        assert_eq!(other.fields.len(), 1);
    }

    #[test]
    fn mcp_images_are_asked_for_by_index() {
        let it = item(json!({ "type": "dynamic_tool", "toolName": "mcp__cua__js", "input": { "code": "1" },
            "output": { "content": [{ "type": "text", "text": "ok" }, { "type": "image", "mimeType": "image/jpeg", "data": "AAAA" }] } }));
        assert_eq!(image_sources(&it, ""), vec![ImageSource::Output(0)]);
        let v = row(json!({ "type": "dynamic_tool", "toolName": "mcp__cua__js", "input": { "code": "1" },
            "output": { "content": [{ "type": "text", "text": "ok" }, { "type": "image", "mimeType": "image/jpeg", "data": "AAAA" }] } }));
        assert_eq!(v.output.as_deref(), Some("ok"), "no base64 in the text");
    }
}
