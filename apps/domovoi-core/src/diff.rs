//! Unified diffs (as `git diff` and the server print them) into files, hunks
//! and numbered lines; plus a small line diff for edits that arrive as
//! old/new strings.

use crate::view::{DiffFileView, DiffHunkView, DiffLineView};

fn strip_prefix(p: &str) -> String {
    p.strip_prefix("a/").or_else(|| p.strip_prefix("b/")).unwrap_or(p).to_owned()
}

/// `@@ -a,b +c,d @@` -> (a, c)
fn hunk_start(header: &str) -> (u32, u32) {
    let mut parts = header.split_whitespace().skip(1);
    // `get`, not `[1..]`: the token may start with a character wider than a byte.
    let num = |s: Option<&str>| s.and_then(|s| s.get(1..)?.split(',').next()?.parse().ok()).unwrap_or(1);
    (num(parts.next()), num(parts.next()))
}

pub fn parse_unified(diff: &str) -> Vec<DiffFileView> {
    let mut files: Vec<DiffFileView> = vec![];
    let (mut old_no, mut new_no) = (0u32, 0u32);
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let mut f = DiffFileView { change: "modified".into(), ..Default::default() };
            if let Some((a, b)) = rest.split_once(" b/") {
                f.path = b.to_owned();
                let a = strip_prefix(a);
                if a != f.path {
                    f.old_path = Some(a);
                }
            }
            files.push(f);
            continue;
        }
        let Some(f) = files.last_mut() else {
            // A diff without `diff --git` headers (e.g. plain `diff -u`).
            if line.starts_with("--- ") {
                files.push(DiffFileView { change: "modified".into(), ..Default::default() });
            }
            continue;
        };
        if line.starts_with("new file mode") {
            f.change = "added".into();
        } else if line.starts_with("deleted file mode") {
            f.change = "deleted".into();
        } else if line.starts_with("rename from ") {
            f.change = "renamed".into();
        } else if line.starts_with("Binary files ") {
            f.change = "binary".into();
        } else if let Some(p) = line.strip_prefix("+++ ") {
            if p != "/dev/null" && f.path.is_empty() {
                f.path = strip_prefix(p);
            }
        } else if let Some(p) = line.strip_prefix("--- ") {
            if p == "/dev/null" {
                f.change = "added".into();
            } else if f.path.is_empty() {
                f.path = strip_prefix(p);
            }
        } else if line.starts_with("@@") {
            let (o, n) = hunk_start(line);
            old_no = o;
            new_no = n;
            f.hunks.push(DiffHunkView { header: line.to_owned(), lines: vec![] });
        } else if let Some(h) = f.hunks.last_mut() {
            let (kind, text, old, new) = match line.as_bytes().first() {
                Some(b'+') => ("add", &line[1..], None, Some(new_no)),
                Some(b'-') => ("del", &line[1..], Some(old_no), None),
                Some(b'\\') => ("note", line, None, None),
                _ => ("ctx", line.strip_prefix(' ').unwrap_or(line), Some(old_no), Some(new_no)),
            };
            match kind {
                "add" => {
                    new_no = new_no.saturating_add(1);
                    f.additions += 1;
                }
                "del" => {
                    old_no = old_no.saturating_add(1);
                    f.deletions += 1;
                }
                "ctx" => {
                    old_no = old_no.saturating_add(1);
                    new_no = new_no.saturating_add(1);
                }
                _ => {}
            }
            h.lines.push(DiffLineView { kind: kind.into(), old_line: old, new_line: new, text: text.to_owned() });
        }
    }
    files
}

/// A one-hunk diff of two texts (longest-common-subsequence on lines).
pub fn diff_texts(path: &str, old: &str, new: &str) -> DiffFileView {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    // LCS table; edits are small (tool calls), so O(n*m) is fine with a cap.
    let mut lines = vec![];
    if a.len() * b.len() > 4_000_000 {
        lines.extend(a.iter().enumerate().map(|(i, t)| DiffLineView { kind: "del".into(), old_line: Some(i as u32 + 1), new_line: None, text: (*t).into() }));
        lines.extend(b.iter().enumerate().map(|(i, t)| DiffLineView { kind: "add".into(), old_line: None, new_line: Some(i as u32 + 1), text: (*t).into() }));
    } else {
        let mut t = vec![vec![0u32; b.len() + 1]; a.len() + 1];
        for i in (0..a.len()).rev() {
            for j in (0..b.len()).rev() {
                t[i][j] = if a[i] == b[j] { t[i + 1][j + 1] + 1 } else { t[i + 1][j].max(t[i][j + 1]) };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < a.len() || j < b.len() {
            if i < a.len() && j < b.len() && a[i] == b[j] {
                lines.push(DiffLineView { kind: "ctx".into(), old_line: Some(i as u32 + 1), new_line: Some(j as u32 + 1), text: a[i].into() });
                i += 1;
                j += 1;
            } else if i < a.len() && (j == b.len() || t[i + 1][j] >= t[i][j + 1]) {
                // Deletions before additions, as git prints them.
                lines.push(DiffLineView { kind: "del".into(), old_line: Some(i as u32 + 1), new_line: None, text: a[i].into() });
                i += 1;
            } else {
                lines.push(DiffLineView { kind: "add".into(), old_line: None, new_line: Some(j as u32 + 1), text: b[j].into() });
                j += 1;
            }
        }
    }
    let additions = lines.iter().filter(|l| l.kind == "add").count() as u32;
    let deletions = lines.iter().filter(|l| l.kind == "del").count() as u32;
    DiffFileView {
        path: path.into(),
        change: if old.is_empty() { "added".into() } else { "modified".into() },
        additions,
        deletions,
        hunks: vec![DiffHunkView { header: String::new(), lines }],
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_git_diffs() {
        let d = "diff --git a/src/a.rs b/src/a.rs\nindex 1..2 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -3,3 +3,4 @@ fn x\n ctx\n-old\n+new\n+more\n ctx2\ndiff --git a/new.txt b/new.txt\nnew file mode 100644\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+hi\n\\ No newline at end of file\n";
        let f = parse_unified(d);
        assert_eq!(f.len(), 2);
        assert_eq!((f[0].path.as_str(), f[0].additions, f[0].deletions), ("src/a.rs", 2, 1));
        let l = &f[0].hunks[0].lines;
        assert_eq!((l[0].old_line, l[0].new_line), (Some(3), Some(3)));
        assert_eq!((l[1].kind.as_str(), l[1].old_line), ("del", Some(4)));
        assert_eq!((l[2].kind.as_str(), l[2].new_line), ("add", Some(4)));
        assert_eq!((l[4].old_line, l[4].new_line), (Some(5), Some(6)));
        assert_eq!((f[1].change.as_str(), f[1].hunks[0].lines[1].kind.as_str()), ("added", "note"));
    }

    #[test]
    fn odd_hunk_headers_do_not_panic() {
        // Text after `@@` that starts with a wide character, and numbers too
        // big for a line count.
        let d = "--- a/x\n+++ b/x\n@@ 函数 @@\n+a\n@@ -1 +4294967295 @@\n+b\n c\n";
        let f = parse_unified(d);
        assert_eq!(f[0].hunks.len(), 2);
        assert_eq!(f[0].hunks[0].lines[0].new_line, Some(1), "an unreadable start counts from 1");
        assert_eq!(f[0].hunks[1].lines[1].new_line, Some(u32::MAX), "held at the largest line number");
    }

    #[test]
    fn diffs_two_texts() {
        let f = diff_texts("a.py", "a\nb\nc\n", "a\nB\nc\nd\n");
        let kinds: Vec<&str> = f.hunks[0].lines.iter().map(|l| l.kind.as_str()).collect();
        assert_eq!(kinds, ["ctx", "del", "add", "ctx", "add"]);
        assert_eq!((f.additions, f.deletions), (2, 1));
    }
}
