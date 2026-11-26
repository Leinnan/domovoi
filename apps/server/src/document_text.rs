//! Text of attached documents (t3code `provider/DocumentText.ts` and
//! `provider/documentTextWorker.ts`): the text of a PDF, DOCX, XLSX or PPTX
//! attachment, written beside it as `<file>.txt` (the sidecar), so the agent is
//! pointed at text it can read (`[Attached file "x.docx" is saved at: <path>
//! (text extracted to: <path>.txt)]`). Plain text, CSV and Markdown need no
//! sidecar: the agent reads the file itself.
//!
//! Extraction runs in a child process, `domovoi-server __document-text <path>
//! .<ext>` (hidden, like `acp-mcp-bridge`), so a malformed file can only take
//! the child down: it is killed after 15 seconds, its output past 4 MiB is
//! refused, two run at a time, and the input is capped at 16 MiB. A file the
//! worker rejects (exit 2: not a document, no text) is not tried again; any
//! other failure is retried after a second. The worker bounds its own work as
//! t3code's does: at most 2048 archive entries, 32 MiB expanded (and no entry
//! expanding more than 200 times), 200 000 XML nodes, nesting 128 deep,
//! 10 000 rows and 512 columns a sheet, and 1 000 000 characters of output
//! (then a note that the text was cut).
//!
//! Output, as t3code's: DOCX paragraphs (headings as `#`), tables as rows of
//! tab-separated cells, tracked deletions left out; PPTX `## Slide N` with
//! each slide's text and its `Notes:`; XLSX `## Sheet: <name>` with rows of
//! cells (shared and inline strings, booleans, values or formulas, a number
//! format noted as `[Excel numFmtId=...; date system 1900]`). PDF (which
//! t3code hands to the provider as it is) is `## Page N` with each page's text.
//!
//! Crates: `zip` (deflate through `flate2`'s pure-Rust backend only) for the
//! OOXML archives, `roxmltree` (already used for SVG) for their XML, and
//! `lopdf` without default features (no rayon or chrono) for PDF text, whose
//! `extract_text_with_limit` bounds each page's decompressed content. All are
//! pure Rust and maintained; `pdf-extract` would add font-encoding crates for
//! better spacing at several times the size.
//!
//! Limits: t3code's ODT/ODS/ODP, RTF, EPUB and notebook extraction is not
//! ported; PDF text is lopdf's (scanned pages give none, and the order is the
//! content stream's).

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The hidden subcommand the worker runs as.
pub const WORKER_COMMAND: &str = "__document-text";
const EXTENSIONS: [&str; 4] = ["pdf", "docx", "xlsx", "pptx"];
const MAX_INPUT: u64 = 16 * 1024 * 1024;
const MAX_EXPANDED: u64 = 32 * 1024 * 1024;
const MAX_ENTRIES: usize = 2048;
const MAX_NODES: u32 = 200_000;
const MAX_DEPTH: usize = 128;
const MAX_ROWS: usize = 10_000;
const MAX_COLUMNS: usize = 512;
const MAX_OUTPUT: usize = 1_000_000;
/// What the worker may write: its character budget, as UTF-8, and a note.
const MAX_STDOUT: usize = 4 * 1024 * 1024 + 1024;
const TIMEOUT: Duration = Duration::from_secs(15);
const TRUNCATED: &str = "\n\n[Extracted text truncated at document extraction limits.]";
/// The worker's exit code for a document it rejects.
const REJECTED: i32 = 2;

fn extension(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    EXTENSIONS.contains(&ext.as_str()).then_some(ext)
}

/// Where a document's text is kept.
pub fn sidecar(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".txt");
    PathBuf::from(s)
}

/// `resolveExtractedTextPath`: the sidecar of a document whose text was extracted.
pub fn extracted_text_path(path: &Path) -> Option<PathBuf> {
    extension(path)?;
    let s = sidecar(path);
    s.is_file().then_some(s)
}

// ---- the parent: one child per document ----

/// When each failed document may be tried again (`None`: never).
fn failed() -> &'static Mutex<HashMap<PathBuf, Option<Instant>>> {
    static FAILED: OnceLock<Mutex<HashMap<PathBuf, Option<Instant>>>> = OnceLock::new();
    FAILED.get_or_init(Default::default)
}

fn slots() -> &'static tokio::sync::Semaphore {
    static SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
    &SLOTS
}

/// Documents being extracted, so a second ask waits for the first.
fn inflight() -> &'static Mutex<HashMap<PathBuf, std::sync::Arc<tokio::sync::Mutex<()>>>> {
    static INFLIGHT: OnceLock<Mutex<HashMap<PathBuf, std::sync::Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    INFLIGHT.get_or_init(Default::default)
}

/// `DocumentText.ensure`: the sidecar of a document, extracted first if it is not there.
pub async fn ensure(path: &Path) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    ensure_with(&exe, path).await
}

/// Starts extracting an uploaded document's text, so it is ready when the message is sent.
pub fn prepare(path: &Path) {
    if extension(path).is_none() {
        return;
    }
    if let Ok(rt) = tokio::runtime::Handle::try_current() {
        let path = path.to_path_buf();
        rt.spawn(async move { ensure(&path).await });
    }
}

/// [`ensure`] with `exe` as the worker.
pub async fn ensure_with(exe: &Path, path: &Path) -> Option<PathBuf> {
    let ext = extension(path)?;
    let out = sidecar(path);
    if out.is_file() {
        return Some(out);
    }
    let one = crate::util::locked(inflight()).entry(path.to_path_buf()).or_default().clone();
    let _one = one.lock().await;
    let result = extract(exe, path, &ext, &out).await;
    crate::util::locked(inflight()).remove(path);
    result
}

async fn extract(exe: &Path, path: &Path, ext: &str, out: &Path) -> Option<PathBuf> {
    if out.is_file() {
        return Some(out.to_path_buf());
    }
    if let Some(until) = crate::util::locked(failed()).get(path) {
        if until.is_none_or(|t| Instant::now() < t) {
            return None;
        }
    }
    if std::fs::metadata(path).ok()?.len() > MAX_INPUT {
        return None;
    }
    let _slot = slots().acquire().await.ok()?;
    match run_worker(exe, path, ext).await {
        Ok(text) => {
            let pending = PathBuf::from(format!("{}.{}.part", out.display(), crate::util::new_uuid()));
            let written = std::fs::write(&pending, text).and_then(|()| std::fs::rename(&pending, out));
            let _ = std::fs::remove_file(&pending);
            written.ok().map(|()| out.to_path_buf())
        }
        Err(rejected) => {
            let mut f = crate::util::locked(failed());
            if f.len() >= 1024
                && let Some(k) = f.keys().next().cloned()
            {
                f.remove(&k);
            }
            f.insert(path.to_path_buf(), (!rejected).then(|| Instant::now() + Duration::from_secs(1)));
            tracing::warn!(extension = ext, "Document text extraction failed.");
            None
        }
    }
}

/// The worker's text, or whether it rejected the document (`Err(true)`) or failed otherwise.
async fn run_worker(exe: &Path, path: &Path, ext: &str) -> Result<String, bool> {
    use tokio::io::AsyncReadExt;
    let mut cmd = crate::process::command(exe);
    cmd.arg(WORKER_COMMAND).arg(path).arg(format!(".{ext}")).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null());
    let mut child = cmd.spawn().map_err(|_| false)?;
    let mut stdout = child.stdout.take().ok_or(false)?;
    let run = async {
        let mut bytes = Vec::new();
        (&mut stdout).take(MAX_STDOUT as u64 + 1).read_to_end(&mut bytes).await.map_err(|_| false)?;
        if bytes.len() > MAX_STDOUT {
            return Err(false);
        }
        let status = child.wait().await.map_err(|_| false)?;
        match status.code() {
            Some(0) => String::from_utf8(bytes).map_err(|_| true),
            Some(REJECTED) => Err(true),
            _ => Err(false),
        }
    };
    // Dropping the child on a timeout kills it (`crate::process::command`).
    tokio::time::timeout(TIMEOUT, run).await.unwrap_or(Err(false))
}

// ---- the worker ----

/// Why extraction stopped early: the text so far is kept, with a note.
struct Truncation;

/// A document that can't be read.
#[derive(Debug)]
pub struct Rejected(pub String);

impl From<std::io::Error> for Rejected {
    fn from(e: std::io::Error) -> Self {
        Rejected(e.to_string())
    }
}

/// `__document-text <path> <.ext>`: writes the document's text to stdout;
/// the exit code is 2 for a document it rejects, 1 when stdout fails.
pub fn worker_main(path: &Path, extension: &str) -> i32 {
    use std::io::Write;
    match extract_document_text(path, extension) {
        Ok(text) => {
            let mut out = std::io::stdout().lock();
            if out.write_all(text.as_bytes()).and_then(|()| out.flush()).is_err() {
                return 1;
            }
            0
        }
        Err(Rejected(why)) => {
            eprintln!("{why}");
            REJECTED
        }
    }
}

/// Collects text within the character budget.
struct Out {
    text: String,
    remaining: usize,
}

impl Out {
    fn push(&mut self, s: &str) -> Result<(), Truncation> {
        let n = s.chars().count();
        if n >= self.remaining {
            self.text.extend(s.chars().take(self.remaining).filter(|c| *c != '\0'));
            self.remaining = 0;
            return Err(Truncation);
        }
        self.remaining -= n;
        self.text.extend(s.chars().filter(|c| *c != '\0'));
        Ok(())
    }
}

/// `extractDocumentText`.
pub fn extract_document_text(path: &Path, extension: &str) -> Result<String, Rejected> {
    if std::fs::metadata(path)?.len() > MAX_INPUT {
        return Ok(finish(String::new(), true));
    }
    let mut out = Out { text: String::new(), remaining: MAX_OUTPUT };
    let done = match extension.trim_start_matches('.').to_ascii_lowercase().as_str() {
        "pdf" => pdf(path, &mut out)?,
        "docx" | "xlsx" | "pptx" => {
            let ext = extension.trim_start_matches('.').to_ascii_lowercase();
            let (files, truncated) = archive(path)?;
            if truncated {
                Err(Truncation)
            } else {
                match ext.as_str() {
                    "docx" => docx(&files, &mut out)?,
                    "xlsx" => xlsx(&files, &mut out)?,
                    _ => pptx(&files, &mut out)?,
                }
            }
        }
        other => return Err(Rejected(format!("Unsupported document .{other}"))),
    };
    let truncated = done.is_err();
    let text = finish(out.text, truncated);
    if text.is_empty() {
        return Err(Rejected("No text extracted".into()));
    }
    Ok(text)
}

/// Collapses runs of blank lines, trims, and notes a cut.
fn finish(text: String, truncated: bool) -> String {
    let mut lines: Vec<&str> = vec![];
    let mut blank = 0;
    for line in text.split('\n') {
        if line.trim_matches([' ', '\t']).is_empty() {
            blank += 1;
            if blank == 1 {
                lines.push("");
            }
            continue;
        }
        blank = 0;
        lines.push(line);
    }
    let joined = lines.join("\n");
    let trimmed = joined.trim();
    if truncated {
        format!("{trimmed}{TRUNCATED}")
    } else {
        trimmed.to_owned()
    }
}

fn pdf(path: &Path, out: &mut Out) -> Result<Result<(), Truncation>, Rejected> {
    let bytes = std::fs::read(path)?;
    let doc = lopdf::Document::load_mem(&bytes).map_err(|e| Rejected(format!("Invalid PDF: {e}")))?;
    let pages: Vec<u32> = doc.get_pages().keys().copied().collect();
    let mut budget = MAX_EXPANDED as usize;
    for page in pages {
        let text = doc.extract_text_with_limit(&[page], budget).unwrap_or_default();
        budget = budget.saturating_sub(text.len());
        let step = out.push(&format!("\n\n## Page {page}\n")).and_then(|()| out.push(&text));
        if step.is_err() || budget == 0 {
            return Ok(Err(Truncation));
        }
    }
    Ok(Ok(()))
}

/// The XML parts of an OOXML archive by name, and whether a limit was reached.
fn archive(path: &Path) -> Result<(HashMap<String, String>, bool), Rejected> {
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| Rejected(format!("Invalid archive: {e}")))?;
    if zip.len() > MAX_ENTRIES {
        return Ok((HashMap::new(), true));
    }
    let mut files = HashMap::new();
    let mut expanded = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| Rejected(format!("Unreadable archive entry: {e}")))?;
        let Ok(name) = entry.name().map(|n| n.into_owned()) else { continue };
        let wanted = [".xml", ".rels"].iter().any(|s| name.to_ascii_lowercase().ends_with(s));
        if !wanted {
            continue;
        }
        let size = entry.size();
        if size > MAX_EXPANDED || size > (entry.compressed_size() * 200).max(1024) || expanded + size > MAX_EXPANDED {
            return Ok((files, true));
        }
        let mut data = Vec::with_capacity(size as usize);
        (&mut entry).take(size + 1).read_to_end(&mut data).map_err(|e| Rejected(format!("Unreadable archive entry: {e}")))?;
        if data.len() as u64 > size {
            return Ok((files, true));
        }
        expanded += data.len() as u64;
        files.insert(name, decode(&data));
    }
    Ok((files, false))
}

/// An XML part's text: UTF-16 by its byte order mark or first character, else UTF-8.
fn decode(data: &[u8]) -> String {
    let utf16 = |le: bool, body: &[u8]| {
        let units: Vec<u16> = body.chunks_exact(2).map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) }).collect();
        String::from_utf16_lossy(&units)
    };
    match data {
        [0xff, 0xfe, rest @ ..] => utf16(true, rest),
        [0x3c, 0, ..] => utf16(true, data),
        [0xfe, 0xff, rest @ ..] => utf16(false, rest),
        [0, 0x3c, ..] => utf16(false, data),
        [0xef, 0xbb, 0xbf, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => String::from_utf8_lossy(data).into_owned(),
    }
}

type Node<'a, 'b> = roxmltree::Node<'a, 'b>;

fn parse(text: &str) -> Option<roxmltree::Document<'_>> {
    roxmltree::Document::parse_with_options(text, roxmltree::ParsingOptions { allow_dtd: false, nodes_limit: MAX_NODES, ..Default::default() }).ok()
}

/// Parts the document never shows.
const HIDDEN: [&str; 12] = ["rPh", "delText", "moveFrom", "instrText", "delInstrText", "pPr", "positionH", "positionV", "sizeRelH", "sizeRelV", "fldData", "del"];

fn name<'a>(n: &Node<'a, '_>) -> &'a str {
    n.tag_name().name()
}

/// An `mc:Fallback` whose sibling `mc:Choice` already has the text.
fn redundant_fallback(n: &Node) -> bool {
    name(n) == "Fallback" && n.parent().is_some_and(|p| p.children().any(|c| name(&c) == "Choice" && has_text(&c, 0)))
}

fn has_text(n: &Node, depth: usize) -> bool {
    if depth > MAX_DEPTH || HIDDEN.contains(&name(n)) {
        return false;
    }
    n.children().any(|c| if c.is_text() { c.text().is_some_and(|t| !t.trim().is_empty()) && is_text_run(&c) } else { has_text(&c, depth + 1) })
}

/// Character data that is shown: inside `w:t`, `a:t`, `t` (sheets) or `v`, not between elements.
fn is_text_run(text: &Node) -> bool {
    text.parent().is_some_and(|p| matches!(name(&p), "t" | "v" | "f"))
}

/// t3code's `text`: a run's characters, tabs and breaks.
fn run_text(n: &Node, depth: usize, buf: &mut String) -> Result<(), Truncation> {
    if depth > MAX_DEPTH {
        return Err(Truncation);
    }
    for c in n.children() {
        if c.is_text() {
            if is_text_run(&c) {
                buf.push_str(c.text().unwrap_or_default());
            }
            continue;
        }
        if !c.is_element() || HIDDEN.contains(&name(&c)) || redundant_fallback(&c) {
            continue;
        }
        match name(&c) {
            "tab" | "ptab" => buf.push('\t'),
            "br" | "cr" => buf.push('\n'),
            "noBreakHyphen" => buf.push('\u{2011}'),
            _ => run_text(&c, depth + 1, buf)?,
        }
    }
    Ok(())
}

/// t3code's `paragraphs`: block content, paragraphs and tables, as lines.
fn paragraphs(n: &Node, depth: usize, out: &mut Out) -> Result<(), Truncation> {
    if depth > MAX_DEPTH {
        return Err(Truncation);
    }
    for c in n.children().filter(|c| c.is_element()) {
        if HIDDEN.contains(&name(&c)) || redundant_fallback(&c) {
            continue;
        }
        match name(&c) {
            "br" | "cr" => out.push("\n")?,
            "tbl" => table(&c, depth + 1, out)?,
            "p" => {
                let style = c.descendants().find(|d| name(d) == "pStyle").and_then(|d| d.attributes().find(|a| a.name() == "val").map(|a| a.value().to_owned())).unwrap_or_default();
                let level = style.strip_prefix("Heading").or_else(|| style.strip_prefix("heading")).and_then(|l| l.parse::<usize>().ok()).filter(|l| (1..=6).contains(l));
                if let Some(level) = level {
                    out.push(&format!("{} ", "#".repeat(level)))?;
                }
                let mut buf = String::new();
                run_text(&c, depth + 1, &mut buf)?;
                out.push(&buf)?;
                out.push("\n")?;
            }
            _ => paragraphs(&c, depth + 1, out)?,
        }
    }
    Ok(())
}

/// A cell's text on one line: whitespace collapsed, trimmed.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A cell's text as paragraphs would write it, then on one line.
fn cell_text(cell: &Node, depth: usize) -> Result<String, Truncation> {
    let mut inner = Out { text: String::new(), remaining: MAX_OUTPUT };
    paragraphs(cell, depth, &mut inner)?;
    Ok(one_line(&inner.text))
}

/// t3code's `table`: rows of tab-separated cells, trailing empty cells dropped.
fn table(n: &Node, depth: usize, out: &mut Out) -> Result<(), Truncation> {
    let mut rows = 0;
    for row in n.children().filter(|c| name(c) == "tr") {
        let mut cells: Vec<String> = vec![];
        for cell in row.children().filter(|c| name(c) == "tc") {
            if cells.len() >= MAX_COLUMNS {
                return Err(Truncation);
            }
            cells.push(cell_text(&cell, depth + 1)?);
        }
        while cells.last().is_some_and(String::is_empty) {
            cells.pop();
        }
        if cells.is_empty() {
            continue;
        }
        rows += 1;
        if rows > MAX_ROWS {
            return Err(Truncation);
        }
        out.push(&cells.join("\t"))?;
        out.push("\n")?;
    }
    Ok(())
}

/// A missing or broken part is noted, as t3code notes it.
fn skipped(notes: &mut Vec<String>, part: &str) {
    notes.push(format!("\n[Skipped document part: {part} (missing or invalid XML).]\n"));
}

fn with_part<T>(files: &HashMap<String, String>, part: &str, notes: &mut Vec<String>, f: impl FnOnce(&roxmltree::Document) -> T) -> Option<T> {
    match files.get(part).and_then(|t| parse(t)) {
        Some(doc) => Some(f(&doc)),
        None => {
            skipped(notes, part);
            None
        }
    }
}

fn finish_notes(notes: Vec<String>, out: &mut Out) -> Result<(), Truncation> {
    for n in notes {
        out.push(&n)?;
    }
    Ok(())
}

fn docx(files: &HashMap<String, String>, out: &mut Out) -> Result<Result<(), Truncation>, Rejected> {
    let mut notes = vec![];
    let r = with_part(files, "word/document.xml", &mut notes, |doc| paragraphs(&doc.root_element(), 0, out)).unwrap_or(Ok(()));
    Ok(r.and_then(|()| finish_notes(notes, out)))
}

/// A part's relationships: id to target.
fn relations(files: &HashMap<String, String>, part: &str, notes: &mut Vec<String>) -> HashMap<String, (String, String, bool)> {
    with_part(files, part, notes, |doc| {
        doc.descendants()
            .filter(|n| name(n) == "Relationship")
            .filter_map(|n| Some((n.attribute("Id")?.to_owned(), (n.attribute("Target")?.to_owned(), n.attribute("Type").unwrap_or_default().to_owned(), n.attribute("TargetMode") == Some("External")))))
            .collect()
    })
    .unwrap_or_default()
}

/// `a/b/../c` as `a/c`.
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = vec![];
    for p in path.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

fn resolve(dir: &str, target: &str) -> String {
    match target.strip_prefix('/') {
        Some(abs) => normalize(abs),
        None => normalize(&format!("{dir}/{target}")),
    }
}

/// The `r:id` attribute of a sheet or slide.
fn rel_id(n: &Node) -> Option<String> {
    n.attributes().find(|a| a.name() == "id" && a.namespace().is_some_and(|ns| ns.contains("relationships"))).or_else(|| n.attributes().find(|a| a.name() == "id")).map(|a| a.value().to_owned())
}

fn pptx(files: &HashMap<String, String>, out: &mut Out) -> Result<Result<(), Truncation>, Rejected> {
    let mut notes = vec![];
    let rels = relations(files, "ppt/_rels/presentation.xml.rels", &mut notes);
    let slides: Vec<Option<String>> = with_part(files, "ppt/presentation.xml", &mut notes, |doc| doc.descendants().filter(|n| name(n) == "sldId").map(|n| rel_id(&n)).collect()).unwrap_or_default();
    let run = (|| -> Result<(), Truncation> {
        for (i, id) in slides.iter().enumerate() {
            let index = i + 1;
            out.push(&format!("\n\n## Slide {index}\n"))?;
            let Some((target, _, _)) = id.as_ref().and_then(|id| rels.get(id)) else {
                skipped(&mut notes, &format!("slide relationship {index}"));
                continue;
            };
            let part = resolve("ppt", target);
            if let Some(r) = with_part(files, &part, &mut notes, |doc| paragraphs(&doc.root_element(), 0, out)) {
                r?;
            }
            let (dir, base) = part.rsplit_once('/').unwrap_or(("", &part));
            let slide_rels = format!("{dir}/_rels/{base}.rels");
            if !files.contains_key(&slide_rels) {
                continue;
            }
            let mut slide_notes = vec![];
            let rels = relations(files, &slide_rels, &mut slide_notes);
            let mut targets: Vec<&(String, String, bool)> = rels.values().filter(|(_, ty, external)| ty.ends_with("/notesSlide") && !external).collect();
            targets.sort();
            for (target, _, _) in targets {
                out.push("Notes:\n")?;
                if let Some(r) = with_part(files, &resolve(dir, target), &mut notes, |doc| paragraphs(&doc.root_element(), 0, out)) {
                    r?;
                }
            }
            notes.extend(slide_notes);
        }
        Ok(())
    })();
    Ok(run.and_then(|()| finish_notes(notes, out)))
}

/// t3code's `xstringText`: the `t`/`v` text of a shared or inline string, `_xHHHH_` escapes decoded.
fn xstring(n: &Node) -> String {
    let mut raw = String::new();
    for d in n.descendants().filter(|d| matches!(name(d), "t" | "v") && !d.ancestors().any(|a| a != *n && HIDDEN.contains(&name(&a)))) {
        raw.push_str(&d.children().filter(|c| c.is_text()).filter_map(|c| c.text()).collect::<String>());
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw.as_str();
    while let Some(i) = rest.find("_x") {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let hex = tail.get(2..6).filter(|h| h.bytes().all(|b| b.is_ascii_hexdigit()));
        match hex {
            Some(h) if tail.as_bytes().get(6) == Some(&b'_') => {
                if h.eq_ignore_ascii_case("005f") && tail.get(7..13).is_some_and(|x| x.starts_with('x') && x[1..5].bytes().all(|b| b.is_ascii_hexdigit()) && x.ends_with('_')) {
                    // `_x005F_` escapes a literal `_xHHHH_`.
                    out.push('_');
                    out.push_str(&tail[7..13]);
                    rest = &tail[13..];
                } else {
                    out.push(u32::from_str_radix(h, 16).ok().and_then(char::from_u32).unwrap_or('\u{fffd}'));
                    rest = &tail[7..];
                }
            }
            _ => {
                out.push_str("_x");
                rest = &tail[2..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// A cell's column from its reference (`C7` is 2).
fn column_of(reference: &str) -> Option<usize> {
    let letters: String = reference.chars().take_while(char::is_ascii_uppercase).collect();
    (!letters.is_empty()).then(|| letters.bytes().fold(0usize, |n, b| n.saturating_mul(26).saturating_add((b - b'A' + 1) as usize)) - 1)
}

fn xlsx(files: &HashMap<String, String>, out: &mut Out) -> Result<Result<(), Truncation>, Rejected> {
    let mut notes = vec![];
    struct Sheet {
        name: String,
        id: Option<String>,
    }
    let (sheets, date_system) = with_part(files, "xl/workbook.xml", &mut notes, |doc| {
        let sheets: Vec<Sheet> = doc.descendants().filter(|n| name(n) == "sheet").map(|n| Sheet { name: n.attribute("name").unwrap_or_default().to_owned(), id: rel_id(&n) }).collect();
        let d1904 = doc.descendants().find(|n| name(n) == "workbookPr").and_then(|n| n.attribute("date1904")).is_some_and(|v| v == "1" || v == "true");
        (sheets, if d1904 { "1904" } else { "1900" })
    })
    .unwrap_or((vec![], "1900"));
    let (custom, cell_formats): (HashMap<String, String>, Vec<Option<String>>) = files
        .get("xl/styles.xml")
        .and_then(|t| parse(t).map(|doc| {
            let custom = doc.descendants().filter(|n| name(n) == "numFmt").filter_map(|n| Some((n.attribute("numFmtId")?.to_owned(), n.attribute("formatCode").unwrap_or_default().to_owned()))).collect();
            let xfs = doc.descendants().find(|n| name(n) == "cellXfs").map(|x| x.children().filter(|c| c.is_element()).map(|c| c.attribute("numFmtId").map(str::to_owned)).collect()).unwrap_or_default();
            (custom, xfs)
        }))
        .unwrap_or_default();
    let strings: Vec<String> = files.get("xl/sharedStrings.xml").and_then(|t| parse(t).map(|doc| doc.descendants().filter(|n| name(n) == "si").map(|n| xstring(&n)).collect())).unwrap_or_default();
    let rels = relations(files, "xl/_rels/workbook.xml.rels", &mut notes);
    let run = (|| -> Result<(), Truncation> {
        let mut rows = 0;
        for sheet in &sheets {
            out.push(&format!("\n## Sheet: {}\n", sheet.name))?;
            let Some((target, _, _)) = sheet.id.as_ref().and_then(|id| rels.get(id)) else {
                skipped(&mut notes, &format!("sheet relationship {}", sheet.name));
                continue;
            };
            let part = resolve("xl", target);
            let Some(text) = files.get(&part) else {
                skipped(&mut notes, &part);
                continue;
            };
            let Some(doc) = parse(text) else {
                skipped(&mut notes, &part);
                continue;
            };
            for row in doc.descendants().filter(|n| name(n) == "row") {
                let mut cells: Vec<Option<String>> = vec![];
                let mut next = 0;
                let mut wide = false;
                for cell in row.children().filter(|c| name(c) == "c") {
                    let column = cell.attribute("r").and_then(column_of).unwrap_or(next);
                    next = column + 1;
                    let value = cell.children().find(|c| name(c) == "v");
                    let raw = value.map(|v| v.text().unwrap_or_default().to_owned()).unwrap_or_default();
                    let text = match cell.attribute("t") {
                        Some("s") => raw.trim().parse::<usize>().ok().and_then(|i| strings.get(i)).cloned().unwrap_or_default(),
                        Some("inlineStr") => xstring(&cell),
                        Some("b") if matches!(raw.trim(), "0" | "1") => (if raw.trim() == "1" { "true" } else { "false" }).to_owned(),
                        t => {
                            let shown = if !raw.trim().is_empty() {
                                raw.clone()
                            } else {
                                cell.children().filter(|c| name(c) == "f").map(|f| f.text().unwrap_or_default().to_owned()).collect()
                            };
                            let format = cell.attribute("s").unwrap_or("0").parse::<usize>().ok().and_then(|i| cell_formats.get(i).cloned().flatten());
                            match format {
                                Some(id) if matches!(t, None | Some("n")) && !raw.trim().is_empty() && id != "0" => {
                                    let code = custom.get(&id).map(|c| format!("; format={c}")).unwrap_or_default();
                                    format!("{shown} [Excel numFmtId={id}{code}; date system {date_system}]")
                                }
                                _ => shown,
                            }
                        }
                    };
                    let text = one_line(&text);
                    if text.is_empty() {
                        continue;
                    }
                    if column >= MAX_COLUMNS {
                        wide = true;
                        continue;
                    }
                    if cells.len() <= column {
                        cells.resize(column + 1, None);
                    }
                    cells[column] = Some(text);
                }
                if cells.is_empty() && !wide {
                    continue;
                }
                rows += 1;
                if rows > MAX_ROWS {
                    return Err(Truncation);
                }
                let line: Vec<String> = cells.into_iter().map(Option::unwrap_or_default).collect();
                out.push(&line.join("\t"))?;
                out.push("\n")?;
                if wide {
                    return Err(Truncation);
                }
            }
        }
        Ok(())
    })();
    Ok(run.and_then(|()| finish_notes(notes, out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/documents").join(name)
    }

    #[test]
    fn extracts_docx_with_headings_tables_and_no_deletions() {
        let text = extract_document_text(&fixture("report.docx"), ".docx").unwrap();
        assert_eq!(text, "# Quarterly report\nRevenue grew\tby 12%.\nRegion\tSales\nNorth\t1 200\nSouth\t900\nThe end.");
    }

    #[test]
    fn extracts_xlsx_sheets_rows_and_formats() {
        let text = extract_document_text(&fixture("budget.xlsx"), ".xlsx").unwrap();
        assert_eq!(text, "## Sheet: Budget\nItem\tCost\tPaid\nRent\t1200\ttrue\nTotal\t1200\nDue\t46000 [Excel numFmtId=14; date system 1900]\n\n## Sheet: Notes\n\tSee_x_ A");
    }

    #[test]
    fn extracts_pptx_slides_with_notes() {
        let text = extract_document_text(&fixture("deck.pptx"), ".pptx").unwrap();
        assert_eq!(text, "## Slide 1\nWelcome\nAgenda for today\nNotes:\nSay hello first\n\n## Slide 2\nThanks");
    }

    #[test]
    fn extracts_pdf_pages() {
        let text = extract_document_text(&fixture("letter.pdf"), ".pdf").unwrap();
        assert!(text.starts_with("## Page 1\n"), "{text:?}");
        assert!(text.contains("Hello from a PDF"), "{text:?}");
        assert!(text.contains("## Page 2") && text.contains("Second page"), "{text:?}");
    }

    #[test]
    fn rejects_what_is_no_document() {
        assert!(extract_document_text(&fixture("broken.docx"), ".docx").is_err());
        assert!(extract_document_text(&fixture("broken.pdf"), ".pdf").is_err());
        assert!(extract_document_text(&fixture("letter.pdf"), ".exe").is_err());
    }

    #[test]
    fn stops_at_the_expansion_limits() {
        // One entry expanding far more than 200 times its compressed size.
        let text = extract_document_text(&fixture("bomb.docx"), ".docx").unwrap();
        assert_eq!(text, TRUNCATED);
    }

    #[test]
    fn output_is_cut_at_the_budget() {
        let mut out = Out { text: String::new(), remaining: 5 };
        assert!(out.push("abc").is_ok());
        assert!(out.push("defg").is_err());
        assert_eq!(finish(out.text, true), format!("abcde{TRUNCATED}"));
        assert_eq!(finish("\n a\n\n\n\nb \n".into(), false), "a\n\nb");
    }

    #[test]
    fn decodes_escapes_and_columns() {
        let doc = roxmltree::Document::parse("<si><t>a_x0041_b_x005F_x0041_c_xZZ</t></si>").unwrap();
        assert_eq!(xstring(&doc.root_element()), "aAb_x0041_c_xZZ");
        assert_eq!((column_of("A1"), column_of("AA3"), column_of("7")), (Some(0), Some(26), None));
        assert_eq!(resolve("ppt", "slides/../slides/slide1.xml"), "ppt/slides/slide1.xml");
        assert_eq!(resolve("ppt/slides", "/ppt/notesSlides/n.xml"), "ppt/notesSlides/n.xml");
    }

    /// A stand-in worker: a shell script answering as told.
    #[cfg(unix)]
    fn script(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(format!("worker-{}.sh", crate::util::new_uuid()));
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_child_writes_a_sidecar_once_and_failures_are_remembered() {
        let dir = std::env::temp_dir().join(format!("domovoi-doctext-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&dir).unwrap();
        let doc = dir.join("a.docx");
        std::fs::write(&doc, b"x").unwrap();
        let ok = script(&dir, &format!("[ \"$1\" = {WORKER_COMMAND} ] && [ \"$3\" = .docx ] && printf 'the text'"));
        assert_eq!(ensure_with(&ok, &doc).await, Some(sidecar(&doc)));
        assert_eq!(std::fs::read_to_string(sidecar(&doc)).unwrap(), "the text");
        assert_eq!(extracted_text_path(&doc), Some(sidecar(&doc)));
        // Kept: the worker is not run again.
        let never = script(&dir, "exit 9");
        assert_eq!(ensure_with(&never, &doc).await, Some(sidecar(&doc)));

        // A rejected document is not tried again.
        let rejected = dir.join("b.pptx");
        std::fs::write(&rejected, b"x").unwrap();
        assert_eq!(ensure_with(&script(&dir, "exit 2"), &rejected).await, None);
        assert_eq!(ensure_with(&ok, &rejected).await, None, "rejections are remembered");
        // Another failure is tried again after a second.
        let flaky = dir.join("c.xlsx");
        std::fs::write(&flaky, b"x").unwrap();
        assert_eq!(ensure_with(&script(&dir, "exit 1"), &flaky).await, None);
        assert_eq!(ensure_with(&script(&dir, "printf later"), &flaky).await, None, "within the second");
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(ensure_with(&script(&dir, "printf later"), &flaky).await.is_some());

        // Output past the cap is refused; plain text is no document.
        let big = dir.join("d.pdf");
        std::fs::write(&big, b"x").unwrap();
        assert_eq!(ensure_with(&script(&dir, &format!("head -c {} /dev/zero", MAX_STDOUT + 10)), &big).await, None);
        assert!(!sidecar(&big).exists());
        let txt = dir.join("e.txt");
        std::fs::write(&txt, b"plain").unwrap();
        assert_eq!(ensure_with(&ok, &txt).await, None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_hung_child_is_killed() {
        let dir = std::env::temp_dir().join(format!("domovoi-doctext-hang-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&dir).unwrap();
        let doc = dir.join("slow.docx");
        std::fs::write(&doc, b"x").unwrap();
        let started = Instant::now();
        assert_eq!(ensure_with(&script(&dir, "sleep 60"), &doc).await, None);
        let took = started.elapsed();
        assert!(took >= TIMEOUT && took < TIMEOUT + Duration::from_secs(10), "{took:?}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
