//! What Antigravity's agent sends during a turn, made fit to record (t3code
//! `acp/AntigravityProtocol.ts`, `AntigravityEditLocation.ts`,
//! `AntigravityTurnOutcome.ts`, `AntigravityProfilePaths.ts`):
//! - tool payloads bounded (the tail of long text, no image data), with its
//!   native field names (`CommandLine`, `Cwd`, `combinedOutput`…) read;
//! - edits placed: the agent reports bare old/new snippets, which are put at
//!   their only match in the file before the edit, else marked as snippets;
//! - reads the client served marked done (the agent never completes them);
//! - the prompt's attachments as the agent takes them;
//! - a turn's real outcome from the agent's own session log, as it reports
//!   upstream failures (a quota, an API error) as a reply and `end_turn`.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "Antigravity's ACP tool calls and content blocks, read and bounded as its agent sends them")]

use crate::adapters::turn::Work;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use domovoi_model::typed::FileChangeDetail;

const TEXT_LIMIT: usize = 8_000;
pub const TRUNCATED: &str = "[Earlier output truncated]\n\n";
const PRE_EDIT_FILE_LIMIT: u64 = 1024 * 1024;
const EDIT_DIFF_BUDGET: i64 = 1_000;
const EDIT_TEXT_BUDGET: usize = 256 * 1024;
const SNIPPET_LABEL: &str = " snippet, line unknown";

/// The tail of a long text (the end of output is what matters), marked as cut.
pub fn bound_text(text: &str, limit: usize) -> String {
    let count = text.chars().count();
    if count <= limit {
        return text.to_owned();
    }
    let tail: String = text.chars().skip(count - limit).collect();
    format!("{TRUNCATED}{tail}")
}

struct Budget {
    nodes: i64,
    text: i64,
}

fn sanitize(value: &Value, budget: &mut Budget, depth: u32) -> Option<Value> {
    budget.nodes -= 1;
    if depth > 12 || budget.nodes < 0 {
        return None;
    }
    match value {
        Value::String(s) => {
            if s.as_bytes().get(..11).is_some_and(|p| p.eq_ignore_ascii_case(b"data:image/")) || budget.text <= 0 {
                return None;
            }
            let text = bound_text(s, TEXT_LIMIT.min(budget.text as usize));
            budget.text -= text.chars().count() as i64;
            Some(Value::String(text))
        }
        Value::Array(items) => {
            let mut out = vec![];
            for item in items {
                if budget.nodes <= 0 {
                    break;
                }
                if let Some(v) = sanitize(item, budget, depth + 1) {
                    out.push(v);
                }
            }
            Some(Value::Array(out))
        }
        Value::Object(fields) => {
            let image = fields.get("type").and_then(Value::as_str) == Some("image");
            let image_mime = fields.get("mimeType").and_then(Value::as_str).is_some_and(|m| m.starts_with("image/"));
            let combined = fields.get("combinedOutput").or(fields.get("combined_output"));
            let mut out = Map::new();
            for (key, entry) in fields {
                if budget.nodes <= 0 {
                    break;
                }
                let skip = (image && (key == "data" || key == "blob"))
                    || (key == "blob" && image_mime)
                    || ((key == "formatted_output" || key == "formattedOutput") && combined == Some(entry));
                if skip {
                    continue;
                }
                if let Some(v) = sanitize(entry, budget, depth + 1) {
                    out.insert(key.clone(), v);
                }
            }
            Some(Value::Object(out))
        }
        other => Some(other.clone()),
    }
}

/// A tool payload with its text bounded (t3code `sanitizeAntigravityToolPayload`).
pub fn sanitize_payload(payload: &Value) -> Value {
    sanitize(payload, &mut Budget { nodes: 512, text: 64_000 }, 0).unwrap_or(Value::Null)
}

/// A `tool_call` or `tool_call_update` with every part bounded
/// (t3code `normalizeAntigravitySessionUpdate`). Other updates are left as they are.
pub fn normalize_update(u: &mut Value) {
    if !matches!(u["sessionUpdate"].as_str(), Some("tool_call" | "tool_call_update")) {
        return;
    }
    let Some(fields) = u.as_object_mut() else { return };
    if let Some(Value::Array(content)) = fields.get("content") {
        let mut budget = Budget { nodes: 512, text: 32_000 };
        let bounded: Vec<Value> = content.iter().filter_map(|c| sanitize(c, &mut budget, 0)).filter(|c| c.get("type").is_some()).collect();
        fields.insert("content".into(), Value::Array(bounded));
    }
    if let Some(Value::String(title)) = fields.get("title") {
        let bounded = bound_text(title, TEXT_LIMIT);
        fields.insert("title".into(), Value::String(bounded));
    }
    for key in ["rawInput", "rawOutput"] {
        if let Some(v) = fields.get(key).filter(|v| !v.is_null()) {
            let bounded = sanitize_payload(v);
            fields.insert(key.into(), bounded);
        }
    }
    if let Some(meta) = fields.get("_meta").filter(|v| !v.is_null()) {
        let bounded = sanitize_payload(meta);
        fields.insert("_meta".into(), if bounded.is_object() { bounded } else { Value::Null });
    }
}

/// The first of several native spellings of a field that is a string.
fn field<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| v[*k].as_str())
}

fn int_field(v: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|k| v[*k].as_i64())
}

/// A tool call's command, as the agent spells it.
fn command_of(call: &Value) -> Option<String> {
    let (input, output) = (&call["rawInput"], &call["rawOutput"]);
    field(input, &["CommandLine", "command_line", "commandLine", "command"]).or(field(output, &["commandLine", "command_line"])).map(str::trim).filter(|c| !c.is_empty()).map(|c| bound_text(c, TEXT_LIMIT))
}

/// A local image the agent viewed (`imagePath`): a path, or a local `file:` URL.
fn local_image(output: &Value) -> Option<String> {
    let path = output["imagePath"].as_str().map(str::trim).filter(|p| !p.is_empty() && p.len() <= TEXT_LIMIT)?;
    let path = match path.strip_prefix("file://") {
        Some(rest) => {
            let url = url::Url::parse(&format!("file://{rest}")).ok()?;
            if url.host_str().is_some_and(|h| h != "localhost" && !h.is_empty()) {
                return None;
            }
            url.to_file_path().ok()?.to_string_lossy().into_owned()
        }
        None if path.contains("://") => return None,
        None => path.to_owned(),
    };
    let ext = Path::new(&path).extension().and_then(|e| e.to_str()).map(str::to_lowercase);
    matches!(ext.as_deref(), Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg")).then_some(path)
}

/// A tool call (all its updates merged) as a turn item: the generic ACP
/// reading, with Antigravity's native fields and placed edit patches.
/// `patch` is the edit's patch, worked out while the edit was pending.
pub fn tool_work(call: &Value, patch: Option<&EditPatch>) -> Work {
    let command = command_of(call);
    let kind = call["kind"].as_str().map(str::to_owned).or_else(|| command.is_some().then(|| "execute".to_owned()));
    let mut merged = call.clone();
    if let Some(k) = &kind {
        merged["kind"] = json!(k);
    }
    match kind.as_deref() {
        Some("execute") => {
            let output = &call["rawOutput"];
            let native = field(output, &["combinedOutput", "combined_output"]).map(|o| bound_text(o, TEXT_LIMIT));
            match crate::adapters::acp::tool_work(&merged) {
                Work::Command { input, output: generic, exit_code } => Work::Command {
                    input: command.unwrap_or(input),
                    output: native.or(generic),
                    exit_code: int_field(output, &["exitCode", "exit_code"]).or(exit_code),
                },
                other => other,
            }
        }
        Some("edit") => match crate::adapters::acp::tool_work(&merged) {
            Work::FileChange { file_name, changes, texts, .. } => match patch {
                Some(EditPatch::Placed(text)) | Some(EditPatch::Snippet(text)) => Work::FileChange { file_name, changes, texts: None, patch: Some(text.clone()), error: None },
                // Too large or uncertain to place: the turn's checkpoint diff shows it.
                Some(EditPatch::Omitted) => Work::FileChange { file_name, changes, texts: None, patch: None, error: None },
                None => {
                    // Snippets with no place would show at line 1: kept only when they are the whole file.
                    let whole = texts.as_ref().is_some_and(|(old, _)| old.is_empty());
                    let path = changes.first().map(|c| c.path.clone()).unwrap_or_default();
                    let changes = if changes.is_empty() { vec![FileChangeDetail { operation: "update".into(), path, old_path: None, file_type: None, mime_type: None }] } else { changes };
                    Work::FileChange { file_name, changes, texts: texts.filter(|_| whole), patch: None, error: None }
                }
            },
            other => other,
        },
        _ => match crate::adapters::acp::tool_work(&merged) {
            Work::Tool { name, input, output, viewed_image_path } => Work::Tool { name, input, output, viewed_image_path: local_image(&call["rawOutput"]).or(viewed_image_path) },
            other => other,
        },
    }
}

// ---------------------------------------------------------------------------
// Edits
// ---------------------------------------------------------------------------

/// Where an edit goes in its file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditPatch {
    /// A unified patch at the lines the edit replaced.
    Placed(String),
    /// Hunks of snippets whose place is unknown (labelled so).
    Snippet(String),
    /// Too large or uncertain to show; the checkpoint diff is the review.
    Omitted,
}

/// `snake_case` edit arguments in the agent's PascalCase (a present
/// PascalCase value wins, as in the agent).
fn pascal(v: &Value) -> Value {
    const KEYS: [(&str, &str); 5] =
        [("replacement_chunks", "ReplacementChunks"), ("target_content", "TargetContent"), ("replacement_content", "ReplacementContent"), ("start_line", "StartLine"), ("end_line", "EndLine")];
    match v {
        Value::Array(items) => Value::Array(items.iter().map(pascal).collect()),
        Value::Object(fields) => {
            let mut out = Map::new();
            for (key, entry) in fields {
                match KEYS.iter().find(|(snake, _)| snake == key) {
                    Some((_, p)) if fields.get(*p).is_none_or(Value::is_null) => {
                        out.insert((*p).to_owned(), pascal(entry));
                    }
                    Some(_) => {}
                    None => {
                        out.entry(key.clone()).or_insert_with(|| pascal(entry));
                    }
                }
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// A 1-based line the agent gave (missing, 0 or invalid: none).
fn line_of(v: &Value) -> Option<usize> {
    v.as_u64().filter(|l| *l > 0).map(|l| l as usize)
}

struct Chunk {
    target: String,
    replacement: String,
    start: Option<usize>,
    end: Option<usize>,
}

fn chunks_of(raw_input: &Value, locations: &Value) -> Option<Vec<Chunk>> {
    let edit = pascal(raw_input);
    let chunk = |c: &Value| Some((c["TargetContent"].as_str()?.to_owned(), c["ReplacementContent"].as_str()?.to_owned(), line_of(&c["StartLine"]), line_of(&c["EndLine"])));
    let listed: Vec<_> = match edit["ReplacementChunks"].as_array() {
        Some(list) if !list.is_empty() => list.iter().map(chunk).collect::<Option<Vec<_>>>()?,
        _ => vec![chunk(&edit)?],
    };
    let announced = line_of(&locations[0]["line"]);
    let single = listed.len() == 1;
    Some(listed.into_iter().map(|(target, replacement, start, end)| Chunk { target, replacement, start: start.or(if single { announced } else { None }), end }).collect())
}

/// The line where `snippet` starts at its only match beginning in
/// `from..=to`, when the match spans whole lines.
fn unique_start_line(text: &str, snippet: &str, from: usize, to: usize) -> Option<usize> {
    if snippet.is_empty() {
        return None;
    }
    let mut found: Option<(usize, bool)> = None;
    for (index, _) in text.match_indices(snippet) {
        let line = text[..index].matches('\n').count() + 1;
        if line > to {
            break;
        }
        if line < from {
            continue;
        }
        if found.is_some() {
            return None;
        }
        let line_start = text[..index].rfind('\n').map_or(0, |i| i + 1);
        let end = index + snippet.len();
        let whole = index == line_start && (end == text.len() || snippet.ends_with('\n') || text.as_bytes().get(end) == Some(&b'\n'));
        found = Some((line, whole));
    }
    found.filter(|(_, whole)| *whole).map(|(line, _)| line)
}

/// One hunk: where it starts in the old and new text, and its lines.
#[derive(Debug, Clone)]
struct Hunk {
    old_start: usize,
    old_lines: usize,
    new_start: usize,
    new_lines: usize,
    lines: Vec<String>,
}

/// The hunks from `old` to `new` with 4 lines of context, or `None` when it
/// needs more than `budget` changed lines to say.
fn hunks(old: &str, new: &str, budget: i64) -> Option<Vec<Hunk>> {
    hunks_of(old, new, budget, true)
}

/// [`hunks`]; `snippet` gives a snippet without its final newline the
/// line's own (a whole file is diffed as it is).
fn hunks_of(old: &str, new: &str, budget: i64, snippet: bool) -> Option<Vec<Hunk>> {
    let before = old.replace("\r\n", "\n");
    let after = new.replace("\r\n", "\n");
    // A target without its newline keeps the line's own, on both sides.
    let eol = if before.ends_with('\n') || !snippet { "" } else { "\n" };
    let (before, after) = (format!("{before}{eol}"), format!("{after}{eol}"));
    let diff = similar::TextDiff::configure().algorithm(similar::Algorithm::Myers).timeout(std::time::Duration::from_millis(200)).diff_lines(&before, &after);
    let mut out = vec![];
    let mut changed = 0i64;
    for group in diff.grouped_ops(4) {
        let (Some(first), Some(last)) = (group.first(), group.last()) else { continue };
        let (old_range, new_range) = (first.old_range().start..last.old_range().end, first.new_range().start..last.new_range().end);
        let mut lines = vec![];
        for op in &group {
            for change in diff.iter_changes(op) {
                let sign = match change.tag() {
                    similar::ChangeTag::Equal => ' ',
                    similar::ChangeTag::Delete => '-',
                    similar::ChangeTag::Insert => '+',
                };
                if sign != ' ' {
                    changed += 1;
                }
                lines.push(format!("{sign}{}", change.value().strip_suffix('\n').unwrap_or(change.value())));
            }
        }
        out.push(Hunk { old_start: old_range.start + 1, old_lines: old_range.len(), new_start: new_range.start + 1, new_lines: new_range.len(), lines });
    }
    (changed <= budget).then_some(out)
}

fn changed_lines(hunks: &[Hunk]) -> i64 {
    hunks.iter().map(|h| h.lines.iter().filter(|l| l.starts_with('+') || l.starts_with('-')).count() as i64).sum()
}

/// A file's hunks as a unified patch with file headers only (jsdiff's `formatPatch`).
fn format_patch(path: &str, hunks: &[Hunk], label: &str) -> String {
    if hunks.is_empty() {
        return String::new();
    }
    let mut out = format!("--- {path}\n+++ {path}\n");
    for h in hunks {
        // Unified diff: an empty side's start is one lower.
        let old_start = if h.old_lines == 0 { h.old_start - 1 } else { h.old_start };
        let new_start = if h.new_lines == 0 { h.new_start - 1 } else { h.new_start };
        out.push_str(&format!("@@ -{old_start},{} +{new_start},{} @@{label}\n", h.old_lines, h.new_lines));
        for l in &h.lines {
            out.push_str(l);
            out.push('\n');
        }
    }
    out
}

/// A small file's text before an edit (larger or unreadable ones stay unplaced).
pub fn read_pre_edit_file(path: &str) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    (meta.is_file() && meta.len() <= PRE_EDIT_FILE_LIMIT).then(|| std::fs::read_to_string(path).ok()).flatten()
}

/// An edit's patch (t3code `antigravityEditPatch`). `pre_edit` is the
/// file's text while the edit is pending (it still holds the old text).
/// `None` when the call is no edit the agent describes this way.
pub fn edit_patch(path: &str, raw_input: &Value, locations: &Value, pre_edit: Option<&str>) -> Option<EditPatch> {
    let chunks = chunks_of(raw_input, locations)?;
    let size: usize = chunks.iter().map(|c| c.target.len() + c.replacement.len()).sum();
    if size > EDIT_TEXT_BUDGET {
        return Some(EditPatch::Omitted);
    }
    let file = pre_edit.map(|f| f.replace("\r\n", "\n"));
    let mut budget = EDIT_DIFF_BUDGET;
    let mut placed: Vec<(usize, Vec<Hunk>)> = vec![];
    let mut snippets: Vec<Hunk> = vec![];
    for c in &chunks {
        // Whether a changed final line break, or a cleared line, splits, joins
        // or removes lines depends on what follows: only a whole-line delete is certain.
        let ends_line = c.target.ends_with('\n');
        let uncertain = if c.replacement.is_empty() { !ends_line } else { ends_line != c.replacement.ends_with('\n') };
        if uncertain {
            return Some(EditPatch::Omitted);
        }
        let Some(h) = hunks(&c.target, &c.replacement, budget) else { return Some(EditPatch::Omitted) };
        if h.is_empty() && c.target != c.replacement {
            return Some(EditPatch::Omitted);
        }
        budget -= changed_lines(&h);
        if budget < 0 {
            return Some(EditPatch::Omitted);
        }
        let start = file.as_deref().and_then(|f| unique_start_line(f, &c.target.replace("\r\n", "\n"), c.start.unwrap_or(1), c.end.unwrap_or(usize::MAX)));
        match start {
            Some(line) => placed.push((line, h)),
            None => snippets.extend(h),
        }
    }
    // A placed hunk beside an unplaced one would number the unplaced one from line 1.
    if !placed.is_empty() && !snippets.is_empty() {
        return Some(EditPatch::Omitted);
    }
    placed.sort_by_key(|(start, _)| *start);
    let mut shift: i64 = 0;
    let mut shifted = vec![];
    for (start, hs) in placed {
        for h in &hs {
            let mut s = h.clone();
            s.old_start = h.old_start + start - 1;
            s.new_start = ((h.new_start + start - 1) as i64 + shift).max(1) as usize;
            shifted.push(s);
        }
        for h in &hs {
            shift += h.new_lines as i64 - h.old_lines as i64;
        }
    }
    if !shifted.is_empty() {
        let text = format_patch(path, &shifted, "");
        return Some(if text.chars().count() > TEXT_LIMIT { EditPatch::Omitted } else { EditPatch::Placed(text) });
    }
    if !snippets.is_empty() {
        let text = format_patch(path, &snippets, SNIPPET_LABEL);
        return Some(if text.chars().count() > TEXT_LIMIT { EditPatch::Omitted } else { EditPatch::Snippet(text) });
    }
    None
}

/// Whether an edit's arguments are the older snippet form (`TargetContent`,
/// `ReplacementChunks`), whose diff shows only the snippets.
fn snippet_edit(raw_input: &Value) -> bool {
    ["TargetContent", "target_content", "ReplacementChunks", "replacement_chunks"].iter().any(|k| !raw_input[*k].is_null())
}

/// A patch between a file's whole text before and after an edit.
fn whole_file_patch(path: &str, old: &str, new: &str) -> EditPatch {
    if old.len() + new.len() > EDIT_TEXT_BUDGET {
        return EditPatch::Omitted;
    }
    match hunks_of(old, new, EDIT_DIFF_BUDGET, false) {
        Some(h) if !h.is_empty() => {
            let text = format_patch(path, &h, "");
            if text.chars().count() > TEXT_LIMIT { EditPatch::Omitted } else { EditPatch::Placed(text) }
        }
        _ => EditPatch::Omitted,
    }
}

/// The patch of an edit call, as first seen. The native CLI (`edit_file`
/// with `Target` / `Replacement`) reports the whole file before and after,
/// so the diff of the two is exact; the older snippet form is placed while
/// the edit is pending (from the file), else shown as snippets.
pub fn patch_for(call: &Value) -> Option<EditPatch> {
    let diff = call["content"].as_array().and_then(|c| c.iter().find(|b| b["type"] == "diff"))?;
    let path = diff["path"].as_str()?;
    if !snippet_edit(&call["rawInput"])
        && let Some(new) = diff["newText"].as_str()
    {
        if [&call["rawInput"], &diff["oldText"], &diff["newText"]].iter().any(|v| v.to_string().contains("[Earlier output truncated]")) {
            return Some(EditPatch::Omitted);
        }
        return Some(whole_file_patch(path, diff["oldText"].as_str().unwrap_or_default(), new));
    }
    // Bounding keeps only the tail of long text: a cut snippet can't be placed.
    let cut = |v: &Value| v.to_string().contains("[Earlier output truncated]");
    if cut(&call["rawInput"]) || cut(&diff["oldText"]) || cut(&diff["newText"]) {
        return Some(EditPatch::Omitted);
    }
    let pending = call["status"].as_str().is_none_or(|s| s == "pending");
    let file = pending.then(|| read_pre_edit_file(path)).flatten();
    edit_patch(path, &call["rawInput"], &call["locations"], file.as_deref())
}

// ---------------------------------------------------------------------------
// Reads the client served
// ---------------------------------------------------------------------------

/// Reads of the client's files the agent announces (`client_view_file`) and
/// never completes: one Domovoi served is reported as done, and the
/// unrelated error the agent later fails it with is dropped
/// (t3code `makeAntigravityReadRepair`).
#[derive(Default)]
pub struct ReadRepair {
    /// Open reads by tool call: (path, served?).
    open: Vec<(String, String, Option<bool>)>,
    /// Reads served before the agent announced them: (path, served).
    early: Vec<(String, bool)>,
}

fn client_read_path(u: &Value) -> Option<String> {
    let title = u["title"].as_str()?;
    if title != "Running client_view_file" && title != "Run client_view_file?" {
        return None;
    }
    u["locations"][0]["path"].as_str().or(u["rawInput"]["absolute_path"].as_str()).filter(|p| !p.is_empty()).map(str::to_owned)
}

impl ReadRepair {
    /// A `fs/read_text_file` of `path` was served (or refused): the oldest
    /// open read of that path takes it. Returns the call to report done.
    pub fn attempted(&mut self, path: &str, served: bool) -> Option<String> {
        if let Some((call, _, outcome)) = self.open.iter_mut().find(|(_, p, o)| p == path && o.is_none()) {
            *outcome = Some(served);
            return served.then(|| call.clone());
        }
        self.early.push((path.to_owned(), served));
        None
    }

    /// An update, corrected: a served read is completed, its later failure too.
    pub fn repair(&mut self, u: &mut Value) {
        if !matches!(u["sessionUpdate"].as_str(), Some("tool_call" | "tool_call_update")) {
            return;
        }
        let Some(call) = u["toolCallId"].as_str().map(str::to_owned) else { return };
        if let Some(path) = client_read_path(u)
            && !self.open.iter().any(|(c, _, _)| *c == call)
        {
            let early = self.early.iter().position(|(p, _)| *p == path).map(|i| self.early.remove(i).1);
            self.open.push((call.clone(), path, early));
        }
        let served = self.open.iter().find(|(c, _, _)| *c == call).and_then(|(_, _, o)| *o) == Some(true);
        match u["status"].as_str() {
            Some("pending" | "in_progress") if served => u["status"] = json!("completed"),
            Some(status @ ("completed" | "failed")) => {
                let status = status.to_owned();
                self.open.retain(|(c, _, _)| *c != call);
                if status == "failed" && served {
                    u["status"] = json!("completed");
                    if let Some(fields) = u.as_object_mut() {
                        fields.remove("rawOutput");
                    }
                }
            }
            _ => {}
        }
    }
}

/// Why a `/goal` message can't go to Antigravity: it takes `/goal` with an
/// objective only (t3code `ensureGoalCommandSupported`); a bare `/goal`,
/// `edit`, `clear`, `pause` or `resume` would become an objective.
pub fn goal_control(text: &str, attachments: bool) -> Option<&'static str> {
    let rest = text.trim().strip_prefix("/goal")?;
    if attachments || !(rest.is_empty() || rest.starts_with(char::is_whitespace)) {
        return None;
    }
    matches!(rest.trim().to_lowercase().as_str(), "" | "edit" | "clear" | "pause" | "resume")
        .then_some("Antigravity accepts /goal followed by an objective. Interrupt the running turn, then ask Antigravity to stop the current goal.")
}

// ---------------------------------------------------------------------------
// The prompt's attachments
// ---------------------------------------------------------------------------

const IMAGE_TYPES: [&str; 4] = ["image/bmp", "image/jpeg", "image/png", "image/webp"];
const AUDIO_TYPES: [&str; 11] = ["audio/aac", "audio/flac", "audio/mp3", "audio/mpeg", "audio/mp4", "audio/m4a", "audio/x-m4a", "audio/ogg", "audio/wav", "audio/x-wav", "audio/webm"];
const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_AUDIO_BYTES: u64 = 20 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 50 * 1024 * 1024;
const MAX_TEXT_BYTES: u64 = 1024 * 1024;
/// Text files this small are already in the prompt's text (`with_attached_files`).
const INLINED_TEXT_BYTES: u64 = 32 * 1024;
const TEXT_TYPES: [&str; 8] = ["application/json", "application/ld+json", "application/javascript", "application/typescript", "application/xml", "application/yaml", "application/x-yaml", "application/x-sh"];
const TEXT_EXTENSIONS: [&str; 49] = [
    "txt", "md", "mdx", "json", "jsonl", "yaml", "yml", "toml", "xml", "csv", "tsv", "js", "jsx", "mjs", "cjs", "ts", "tsx", "html", "css", "scss", "less", "py", "rs", "go", "java", "kt",
    "swift", "c", "h", "cc", "cpp", "hpp", "cs", "rb", "php", "sh", "bash", "zsh", "sql", "graphql", "svelte", "vue", "log", "diff", "patch", "ini", "conf", "lua", "pl",
];

/// A text attachment by its type or name (t3code's lists).
fn text_file(name: &str, mime: &str) -> bool {
    let ext = Path::new(name).extension().and_then(|e| e.to_str()).map(str::to_lowercase).unwrap_or_default();
    mime.starts_with("text/") || TEXT_TYPES.contains(&mime) || TEXT_EXTENSIONS.contains(&ext.as_str())
}

fn file_url(path: &Path) -> Option<String> {
    url::Url::from_file_path(path).ok().map(String::from)
}

/// The blocks that carry a turn's attachments to the agent, before its
/// text: images and audio as data, PDFs as links to the file. Text files
/// are already in the text (Domovoi inlines small ones), as is every
/// attachment's path, so a file the agent can't take is still named.
pub fn attachment_blocks(images: &[(String, PathBuf)], files: &[(String, String, PathBuf)], caps: &Value) -> Vec<Value> {
    use base64::Engine as _;
    let mut blocks = vec![];
    let mut total = 0u64;
    let mut take = |size: u64, limit: u64| {
        let fits = size <= limit && total + size <= MAX_TOTAL_BYTES;
        if fits {
            total += size;
        }
        fits
    };
    if caps["image"] == true {
        for (mime, path) in images {
            let mime = mime.to_lowercase();
            let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(u64::MAX);
            if IMAGE_TYPES.contains(&mime.as_str()) && take(size, MAX_IMAGE_BYTES)
                && let Ok(bytes) = std::fs::read(path)
            {
                blocks.push(json!({ "type": "image", "mimeType": mime, "data": base64::engine::general_purpose::STANDARD.encode(bytes) }));
            }
        }
    }
    for (name, mime, path) in files {
        let mime = mime.to_lowercase().split(';').next().unwrap_or_default().trim().to_owned();
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(u64::MAX);
        if AUDIO_TYPES.contains(&mime.as_str()) && caps["audio"] == true {
            if take(size, MAX_AUDIO_BYTES)
                && let Ok(bytes) = std::fs::read(path)
            {
                blocks.push(json!({ "type": "audio", "mimeType": mime, "data": base64::engine::general_purpose::STANDARD.encode(bytes) }));
            }
        } else if mime == "application/pdf" && caps["embeddedContext"] == true && take(size, MAX_TOTAL_BYTES)
            && let Some(uri) = file_url(path)
        {
            blocks.push(json!({ "type": "resource_link", "uri": uri, "name": name, "mimeType": mime, "size": size }));
        } else if text_file(name, &mime) && caps["embeddedContext"] == true && size > INLINED_TEXT_BYTES && take(size, MAX_TEXT_BYTES)
            && let Some(uri) = file_url(path)
            && let Some(text) = std::fs::read(path).ok().and_then(|b| String::from_utf8(b).ok()).filter(|t| !t.contains('\0'))
        {
            // Larger than Domovoi inlines in the text: the file itself, as an embedded resource.
            let mime = if mime.is_empty() || mime == "application/octet-stream" { "text/plain".to_owned() } else { mime };
            blocks.push(json!({ "type": "resource", "resource": { "uri": uri, "mimeType": mime, "text": text } }));
        }
    }
    blocks
}

// ---------------------------------------------------------------------------
// The turn's outcome, from the agent's session log
// ---------------------------------------------------------------------------

/// The agent's error step (`CORTEX_STEP_TYPE_ERROR_MESSAGE`).
const ERROR_STEP: i64 = 17;
const MAX_ERROR_PAYLOAD: i64 = 16_384;

/// How a turn really ended, when not as the agent's answer says.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub message: String,
    /// A usage limit: the turn waits for it to reset.
    pub usage_limit: bool,
    pub reset_at: Option<String>,
}

/// `path` below the profile, every part a real entry (no link: one could
/// lead to another session's database).
fn managed_path(root: &Path, parts: &[&str], file: bool) -> Option<PathBuf> {
    let mut current = root.to_path_buf();
    for (i, part) in parts.iter().enumerate() {
        current.push(part);
        let meta = std::fs::symlink_metadata(&current).ok()?;
        let last_is_file = file && i == parts.len() - 1;
        if meta.file_type().is_symlink() || (if last_is_file { !meta.is_file() } else { !meta.is_dir() }) {
            return None;
        }
    }
    (std::fs::canonicalize(&current).ok()? == current).then_some(current)
}

/// The session's log, when it is the session's own file.
fn session_log(profile_home: &Path, session_id: &str) -> Option<PathBuf> {
    if session_id.is_empty() || !session_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return None;
    }
    let root = std::fs::canonicalize(profile_home).ok()?;
    managed_path(&root, &["antigravity-acp", "conversations", &format!("{session_id}.db")], true)
}

fn open_log(path: &Path) -> Option<rusqlite::Connection> {
    let db = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()?;
    let _ = db.busy_timeout(std::time::Duration::from_millis(100));
    let has_steps: bool = db.query_row("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'steps'", [], |_| Ok(true)).unwrap_or(false);
    has_steps.then_some(db)
}

/// Where the session's log stands before a prompt: its last step (-1: none
/// yet, `None`: it can't be read, so only the answer counts).
pub fn log_mark(profile_home: &Path, session_id: &str) -> Option<i64> {
    let Some(path) = session_log(profile_home, session_id) else {
        // No log yet is a start; a log that is a link or not a file is not the session's.
        let root = std::fs::canonicalize(profile_home).ok()?;
        let at = root.join("antigravity-acp").join("conversations").join(format!("{session_id}.db"));
        return std::fs::symlink_metadata(at).is_err().then_some(-1);
    };
    let db = open_log(&path)?;
    db.query_row("SELECT max(idx) FROM steps", [], |r| r.get::<_, Option<i64>>(0)).ok().map(|i| i.unwrap_or(-1))
}

/// The newest step the prompt added: (type, the start of its payload).
fn last_step_after(profile_home: &Path, session_id: &str, mark: i64) -> Option<(i64, Vec<u8>)> {
    let db = open_log(&session_log(profile_home, session_id)?)?;
    db.query_row("SELECT step_type, substr(step_payload, 1, ?1) FROM steps WHERE idx > ?2 ORDER BY idx DESC LIMIT 1", rusqlite::params![MAX_ERROR_PAYLOAD, mark], |r| {
        let payload = match r.get_ref(1)? {
            rusqlite::types::ValueRef::Blob(b) => b.to_vec(),
            rusqlite::types::ValueRef::Text(t) => t.to_vec(),
            _ => vec![],
        };
        Ok((r.get::<_, i64>(0)?, payload))
    })
    .ok()
}

/// A Go duration (`1h2m3.5s`) in milliseconds.
fn go_duration_ms(text: &str) -> Option<f64> {
    let re = regex::Regex::new(r"(\d+(?:\.\d+)?)(ns|us|µs|μs|ms|h|m|s)").ok()?;
    let mut total = 0.0;
    for c in re.captures_iter(text) {
        let amount: f64 = c[1].parse().ok()?;
        total += amount
            * match &c[2] {
                "ns" => 1e-6,
                "us" | "µs" | "μs" => 1e-3,
                "ms" => 1.0,
                "s" => 1_000.0,
                "m" => 60_000.0,
                _ => 3_600_000.0,
            };
    }
    (total > 0.0).then_some(total)
}

/// When a quota resets: its timestamp, else now plus its delay.
fn reset_at(text: &str, now_ms: i64) -> Option<String> {
    if let Some(ts) = regex::Regex::new(r"\bquotaResetTimeStamp:([^\s\]]+)").ok()?.captures(text).map(|c| c[1].to_owned())
        && ts.parse::<jiff::Timestamp>().is_ok()
    {
        return Some(ts);
    }
    let delay = regex::Regex::new(r"\b(?:quotaResetDelay|retryDelay):((?:\d+(?:\.\d+)?(?:ns|us|µs|μs|ms|h|m|s))+)(?:[\s\]]|$)").ok()?.captures(text).map(|c| c[1].to_owned())?;
    let ms = go_duration_ms(&delay)?;
    Some(crate::time_fmt::iso_from_millis(now_ms + ms as i64))
}

/// The failure a turn's last step (and the answer's stop reason) shows
/// (t3code `turnFailure`).
pub fn failure(stop_reason: Option<&str>, step: Option<(i64, &[u8])>, now_ms: i64) -> Option<Failure> {
    let error = step.filter(|(t, _)| *t == ERROR_STEP).map(|(_, p)| String::from_utf8_lossy(p).into_owned());
    let refusal = stop_reason == Some("refusal");
    if error.is_none() && !refusal {
        return None;
    }
    let text = error.unwrap_or_default();
    let api = regex::Regex::new(r"(?s)Error (\d{3}), Message: (.+?), Status: ([A-Z_]+)").ok()?.captures(&text).map(|c| (c[1].to_owned(), c[2].trim().to_owned(), c[3].to_owned()));
    if refusal || text.contains("reason:QUOTA_EXHAUSTED") {
        return Some(Failure { message: "You have reached your Antigravity quota for this period.".into(), usage_limit: true, reset_at: reset_at(&text, now_ms) });
    }
    let Some((http, message, status)) = api else {
        return Some(Failure { message: "Antigravity stopped this turn because of an error.".into(), usage_limit: false, reset_at: None });
    };
    let limited = http == "429" || status == "RESOURCE_EXHAUSTED";
    Some(Failure { message, usage_limit: limited, reset_at: if limited { reset_at(&text, now_ms) } else { None } })
}

/// The real outcome of a turn: what its last step after `mark` shows.
pub fn outcome(profile_home: &Path, session_id: &str, mark: Option<i64>, stop_reason: Option<&str>) -> Option<Failure> {
    let step = mark.and_then(|m| last_step_after(profile_home, session_id, m));
    failure(stop_reason, step.as_ref().map(|(t, p)| (*t, p.as_slice())), crate::time_fmt::now_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_tool_output_keeps_its_tail() {
        let long = "x".repeat(9_000) + "END";
        let bounded = bound_text(&long, TEXT_LIMIT);
        assert!(bounded.starts_with(TRUNCATED) && bounded.ends_with("END"));
        assert_eq!(bounded.chars().count(), TEXT_LIMIT + TRUNCATED.chars().count());
        let mut u = json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c", "rawOutput": { "combinedOutput": long, "formattedOutput": long, "x": "data:image/png;base64,AAAA" },
            "content": [{ "type": "image", "data": "AAAA", "mimeType": "image/png" }, { "type": "content", "content": { "type": "text", "text": "ok" } }] });
        normalize_update(&mut u);
        assert!(u["rawOutput"]["combinedOutput"].as_str().unwrap().starts_with(TRUNCATED));
        assert!(u["rawOutput"].get("formattedOutput").is_none(), "the copy of the output goes");
        assert!(u["rawOutput"].get("x").is_none(), "image data goes");
        assert!(u["content"][0].get("data").is_none());
    }

    #[test]
    fn native_command_fields_are_read() {
        let call = json!({ "toolCallId": "c", "title": "Running run_command", "status": "completed",
            "rawInput": { "CommandLine": " cargo test ", "Cwd": "/w" }, "rawOutput": { "combined_output": "ok\n", "exit_code": 3 } });
        let Work::Command { input, output, exit_code } = tool_work(&call, None) else { panic!("a command") };
        assert_eq!((input.as_str(), output.as_deref(), exit_code), ("cargo test", Some("ok\n"), Some(3)));
        let viewed = json!({ "toolCallId": "v", "title": "View", "kind": "other", "rawOutput": { "imagePath": "/w/shot.png" } });
        assert!(matches!(tool_work(&viewed, None), Work::Tool { viewed_image_path: Some(p), .. } if p == "/w/shot.png"));
        let remote = json!({ "toolCallId": "v", "kind": "other", "rawOutput": { "imagePath": "https://x/y.png" } });
        assert!(matches!(tool_work(&remote, None), Work::Tool { viewed_image_path: None, .. }));
    }

    #[test]
    fn edits_are_placed_at_their_only_match() {
        let file = "one\ntwo\nthree\nfour\n";
        let input = json!({ "TargetContent": "three\n", "ReplacementContent": "THREE\n" });
        let Some(EditPatch::Placed(p)) = edit_patch("/w/a.txt", &input, &Value::Null, Some(file)) else { panic!("placed") };
        // The hunk is the snippet's, numbered where it is in the file.
        assert_eq!(p, "--- /w/a.txt\n+++ /w/a.txt\n@@ -3,1 +3,1 @@\n-three\n+THREE\n");
        // Two matches: no place; with no file at all, a snippet.
        let twice = "a\nx\nb\nx\n";
        let x = json!({ "target_content": "x\n", "replacement_content": "y\n" });
        let Some(EditPatch::Snippet(s)) = edit_patch("/w/b.txt", &x, &Value::Null, Some(twice)) else { panic!("snippet") };
        assert!(s.contains("@@ -1,1 +1,1 @@ snippet, line unknown"), "{s}");
        // StartLine narrows the search to the second.
        let ranged = json!({ "TargetContent": "x\n", "ReplacementContent": "y\n", "StartLine": 3 });
        assert!(matches!(edit_patch("/w/b.txt", &ranged, &Value::Null, Some(twice)), Some(EditPatch::Placed(p)) if p.contains("@@ -4,1 +4,1 @@")));
        // A changed line break is uncertain.
        let joined = json!({ "TargetContent": "two\n", "ReplacementContent": "two" });
        assert_eq!(edit_patch("/w/a.txt", &joined, &Value::Null, Some(file)), Some(EditPatch::Omitted));
        // Several chunks, shifted by the lines earlier ones add.
        let multi = json!({ "ReplacementChunks": [
            { "TargetContent": "one\n", "ReplacementContent": "one\nhalf\n" },
            { "TargetContent": "four\n", "ReplacementContent": "FOUR\n" }] });
        let long = (1..=20).map(|i| if i == 1 { "one".into() } else if i == 20 { "four".into() } else { format!("l{i}") }).collect::<Vec<_>>().join("\n") + "\n";
        let Some(EditPatch::Placed(p)) = edit_patch("/w/c.txt", &multi, &Value::Null, Some(&long)) else { panic!("placed") };
        assert!(p.contains("@@ -1,1 +1,2 @@") && p.contains("@@ -20,1 +21,1 @@"), "{p}");
        assert_eq!(edit_patch("/w/a.txt", &json!({ "Other": 1 }), &Value::Null, Some(file)), None);
    }

    #[test]
    fn the_native_clis_edits_diff_the_whole_file() {
        // agy 1.5.14's `edit_file`, as recorded live: the whole file before and after.
        let call = json!({ "toolCallId": "e", "kind": "edit", "status": "completed", "title": "File notes.txt",
            "rawInput": { "Path": "/w/notes.txt", "Target": "charlie", "Replacement": "CHARLIE" },
            "content": [{ "type": "diff", "path": "/w/notes.txt", "oldText": "alpha\nbravo\ncharlie\ndelta\n", "newText": "alpha\nbravo\nCHARLIE\ndelta\n" }] });
        let Some(EditPatch::Placed(p)) = patch_for(&call) else { panic!("placed") };
        assert_eq!(p, "--- /w/notes.txt\n+++ /w/notes.txt\n@@ -1,4 +1,4 @@\n alpha\n bravo\n-charlie\n+CHARLIE\n delta\n");
        let new_file = json!({ "kind": "edit", "rawInput": { "Path": "/w/n.txt" }, "content": [{ "type": "diff", "path": "/w/n.txt", "oldText": null, "newText": "x\n" }] });
        assert!(matches!(patch_for(&new_file), Some(EditPatch::Placed(p)) if p.contains("@@ -0,0 +1,1 @@\n+x")));
        assert!(matches!(tool_work(&call, patch_for(&call).as_ref()), Work::FileChange { patch: Some(_), texts: None, .. }));
    }

    #[test]
    fn text_with_a_wide_character_near_the_start_is_kept() {
        // agy's whole-file diff of a file starting `//! The façade`: byte 11 is
        // inside `ç`, which a byte slice of the text panicked on, killing the turn.
        let text = "//! The façade shells use: vault registry\nfn main() {}\n";
        let mut u = json!({ "sessionUpdate": "tool_call_update", "toolCallId": "e", "kind": "edit", "status": "completed",
            "rawInput": { "Path": "/w/runtime.rs", "Target": "fn main() {}", "Replacement": "fn main() { run() }" },
            "content": [{ "type": "diff", "path": "/w/runtime.rs", "oldText": text, "newText": text.replace("{}", "{ run() }") }] });
        normalize_update(&mut u);
        assert_eq!(u["content"][0]["oldText"], text);
        assert!(matches!(patch_for(&u), Some(EditPatch::Placed(p)) if p.contains("+fn main() { run() }")));
        assert_eq!(sanitize_payload(&json!(["façade1234é", "DATA:IMAGE/png;base64,AA"])), json!(["façade1234é"]));
    }

    #[test]
    fn a_read_the_client_served_is_done() {
        let mut repair = ReadRepair::default();
        let mut announced = json!({ "sessionUpdate": "tool_call", "toolCallId": "r1", "title": "Running client_view_file", "status": "pending", "locations": [{ "path": "/w/a.txt" }] });
        repair.repair(&mut announced);
        assert_eq!(announced["status"], "pending");
        assert_eq!(repair.attempted("/w/a.txt", true).as_deref(), Some("r1"));
        let mut again = json!({ "sessionUpdate": "tool_call_update", "toolCallId": "r1", "status": "in_progress" });
        repair.repair(&mut again);
        assert_eq!(again["status"], "completed");
        let mut failed = json!({ "sessionUpdate": "tool_call_update", "toolCallId": "r1", "status": "failed", "rawOutput": "unrelated step error" });
        repair.repair(&mut failed);
        assert_eq!(failed["status"], "completed");
        assert!(failed.get("rawOutput").is_none());
        // Served before it was announced.
        assert_eq!(repair.attempted("/w/b.txt", true), None);
        let mut late = json!({ "sessionUpdate": "tool_call", "toolCallId": "r2", "title": "Run client_view_file?", "status": "pending", "rawInput": { "absolute_path": "/w/b.txt" } });
        repair.repair(&mut late);
        assert_eq!(late["status"], "completed");
    }

    #[test]
    fn attachments_go_as_the_agent_takes_them() {
        let dir = std::env::temp_dir().join(format!("domovoi-ag-att-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, bytes) in [("a.png", &b"png"[..]), ("a.gif", b"gif"), ("s.mp3", b"mp3"), ("d.pdf", b"%PDF"), ("n.zip", b"zip")] {
            std::fs::write(dir.join(name), bytes).unwrap();
        }
        let images = vec![("image/png".to_owned(), dir.join("a.png")), ("image/gif".to_owned(), dir.join("a.gif"))];
        let files = vec![("s.mp3".to_owned(), "audio/mpeg".to_owned(), dir.join("s.mp3")), ("d.pdf".to_owned(), "application/pdf".to_owned(), dir.join("d.pdf")), ("n.zip".to_owned(), "application/zip".to_owned(), dir.join("n.zip"))];
        let caps = json!({ "image": true, "audio": true, "embeddedContext": true });
        let blocks = attachment_blocks(&images, &files, &caps);
        let types: Vec<&str> = blocks.iter().filter_map(|b| b["type"].as_str()).collect();
        assert_eq!(types, ["image", "audio", "resource_link"], "a GIF and a zip stay named in the text only");
        assert!(blocks[2]["uri"].as_str().unwrap().starts_with("file:///"));
        assert_eq!(attachment_blocks(&images, &files, &json!({ "image": true })).len(), 1, "what the agent can't take isn't sent");
        // A text file too large to inline goes as a resource; a small one is already in the text.
        std::fs::write(dir.join("big.rs"), "fn main() {}\n".repeat(4_000)).unwrap();
        std::fs::write(dir.join("small.rs"), "fn main() {}\n").unwrap();
        let texts = vec![("big.rs".to_owned(), "application/octet-stream".to_owned(), dir.join("big.rs")), ("small.rs".to_owned(), String::new(), dir.join("small.rs"))];
        let blocks = attachment_blocks(&[], &texts, &caps);
        assert_eq!(blocks.len(), 1, "{blocks:?}");
        assert_eq!((blocks[0]["type"].as_str(), blocks[0]["resource"]["mimeType"].as_str()), (Some("resource"), Some("text/plain")));
        assert!(goal_control("/goal", false).is_some() && goal_control("/goal PAUSE", false).is_some());
        assert_eq!(goal_control("/goal ship the release", false), None);
        assert_eq!(goal_control("/goals", false), None);
        assert_eq!(goal_control("/goal clear", true), None, "with attachments it is no command");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upstream_failures_are_read_from_the_session_log() {
        let now = 1_000_000_000_000;
        assert_eq!(failure(Some("end_turn"), None, now), None);
        assert_eq!(failure(Some("end_turn"), Some((5, b"fine")), now), None);
        let quota = b"Error 429, Message: Quota exceeded, Status: RESOURCE_EXHAUSTED [reason:QUOTA_EXHAUSTED quotaResetDelay:1h30m0s]";
        let f = failure(Some("end_turn"), Some((17, quota)), now).unwrap();
        assert!(f.usage_limit && f.message.contains("quota"));
        assert_eq!(f.reset_at.as_deref(), Some(crate::time_fmt::iso_from_millis(now + 90 * 60_000).as_str()));
        let rate = b"Error 429, Message: Too many requests, Status: RESOURCE_EXHAUSTED [retryDelay:30s]";
        assert!(failure(Some("end_turn"), Some((17, rate)), now).unwrap().usage_limit);
        let server = b"Error 503, Message: The model is overloaded, Status: UNAVAILABLE";
        let f = failure(Some("end_turn"), Some((17, server)), now).unwrap();
        assert_eq!((f.message.as_str(), f.usage_limit), ("The model is overloaded", false));
        assert!(failure(Some("refusal"), None, now).unwrap().usage_limit);
        assert_eq!(failure(Some("end_turn"), Some((17, b"panic: x")), now).unwrap().message, "Antigravity stopped this turn because of an error.");
    }

    #[test]
    fn the_log_is_read_only_when_it_is_the_sessions_own() {
        let home = std::env::temp_dir().join(format!("domovoi-ag-log-{}", crate::util::new_uuid()));
        let conversations = home.join("antigravity-acp/conversations");
        std::fs::create_dir_all(&conversations).unwrap();
        let id = "829f3c80-9989-4315-8eed-b609214afb48";
        assert_eq!(log_mark(&home, id), Some(-1), "no log yet");
        let db = rusqlite::Connection::open(conversations.join(format!("{id}.db"))).unwrap();
        db.execute_batch("CREATE TABLE steps (idx INTEGER, step_type INTEGER, step_payload BLOB); INSERT INTO steps VALUES (0, 5, x'00');").unwrap();
        let mark = log_mark(&home, id);
        assert_eq!(mark, Some(0));
        db.execute("INSERT INTO steps VALUES (1, 17, ?1)", [&b"Error 503, Message: Down, Status: UNAVAILABLE"[..]]).unwrap();
        assert_eq!(outcome(&home, id, mark, Some("end_turn")).unwrap().message, "Down");
        assert_eq!(outcome(&home, "../x", mark, Some("end_turn")), None, "an id that is no file name");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(conversations.join(format!("{id}.db")), conversations.join("other.db")).unwrap();
            assert_eq!(log_mark(&home, "other"), None, "a link is not the session's own log");
        }
        let _ = std::fs::remove_dir_all(&home);
    }
}
