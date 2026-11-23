//! Antigravity's subagent batches and async background subagents (t3code
//! `acp/AntigravityChildWork.ts`, `NativeChildWorkQuery.ts`).
//!
//! Older `agy` versions ran synchronous `invoke_subagent` batches (`Subagents: [...]`,
//! recorded in `step_type = 127`). `agy` 1.5.x launches individual background
//! subagents (`invoke_subagent` with `Role`, `Prompt`, `TypeName`, `Model`,
//! recorded in `step_type = 132`) whose transcripts live under
//! `<profile>/antigravity-acp/brain/<child_id>/.system_generated/logs/transcript.jsonl`
//! and whose completion reports arrive as `step_type = 101` (`agent_message`)
//! in the parent conversation database.

#![expect(clippy::disallowed_types, reason = "the agent's subagent launch arguments and its children's transcript lines, read as it writes them")]

use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

// Field numbers of the runtime's trajectory protos (exa.cortex_pb).
const STEP_METADATA: u64 = 5;
const METADATA_TOOL_CALL: u64 = 4;
const STEP_PLANNER_RESPONSE: u64 = 20;
const PLANNER_TOOL_CALLS: u64 = 7;
const STEP_AGENT_MESSAGE: u64 = 114;
const STEP_GENERIC_TOOL: u64 = 140;
const STEP_INVOKE_SUBAGENT: u64 = 143;
const INVOKE_SPECS: u64 = 9;
const INVOKE_RESULTS: u64 = 10;

const PLANNER_RESPONSE_STEP_TYPE: i64 = 15;
const PLANNER_DONE_STATUS: i64 = 3;
const AGENT_MESSAGE_STEP_TYPE: i64 = 101;
const INVOKE_SUBAGENT_STEP_TYPE: i64 = 127;
const GENERIC_TOOL_STEP_TYPE: i64 = 132;

const MAX_STEP_BYTES: i64 = 1024 * 1024;
const MAX_INVOKE_ROWS: i64 = 200;
const MAX_SCAN_BYTES: i64 = 8 * 1024 * 1024;
const MAX_CHILDREN: usize = 16;
const TRANSCRIPT_BYTE_CAP: u64 = 256 * 1024;
const BATCH_TRANSCRIPT_BYTES: u64 = 512 * 1024;
const STEP_TEXT_CAP: usize = 4_000;
const TOOL_INPUT_CAP: usize = 1_000;

/// The title of a batch whose launch text was never seen.
pub const BATCH_TITLE: &str = "Antigravity subagent batch";

const SUBAGENT_NAMES: [&str; 2] = ["invoke_subagent", "start_subagent"];
const SUBAGENT_TITLES: [&str; 6] = [
    "Running start_subagent",
    "Run start_subagent?",
    "Run start_subagent",
    "Running invoke_subagent",
    "Run invoke_subagent?",
    "Run invoke_subagent",
];

/// Whether a tool call is a native Antigravity subagent launch (not an MCP tool of that name).
pub fn is_subagent_call(call: &Value) -> bool {
    if call["_meta"]["is_mcp_tool_call"] == true {
        return false;
    }
    let kind = call["kind"].as_str();
    if kind.is_some() && kind != Some("other") {
        return false;
    }
    let by_name = call["name"].as_str().is_some_and(|n| SUBAGENT_NAMES.contains(&n));
    let by_title = call["title"].as_str().is_some_and(|t| SUBAGENT_TITLES.contains(&t));
    by_name || by_title
}

/// The tool input object (`rawInput`, falling back to `args`).
pub fn input_of(call: &Value) -> &Value {
    if call["rawInput"].is_object() { &call["rawInput"] } else { &call["args"] }
}

/// Whether the call uses the legacy synchronous batch array (`Subagents: [...]`).
pub fn is_batch_call(call: &Value) -> bool {
    input_of(call)["Subagents"].as_array().is_some_and(|l| !l.is_empty())
}

/// The explicit model requested for a single `invoke_subagent` call (`inherit` omitted).
pub fn model_of(call: &Value) -> Option<String> {
    input_of(call)["Model"]
        .as_str()
        .map(str::trim)
        .filter(|m| !m.is_empty() && !m.eq_ignore_ascii_case("inherit"))
        .map(str::to_owned)
}

/// A launch's children as (role, prompt): supports both the legacy `Subagents`
/// batch array and `agy` 1.5.x's single-subagent `{ Role, Prompt, TypeName }`.
pub fn launches(raw_input: &Value) -> Option<Vec<(String, String)>> {
    if let Some(list) = raw_input["Subagents"].as_array().filter(|l| !l.is_empty()) {
        return list.iter().map(|s| Some((s["Role"].as_str()?.to_owned(), s["Prompt"].as_str()?.to_owned()))).collect();
    }
    let role = raw_input["Role"]
        .as_str()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .or_else(|| raw_input["TypeName"].as_str().map(str::trim).filter(|t| !t.is_empty()));
    let prompt = raw_input["Prompt"].as_str();
    if role.is_none() && prompt.is_none() {
        return None;
    }
    Some(vec![(role.unwrap_or("Subagent").to_owned(), prompt.unwrap_or_default().to_owned())])
}

/// A batch's title and prompt (the children's, one under the other).
pub fn describe(launches: &[(String, String)]) -> (String, String) {
    match launches {
        [(role, prompt)] => (role.clone(), prompt.clone()),
        _ => (launches.iter().map(|(r, _)| r.as_str()).collect::<Vec<_>>().join(", "), launches.iter().map(|(r, p)| format!("{r}\n{p}")).collect::<Vec<_>>().join("\n\n")),
    }
}

/// The title and prompt a subagent tool call shows.
pub fn batch(call: &Value) -> (String, String) {
    if let Some(l) = launches(input_of(call)) {
        return describe(&l);
    }
    let output = call["rawOutput"].as_str().map(str::trim).filter(|o| !o.is_empty()).map(|o| super::protocol::bound_text(o, 8_000));
    (BATCH_TITLE.to_owned(), output.or_else(|| call["title"].as_str().map(str::to_owned)).unwrap_or_else(|| BATCH_TITLE.to_owned()))
}

/// The output of a failed subagent call.
pub fn failure_output(call: &Value) -> Option<String> {
    call["rawOutput"]
        .as_str()
        .or_else(|| call["rawOutput"]["output"].as_str())
        .map(str::trim)
        .filter(|o| !o.is_empty())
        .map(|o| super::protocol::bound_text(o, 8_000))
}

// ---------------------------------------------------------------------------
// The session log's records
// ---------------------------------------------------------------------------

/// Length-delimited fields of a protobuf message (others skipped); `None` when malformed.
fn fields(bytes: &[u8]) -> Option<Vec<(u64, &[u8])>> {
    let mut out = vec![];
    let mut at = 0usize;
    let varint = |at: &mut usize| -> Option<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = *bytes.get(*at)?;
            *at += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte < 0x80 {
                return Some(value);
            }
        }
        None
    };
    while at < bytes.len() {
        let key = varint(&mut at)?;
        match key % 8 {
            0 => {
                varint(&mut at)?;
            }
            1 => at += 8,
            5 => at += 4,
            2 => {
                let len = usize::try_from(varint(&mut at)?).ok()?;
                let end = at.checked_add(len).filter(|e| *e <= bytes.len())?;
                out.push((key / 8, &bytes[at..end]));
                at = end;
            }
            _ => return None,
        }
    }
    (at <= bytes.len()).then_some(out)
}

fn messages(bytes: &[u8], field: u64) -> Option<Vec<&[u8]>> {
    Some(fields(bytes)?.into_iter().filter(|(n, _)| *n == field).map(|(_, v)| v).collect())
}

fn message(bytes: Option<&[u8]>, field: u64) -> Option<&[u8]> {
    messages(bytes?, field)?.into_iter().next()
}

fn text(bytes: Option<&[u8]>, field: u64) -> Option<String> {
    String::from_utf8(message(bytes, field)?.to_vec()).ok()
}

struct InvokeRecord {
    tool_call_id: Option<String>,
    launches: Vec<(String, String)>,
    children: Vec<(String, String)>,
}

fn native_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn decode_invoke(payload: &[u8]) -> Option<InvokeRecord> {
    let invoke = message(Some(payload), STEP_INVOKE_SUBAGENT)?;
    let children: Vec<(String, String)> = messages(invoke, INVOKE_RESULTS)?.into_iter().map(|r| (text(Some(r), 1).unwrap_or_default(), text(Some(r), 2).unwrap_or_default())).collect();
    if !children.iter().all(|(id, _)| native_id(id)) {
        return None;
    }
    Some(InvokeRecord {
        tool_call_id: text(message(message(Some(payload), STEP_METADATA), METADATA_TOOL_CALL), 1),
        launches: messages(invoke, INVOKE_SPECS)?.into_iter().map(|s| (text(Some(s), 2).unwrap_or_default(), text(Some(s), 3).unwrap_or_default())).collect(),
        children,
    })
}

/// Extracts `("conversationId", "logAbsoluteUri")` from `agy` 1.5.x's
/// `invoke_subagent` tool output (`Created the following subagent:\n{ ... }`).
fn parse_created_subagent_json(output: &str) -> Option<(String, String)> {
    let start = output.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut end = None;
    for (offset, ch) in output[start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    end = Some(start + offset + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let slice = &output[start..end?];
    let parsed: Value = serde_json::from_str(slice).ok()?;
    let conv_id = parsed["conversationId"].as_str()?.trim().to_owned();
    if !native_id(&conv_id) {
        return None;
    }
    let log_uri = parsed["logAbsoluteUri"].as_str().unwrap_or_default().trim().to_owned();
    Some((conv_id, log_uri))
}

/// Decodes an `agy` 1.5.x `step_type = 132` (`invoke_subagent`) step payload.
fn decode_generic_invoke(payload: &[u8]) -> Option<InvokeRecord> {
    let tool_call = message(message(Some(payload), STEP_METADATA), METADATA_TOOL_CALL)?;
    let tool_name = text(Some(tool_call), 2)?;
    if !SUBAGENT_NAMES.contains(&tool_name.as_str()) {
        return None;
    }
    let tool_call_id = text(Some(tool_call), 1);
    let generic = message(Some(payload), STEP_GENERIC_TOOL)?;
    let out_str = text(message(Some(generic), 2), 1)?;
    let (conv_id, log_uri) = parse_created_subagent_json(&out_str)?;

    let mut role = None;
    let mut type_name = None;
    let mut prompt = None;
    for kv in messages(generic, 1).unwrap_or_default() {
        let Some(key) = text(Some(kv), 1) else { continue };
        let Some(val) = text(Some(kv), 2) else { continue };
        match key.as_str() {
            "Role" if !val.trim().is_empty() => role = Some(val),
            "TypeName" if !val.trim().is_empty() => type_name = Some(val),
            "Prompt" => prompt = Some(val),
            _ => {}
        }
    }
    if (role.is_none() || prompt.is_none())
        && let Some(raw_args) = text(Some(tool_call), 3)
        && let Ok(parsed) = serde_json::from_str::<Value>(&raw_args)
    {
        if role.is_none() {
            role = parsed["Role"].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
        }
        if type_name.is_none() {
            type_name = parsed["TypeName"].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
        }
        if prompt.is_none() {
            prompt = parsed["Prompt"].as_str().map(str::to_owned);
        }
    }
    let role = role.or(type_name).unwrap_or_else(|| "Subagent".to_owned());
    let prompt = prompt.unwrap_or_default();
    Some(InvokeRecord { tool_call_id, launches: vec![(role, prompt)], children: vec![(conv_id, log_uri)] })
}

fn planner_invoke_ids(payload: &[u8]) -> Option<Vec<String>> {
    let Some(planner) = message(Some(payload), STEP_PLANNER_RESPONSE) else { return Some(vec![]) };
    Some(messages(planner, PLANNER_TOOL_CALLS)?.into_iter().filter(|c| text(Some(c), 2).as_deref() == Some("invoke_subagent")).filter_map(|c| text(Some(c), 1)).collect())
}

fn complete(record: InvokeRecord) -> Option<InvokeRecord> {
    (record.launches.len() == record.children.len() && record.children.len() <= MAX_CHILDREN).then_some(record)
}

/// A batch as Domovoi recorded it.
pub struct Batch {
    pub native_task_id: String,
    pub title: Option<String>,
    pub prompt: String,
}

fn planner_ids_at(db: &rusqlite::Connection, idx: i64) -> Option<Vec<String>> {
    let payload: Vec<u8> = db.query_row("SELECT step_payload FROM steps WHERE idx = ?1 AND step_format = 0 AND length(step_payload) <= ?2", rusqlite::params![idx, MAX_STEP_BYTES], |r| r.get(0)).ok()?;
    planner_invoke_ids(&payload)
}

/// Finds a `step_type = 132` (`agy` 1.5.x) `invoke_subagent` record matching `tool_call_id`.
fn find_generic_record_by_call_id(db: &rusqlite::Connection, tool_call_id: &str) -> Option<InvokeRecord> {
    if tool_call_id.is_empty() {
        return None;
    }
    let mut stmt = db
        .prepare("SELECT length(step_payload), CASE WHEN length(step_payload) <= ?1 THEN step_payload END FROM steps WHERE step_type = ?2 AND step_format = 0 ORDER BY idx DESC")
        .ok()?;
    let rows = stmt.query_map(rusqlite::params![MAX_STEP_BYTES, GENERIC_TOOL_STEP_TYPE], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<Vec<u8>>>(1)?))).ok()?;
    let (mut count, mut bytes) = (0i64, 0i64);
    for row in rows {
        let (size, payload) = row.ok()?;
        count += 1;
        bytes += size;
        if count > MAX_INVOKE_ROWS * 4 || bytes > MAX_SCAN_BYTES * 2 {
            break;
        }
        if let Some(record) = payload.as_deref().and_then(decode_generic_invoke)
            && record.tool_call_id.as_deref() == Some(tool_call_id)
        {
            return complete(record);
        }
    }
    None
}

/// The `invoke_subagent` record of a batch: checks `agy` 1.5.x (`step_type = 132`)
/// by exact `tool_call_id` first, then legacy `step_type = 127` by planner step
/// or unique launch text.
fn find_record(db: &rusqlite::Connection, root_session: &str, batch: &Batch, siblings: &[Batch]) -> Option<InvokeRecord> {
    if let Some(record) = find_generic_record_by_call_id(db, &batch.native_task_id) {
        return Some(record);
    }
    let planner_idx = |id: &str| -> Option<i64> {
        let (session, step) = id.rsplit_once(':')?;
        let step: i64 = step.parse().ok()?;
        Some(if session == root_session { step } else { -1 })
    };
    if let Some(idx) = planner_idx(&batch.native_task_id) {
        if idx < 0 {
            return None;
        }
        let ids = planner_ids_at(db, idx)?;
        let [call] = ids.as_slice() else { return None };
        let mut stmt = db
            .prepare("SELECT length(step_payload), CASE WHEN length(step_payload) <= ?1 THEN step_payload END FROM steps WHERE step_type = ?2 AND step_format = 0 AND idx > ?3 ORDER BY idx")
            .ok()?;
        let rows = stmt.query_map(rusqlite::params![MAX_STEP_BYTES, INVOKE_SUBAGENT_STEP_TYPE, idx], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<Vec<u8>>>(1)?))).ok()?;
        let (mut count, mut bytes) = (0, 0);
        for row in rows {
            let (size, payload) = row.ok()?;
            count += 1;
            bytes += size;
            if count > MAX_INVOKE_ROWS || size > MAX_STEP_BYTES || bytes > MAX_SCAN_BYTES {
                return None;
            }
            if let Some(record) = payload.as_deref().and_then(decode_invoke)
                && record.tool_call_id.as_deref() == Some(call.as_str())
            {
                return complete(record);
            }
        }
        return None;
    }
    let same = |b: &Batch| b.title == batch.title && b.prompt == batch.prompt;
    let (rows, bytes, largest, other): (i64, i64, i64, i64) = db
        .query_row(
            "SELECT count(*), coalesce(sum(length(step_payload)), 0), coalesce(max(length(step_payload)), 0), coalesce(sum(step_format != 0), 0) FROM steps WHERE step_type = ?1",
            [INVOKE_SUBAGENT_STEP_TYPE],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .ok()?;
    if siblings.iter().any(same) || rows > MAX_INVOKE_ROWS || bytes > MAX_SCAN_BYTES || largest > MAX_STEP_BYTES || other > 0 {
        return None;
    }
    let cut = |t: &str| t.contains(super::protocol::TRUNCATED);
    let mut claimed = std::collections::HashSet::new();
    for s in siblings {
        match planner_idx(&s.native_task_id) {
            None => {
                // A sibling whose launch text is missing or cut can't be ruled out.
                let title_cut = s.title.as_deref().is_some_and(|t| t == BATCH_TITLE || cut(t));
                if title_cut || (cut(&s.prompt) && (s.title.is_none() || s.title == batch.title)) {
                    return None;
                }
            }
            Some(idx) if idx < 0 => {}
            Some(idx) => {
                let ids = planner_ids_at(db, idx).filter(|ids| !ids.is_empty())?;
                claimed.extend(ids);
            }
        }
    }
    let mut stmt = db.prepare("SELECT step_payload FROM steps WHERE step_type = ?1 ORDER BY idx").ok()?;
    let rows = stmt.query_map([INVOKE_SUBAGENT_STEP_TYPE], |r| r.get::<_, Vec<u8>>(0)).ok()?;
    let mut matches = vec![];
    for row in rows {
        // An unreadable record could be an identical launch: uniqueness is unproven.
        let record = decode_invoke(&row.ok()?)?;
        if record.tool_call_id.as_ref().is_some_and(|id| claimed.contains(id)) {
            continue;
        }
        // Domovoi kept the launch text bounded, so the record is bounded the same way.
        let bounded: Vec<(String, String)> = record.launches.iter().map(|(r, p)| (super::protocol::bound_text(r, 8_000), super::protocol::bound_text(p, 8_000))).collect();
        let (title, prompt) = describe(&bounded);
        if Some(title) == batch.title && prompt == batch.prompt {
            matches.push(record);
        }
    }
    if matches.len() != 1 {
        return None;
    }
    matches.pop().and_then(complete)
}

/// Finds the latest `agent_message` delivered by `sender_id` in the parent session DB (`step_type = 101`).
fn agent_message_from(db: &rusqlite::Connection, sender_id: &str) -> Option<String> {
    let mut stmt = db
        .prepare("SELECT length(step_payload), CASE WHEN length(step_payload) <= ?1 THEN step_payload END FROM steps WHERE step_type = ?2 AND step_format = 0 ORDER BY idx DESC")
        .ok()?;
    let rows = stmt.query_map(rusqlite::params![MAX_STEP_BYTES, AGENT_MESSAGE_STEP_TYPE], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<Vec<u8>>>(1)?))).ok()?;
    let mut bytes = 0i64;
    for row in rows {
        let (size, payload) = row.ok()?;
        bytes += size;
        if bytes > MAX_SCAN_BYTES {
            break;
        }
        let Some(payload) = payload else { continue };
        let Some(msg) = message(Some(&payload), STEP_AGENT_MESSAGE) else { continue };
        if text(Some(msg), 3).as_deref() != Some("agent_message") {
            continue;
        }
        if let Some(envelope) = message(Some(msg), 4)
            && text(Some(envelope), 3).as_deref() == Some(sender_id)
            && let Some(content) = text(Some(envelope), 4)
            && !content.trim().is_empty()
        {
            return Some(super::protocol::bound_text(content.trim(), 8_000));
        }
        if let Some(raw) = text(Some(msg), 1)
            && raw.contains(&format!("sender={sender_id}"))
            && let Some((_, rest)) = raw.split_once("content=")
            && !rest.trim().is_empty()
        {
            return Some(super::protocol::bound_text(rest.trim(), 8_000));
        }
    }
    None
}

/// One step of a child, as clients show it (`OrchestrationV2NativeChildStep`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub index: u64,
    #[serde(rename = "type")]
    pub kind: String,
    pub status: Option<String>,
    pub text: Option<String>,
    pub tool_calls: Vec<ToolCall>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolCall {
    pub name: String,
    pub input: String,
}

/// One child of a batch (`OrchestrationV2NativeChildWork`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Child {
    pub conversation_id: String,
    pub role: String,
    pub prompt: String,
    pub steps: Option<Vec<Step>>,
    pub truncated: bool,
}

/// A spawned child subagent discovered in the parent session DB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveChild {
    pub conversation_id: String,
    pub log_uri: String,
    pub role: String,
    pub prompt: String,
}

/// A parsed tool call inside a child's `transcript.jsonl` step.
#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptToolCall {
    pub name: String,
    pub input: Value,
}

/// One step from a child's `transcript.jsonl`, with structured tool inputs.
#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptStep {
    pub index: u64,
    pub kind: String,
    pub status: Option<String>,
    pub text: Option<String>,
    pub tool_calls: Vec<TranscriptToolCall>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildState {
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl ChildState {
    pub fn as_str(self) -> &'static str {
        match self {
            ChildState::Running => "running",
            ChildState::Completed => "completed",
            ChildState::Failed => "failed",
            ChildState::Cancelled => "cancelled",
        }
    }

    pub fn is_terminal(self) -> bool {
        !matches!(self, ChildState::Running)
    }
}

/// Current state of a running or finished child subagent, read from disk.
#[derive(Debug, Clone, PartialEq)]
pub struct ChildSnapshot {
    pub steps: Vec<TranscriptStep>,
    pub status: ChildState,
    pub result: Option<String>,
}

fn cap(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit { text.to_owned() } else { format!("{}\u{2026}", text.chars().take(limit).collect::<String>()) }
}

fn decode_tool_args(args: &Value) -> Value {
    match args {
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().and_then(|s| serde_json::from_str(s).ok()).unwrap_or_else(|| v.clone())))
                .collect(),
        ),
        Value::Null => Value::Object(serde_json::Map::new()),
        other => other.clone(),
    }
}

/// A transcript's steps (with structured tool args), without the runtime's opening envelope around the launch prompt.
fn parse_transcript_steps(contents: &str, prompt: &str) -> Vec<TranscriptStep> {
    let launch = prompt.trim();
    contents
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|step| {
            let index = step["step_index"].as_u64()?;
            let kind = step["type"].as_str()?.to_owned();
            if index == 0 && kind == "SYSTEM_MESSAGE" && (launch.is_empty() || step["content"].as_str().is_some_and(|c| c.contains(launch))) {
                return None;
            }
            let text = step["content"].as_str().or(step["error"].as_str()).map(|t| cap(t, STEP_TEXT_CAP));
            let tool_calls = step["tool_calls"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| {
                    let name = c["name"].as_str()?.to_owned();
                    Some(TranscriptToolCall { name, input: decode_tool_args(&c["args"]) })
                })
                .collect();
            Some(TranscriptStep { index, kind, status: step["status"].as_str().map(str::to_owned), text, tool_calls })
        })
        .collect()
}

fn to_api_steps(steps: Vec<TranscriptStep>) -> Vec<Step> {
    steps
        .into_iter()
        .map(|s| Step {
            index: s.index,
            kind: s.kind,
            status: s.status,
            text: s.text,
            tool_calls: s.tool_calls.into_iter().map(|tc| ToolCall { name: tc.name, input: cap(&tc.input.to_string(), TOOL_INPUT_CAP) }).collect(),
        })
        .collect()
}

/// Reads the tail of a child's transcript, verified inside the instance profile.
fn read_transcript_raw(root: &Path, conversation: &str, log_uri: &str, byte_cap: u64) -> Option<(String, bool)> {
    use std::io::{Read, Seek};
    if !native_id(conversation) {
        return None;
    }
    let expected = managed(root, &["antigravity-acp", "brain", conversation, ".system_generated", "logs", "transcript.jsonl"])?;
    if !log_uri.is_empty() {
        let named = url::Url::parse(log_uri).ok()?.to_file_path().ok()?;
        if std::fs::canonicalize(named).ok()? != expected {
            return None;
        }
    }
    let mut file = std::fs::File::open(&expected).ok()?;
    let size = file.metadata().ok()?.len();
    let length = size.min(byte_cap);
    file.seek(std::io::SeekFrom::Start(size - length)).ok()?;
    let mut bytes = vec![];
    file.take(length).read_to_end(&mut bytes).ok()?;
    let truncated = size > length;
    let mut contents = String::from_utf8_lossy(&bytes).into_owned();
    // A tail starts mid-line.
    if truncated {
        contents = contents.split_once('\n').map(|(_, rest)| rest.to_owned()).unwrap_or_default();
    }
    Some((contents, truncated))
}

fn read_transcript(root: &Path, conversation: &str, log_uri: &str, prompt: &str, byte_cap: u64) -> Option<(Vec<Step>, bool)> {
    let (contents, truncated) = read_transcript_raw(root, conversation, log_uri, byte_cap)?;
    Some((to_api_steps(parse_transcript_steps(&contents, prompt)), truncated))
}

/// `parts` below `root`, each a real entry (no link), the last a file.
fn managed(root: &Path, parts: &[&str]) -> Option<PathBuf> {
    let mut current = root.to_path_buf();
    for (i, part) in parts.iter().enumerate() {
        current.push(part);
        let meta = std::fs::symlink_metadata(&current).ok()?;
        let file = i == parts.len() - 1;
        if meta.file_type().is_symlink() || (if file { !meta.is_file() } else { !meta.is_dir() }) {
            return None;
        }
    }
    (std::fs::canonicalize(&current).ok()? == current).then_some(current)
}

fn open_session_db(profile_home: &Path, root_session: &str) -> Option<(PathBuf, rusqlite::Connection)> {
    if !native_id(root_session) {
        return None;
    }
    let root = std::fs::canonicalize(profile_home).ok()?;
    let db_path = managed(&root, &["antigravity-acp", "conversations", &format!("{root_session}.db")])?;
    let db = rusqlite::Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()?;
    let _ = db.busy_timeout(std::time::Duration::from_millis(100));
    Some((root, db))
}

/// Whether the Antigravity session database exists on disk for `root_session`.
pub fn session_db_exists(profile_home: &Path, root_session: &str) -> bool {
    if !native_id(root_session) {
        return false;
    }
    let Some(root) = std::fs::canonicalize(profile_home).ok() else { return false };
    managed(&root, &["antigravity-acp", "conversations", &format!("{root_session}.db")]).is_some()
}

/// Looks up the spawned child conversation for an `invoke_subagent` `tool_call_id`.
pub fn lookup_spawned_child(profile_home: &Path, root_session: &str, tool_call_id: &str) -> Option<LiveChild> {
    let (_, db) = open_session_db(profile_home, root_session)?;
    let record = find_generic_record_by_call_id(&db, tool_call_id)?;
    let (conversation_id, log_uri) = record.children.into_iter().next()?;
    let (role, prompt) = record.launches.into_iter().next().unwrap_or_default();
    Some(LiveChild { conversation_id, log_uri, role, prompt })
}

/// Reads a child subagent's transcript steps, current lifecycle state, and final result.
pub fn inspect_child(profile_home: &Path, root_session: &str, conversation_id: &str, log_uri: &str, prompt: &str) -> Option<ChildSnapshot> {
    if !native_id(conversation_id) {
        return None;
    }
    let root = std::fs::canonicalize(profile_home).ok()?;
    let parent_message = open_session_db(profile_home, root_session).and_then(|(_, db)| agent_message_from(&db, conversation_id));
    let steps = read_transcript_raw(&root, conversation_id, log_uri, TRANSCRIPT_BYTE_CAP)
        .map(|(contents, _)| parse_transcript_steps(&contents, prompt))
        .unwrap_or_default();

    let last = steps.last();
    let last_status = last.and_then(|s| s.status.as_deref()).unwrap_or_default();
    let status = if matches!(last_status, "ERROR" | "FAILED") {
        ChildState::Failed
    } else if matches!(last_status, "CANCELLED" | "CANCELED") {
        ChildState::Cancelled
    } else if last.is_some_and(|s| s.kind == "PLANNER_RESPONSE" && s.status.as_deref() == Some("DONE") && s.tool_calls.is_empty()) {
        ChildState::Completed
    } else if parent_message.is_some() {
        ChildState::Completed
    } else {
        ChildState::Running
    };

    let planner_reply = steps
        .iter()
        .rev()
        .find(|s| s.kind == "PLANNER_RESPONSE" && s.text.as_deref().is_some_and(|t| !t.trim().is_empty()))
        .and_then(|s| s.text.clone());
    let result = match status {
        ChildState::Failed => last.and_then(|s| s.text.clone()).or(parent_message).or(planner_reply),
        ChildState::Completed => parent_message.or(planner_reply),
        ChildState::Cancelled => planner_reply.or(parent_message),
        ChildState::Running => None,
    };
    Some(ChildSnapshot { steps, status, result })
}

/// Highest `idx` currently recorded in the parent session's `steps` table.
pub fn latest_step_idx(profile_home: &Path, root_session: &str) -> Option<i64> {
    let (_, db) = open_session_db(profile_home, root_session)?;
    db.query_row("SELECT max(idx) FROM steps", [], |r| r.get::<_, Option<i64>>(0)).ok().flatten()
}

/// Whether the parent session's latest step after `min_step_idx` is a finished
/// `PLANNER_RESPONSE` with no pending tool calls.
pub fn parent_turn_settled(profile_home: &Path, root_session: &str, min_step_idx: Option<i64>) -> bool {
    let Some((_, db)) = open_session_db(profile_home, root_session) else { return false };
    let Ok((idx, step_type, status, payload)) = db.query_row(
        "SELECT idx, step_type, status, CASE WHEN length(step_payload) <= ?1 THEN step_payload END FROM steps WHERE step_format = 0 ORDER BY idx DESC LIMIT 1",
        [MAX_STEP_BYTES],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, Option<Vec<u8>>>(3)?)),
    ) else {
        return false;
    };
    if min_step_idx.is_some_and(|min| idx <= min) {
        return false;
    }
    if step_type != PLANNER_RESPONSE_STEP_TYPE || status != PLANNER_DONE_STATUS {
        return false;
    }
    let Some(payload) = payload else { return false };
    let Some(planner) = message(Some(&payload), STEP_PLANNER_RESPONSE) else { return false };
    messages(planner, PLANNER_TOOL_CALLS).is_some_and(|calls| calls.is_empty())
}

/// The children a batch launched, as the parent session's own record proves
/// them (none when it does not).
pub fn read(profile_home: &Path, root_session: &str, batch: &Batch, siblings: &[Batch]) -> Vec<Child> {
    let Some((root, db)) = open_session_db(profile_home, root_session) else { return vec![] };
    let Some(record) = find_record(&db, root_session, batch, siblings) else { return vec![] };
    drop(db);
    // One budget for the batch, however many children ran.
    let byte_cap = TRANSCRIPT_BYTE_CAP.min(BATCH_TRANSCRIPT_BYTES / record.children.len().max(1) as u64);
    record
        .children
        .iter()
        .enumerate()
        .map(|(i, (conversation, log_uri))| {
            let (role, prompt) = record.launches.get(i).cloned().unwrap_or_default();
            let transcript = read_transcript(&root, conversation, log_uri, &prompt, byte_cap);
            Child { conversation_id: conversation.clone(), role, prompt, truncated: transcript.as_ref().is_some_and(|t| t.1), steps: transcript.map(|t| t.0) }
        })
        .collect()
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A length-delimited field.
    fn field(number: u64, bytes: &[u8]) -> Vec<u8> {
        let mut out = vec![];
        let mut put = |mut v: u64| loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                break;
            }
            out.push(b | 0x80);
        };
        put((number << 3) | 2);
        put(bytes.len() as u64);
        out.extend_from_slice(bytes);
        out
    }

    /// An `invoke_subagent` step payload (legacy `step_type = 127`).
    pub fn invoke_step(tool_call: &str, specs: &[(&str, &str)], children: &[(&str, &str)]) -> Vec<u8> {
        let mut invoke = vec![];
        for (role, prompt) in specs {
            invoke.extend(field(INVOKE_SPECS, &[field(2, role.as_bytes()), field(3, prompt.as_bytes())].concat()));
        }
        for (id, uri) in children {
            invoke.extend(field(INVOKE_RESULTS, &[field(1, id.as_bytes()), field(2, uri.as_bytes())].concat()));
        }
        let metadata = field(STEP_METADATA, &field(METADATA_TOOL_CALL, &field(1, tool_call.as_bytes())));
        [metadata, field(STEP_INVOKE_SUBAGENT, &invoke)].concat()
    }

    /// An `agy` 1.5.x `step_type = 132` (`invoke_subagent`) payload.
    pub fn generic_invoke_step(tool_call: &str, role: &str, prompt: &str, child_id: &str, log_uri: &str) -> Vec<u8> {
        let tc = [field(1, tool_call.as_bytes()), field(2, b"invoke_subagent")].concat();
        let metadata = field(STEP_METADATA, &field(METADATA_TOOL_CALL, &tc));
        let out_text = format!(
            "Created At: 2026-11-21T18:28:49+01:00\nCompleted At: 2026-11-21T18:28:55+01:00\nCreated the following subagent:\n{{\n  \"conversationId\": \"{child_id}\",\n  \"logAbsoluteUri\": \"{log_uri}\"\n}}\nThe subagent will send you a message when it finishes."
        );
        let generic = [
            field(1, &[field(1, b"Role"), field(2, role.as_bytes())].concat()),
            field(1, &[field(1, b"Prompt"), field(2, prompt.as_bytes())].concat()),
            field(2, &field(1, out_text.as_bytes())),
        ]
        .concat();
        [metadata, field(STEP_GENERIC_TOOL, &generic)].concat()
    }

    /// A `step_type = 101` (`agent_message`) payload delivered from a child subagent.
    pub fn agent_message_step(sender_id: &str, content: &str) -> Vec<u8> {
        let envelope = [field(3, sender_id.as_bytes()), field(4, content.as_bytes())].concat();
        let msg = [field(3, b"agent_message"), field(4, &envelope)].concat();
        field(STEP_AGENT_MESSAGE, &msg)
    }

    #[test]
    fn batches_and_single_subagent_calls_are_told_from_tools_and_described() {
        let call = serde_json::json!({ "title": "Run invoke_subagent?", "kind": "other", "rawInput": { "Subagents": [{ "Role": "tester", "Prompt": "run tests" }, { "Role": "docs", "Prompt": "write docs" }] } });
        assert!(is_subagent_call(&call));
        assert!(is_batch_call(&call));
        assert!(!is_subagent_call(&serde_json::json!({ "title": "Run invoke_subagent?", "_meta": { "is_mcp_tool_call": true } })));
        assert_eq!(batch(&call), ("tester, docs".into(), "tester\nrun tests\n\ndocs\nwrite docs".into()));
        assert_eq!(batch(&serde_json::json!({ "title": "Running start_subagent" })).0, BATCH_TITLE);

        // agy 1.5.x single-subagent format ("Run invoke_subagent", no "?", direct Role/Prompt/TypeName/Model)
        let single = serde_json::json!({
            "name": "invoke_subagent",
            "title": "Run invoke_subagent",
            "kind": "other",
            "rawInput": { "Role": "App Icon Designer A", "Prompt": "Render concept A", "TypeName": "image-generator", "Model": "pro" }
        });
        assert!(is_subagent_call(&single));
        assert!(!is_batch_call(&single));
        assert_eq!(batch(&single), ("App Icon Designer A".into(), "Render concept A".into()));
        assert_eq!(model_of(&single).as_deref(), Some("pro"));

        let inherited = serde_json::json!({
            "name": "invoke_subagent",
            "title": "Run invoke_subagent",
            "args": { "TypeName": "research", "Prompt": "Audit the repo", "Model": "inherit" }
        });
        assert!(is_subagent_call(&inherited));
        assert_eq!(batch(&inherited), ("research".into(), "Audit the repo".into()));
        assert_eq!(model_of(&inherited), None);
    }

    #[test]
    fn a_batchs_children_are_read_from_the_parents_own_log() {
        let home = std::env::temp_dir().join(format!("domovoi-ag-children-{}", crate::util::new_uuid()));
        let conversations = home.join("antigravity-acp/conversations");
        std::fs::create_dir_all(&conversations).unwrap();
        let logs = home.join("antigravity-acp/brain/child-1/.system_generated/logs");
        std::fs::create_dir_all(&logs).unwrap();
        let transcript = logs.join("transcript.jsonl");
        std::fs::write(&transcript, concat!(
            "{\"step_index\":0,\"type\":\"SYSTEM_MESSAGE\",\"content\":\"You are... run tests\"}\n",
            "{\"step_index\":1,\"type\":\"PLANNER_RESPONSE\",\"status\":\"DONE\",\"content\":\"Running them\",\"tool_calls\":[{\"name\":\"run_command\",\"args\":{\"CommandLine\":\"\\\"cargo test\\\"\"}}]}\n",
        )).unwrap();
        let canonical = std::fs::canonicalize(&transcript).unwrap();
        let uri = url::Url::from_file_path(&canonical).unwrap().to_string();
        let db = rusqlite::Connection::open(conversations.join("root-1.db")).unwrap();
        db.execute_batch("CREATE TABLE steps (idx INTEGER, step_type INTEGER, step_format INTEGER, step_payload BLOB)").unwrap();
        db.execute("INSERT INTO steps VALUES (3, 127, 0, ?1)", [invoke_step("tc-9", &[("tester", "run tests")], &[("child-1", &uri)])]).unwrap();
        drop(db);
        let batch = Batch { native_task_id: "call-random".into(), title: Some("tester".into()), prompt: "run tests".into() };
        let children = read(&home, "root-1", &batch, &[]);
        assert_eq!(children.len(), 1);
        let c = &children[0];
        assert_eq!((c.conversation_id.as_str(), c.role.as_str(), c.truncated), ("child-1", "tester", false));
        let steps = c.steps.as_ref().unwrap();
        assert_eq!(steps.len(), 1, "the launch envelope is left out");
        assert_eq!(steps[0].tool_calls[0].input, "{\"CommandLine\":\"cargo test\"}");
        // A sibling with the same launch text: which one it was is unproven.
        let twin = Batch { native_task_id: "other".into(), title: Some("tester".into()), prompt: "run tests".into() };
        assert!(read(&home, "root-1", &batch, &[twin]).is_empty());
        assert!(read(&home, "../x", &batch, &[]).is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn agy_1_5_step_132_children_and_live_inspection_work() {
        let home = std::env::temp_dir().join(format!("domovoi-ag-15-children-{}", crate::util::new_uuid()));
        let conversations = home.join("antigravity-acp/conversations");
        std::fs::create_dir_all(&conversations).unwrap();
        let logs = home.join("antigravity-acp/brain/child-15/.system_generated/logs");
        std::fs::create_dir_all(&logs).unwrap();
        let transcript = logs.join("transcript.jsonl");
        std::fs::write(&transcript, concat!(
            "{\"step_index\":0,\"type\":\"SYSTEM_MESSAGE\",\"status\":\"DONE\",\"content\":\"Generate icon\"}\n",
            "{\"step_index\":1,\"type\":\"PLANNER_RESPONSE\",\"status\":\"DONE\",\"tool_calls\":[{\"name\":\"generate_image\",\"args\":{\"ImageName\":\"\\\"icon_a\\\"\"}}]}\n",
        )).unwrap();
        let canonical = std::fs::canonicalize(&transcript).unwrap();
        let uri = url::Url::from_file_path(&canonical).unwrap().to_string();
        let db = rusqlite::Connection::open(conversations.join("root-15.db")).unwrap();
        db.execute_batch("CREATE TABLE steps (idx INTEGER, step_type INTEGER, status INTEGER, step_format INTEGER, step_payload BLOB)").unwrap();
        db.execute(
            "INSERT INTO steps VALUES (66, 132, 3, 0, ?1)",
            [generic_invoke_step("call_1234", "App Icon Designer A", "Generate icon", "child-15", &uri)],
        ).unwrap();

        // Lookup by tool_call_id finds the child even while running.
        let child = lookup_spawned_child(&home, "root-15", "call_1234").expect("spawned child");
        assert_eq!((child.conversation_id.as_str(), child.role.as_str()), ("child-15", "App Icon Designer A"));
        let running = inspect_child(&home, "root-15", &child.conversation_id, &child.log_uri, &child.prompt).unwrap();
        assert_eq!(running.status, ChildState::Running);
        assert_eq!(running.steps.len(), 1);
        assert_eq!(running.steps[0].tool_calls[0].name, "generate_image");

        // Also readable via getNativeChildWork's `read`.
        let batch = Batch { native_task_id: "call_1234".into(), title: Some("App Icon Designer A".into()), prompt: "Generate icon".into() };
        let api_children = read(&home, "root-15", &batch, &[]);
        assert_eq!(api_children.len(), 1);
        assert_eq!(api_children[0].conversation_id, "child-15");

        // Once the child finishes and delivers an agent_message to parent, status becomes Completed with that result.
        std::fs::write(&transcript, concat!(
            "{\"step_index\":0,\"type\":\"SYSTEM_MESSAGE\",\"status\":\"DONE\",\"content\":\"Generate icon\"}\n",
            "{\"step_index\":1,\"type\":\"PLANNER_RESPONSE\",\"status\":\"DONE\",\"tool_calls\":[{\"name\":\"generate_image\",\"args\":{\"ImageName\":\"\\\"icon_a\\\"\"}}]}\n",
            "{\"step_index\":2,\"type\":\"GENERIC\",\"status\":\"DONE\",\"content\":\"Generated /tmp/icon_a.png\"}\n",
            "{\"step_index\":3,\"type\":\"PLANNER_RESPONSE\",\"status\":\"DONE\",\"content\":\"Sent icon path to parent.\"}\n",
        )).unwrap();
        db.execute("INSERT INTO steps VALUES (72, 101, 3, 0, ?1)", [agent_message_step("child-15", "/tmp/icon_a.png")]).unwrap();
        // Parent finishes its wake-up reply (step_type 15, status 3, no tool calls).
        db.execute("INSERT INTO steps VALUES (73, 15, 3, 0, ?1)", [field(STEP_PLANNER_RESPONSE, &field(1, b"All icons ready!"))]).unwrap();
        drop(db);

        let done = inspect_child(&home, "root-15", &child.conversation_id, &child.log_uri, &child.prompt).unwrap();
        assert_eq!(done.status, ChildState::Completed);
        assert_eq!(done.result.as_deref(), Some("/tmp/icon_a.png"));
        assert_eq!(done.steps.len(), 3);
        assert!(parent_turn_settled(&home, "root-15", Some(71)));
        assert!(!parent_turn_settled(&home, "root-15", Some(73)));
        let _ = std::fs::remove_dir_all(&home);
    }
}
