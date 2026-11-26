//! Graph queries over the SQLite codebase index (`repo_index_*`).

use super::IndexedSymbol;
use crate::db::Db;
use rusqlite::params;
use serde::Serialize;
use std::collections::{BTreeSet, HashSet};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelatedFile {
    pub path: String,
    /// `caller` | `dependency` | `test`
    pub relation: String,
    pub linked_to: String,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexContext {
    pub related_files: Vec<RelatedFile>,
    pub dependent_packages: Vec<String>,
    pub touched_packages: Vec<String>,
    pub convention_paths: Vec<String>,
    pub symbols: Vec<IndexedSymbol>,
}

/// Queries callers, dependencies, nearest tests, touched/dependent packages,
/// convention docs, and symbols for `changed_paths`.
pub fn context_for_changed_files(db: &Db, project_id: &str, changed_paths: &[String], max_related: usize) -> IndexContext {
    let changed_set: HashSet<&str> = changed_paths.iter().map(String::as_str).collect();
    let status = super::status(db, project_id);
    let convention_paths = status.convention_paths;

    let mut related = Vec::new();
    let mut seen_related: HashSet<String> = HashSet::new();
    let mut touched_packages = BTreeSet::new();
    let mut dependent_packages = BTreeSet::new();
    let mut symbols = Vec::new();

    let _ = db.read(|c| {
        // 1. Touched packages and symbols in changed files.
        let mut pkg_stmt = c.prepare("SELECT package_path FROM repo_index_files WHERE project_id = ?1 AND path = ?2")?;
        let mut sym_stmt = c.prepare(
            "SELECT path, name, kind, start_line, end_line, signature, is_exported
             FROM repo_index_symbols WHERE project_id = ?1 AND path = ?2 ORDER BY start_line LIMIT 64",
        )?;
        for path in changed_paths {
            if let Ok(Some(pkg)) = pkg_stmt.query_row(params![project_id, path], |r| r.get::<_, Option<String>>(0)) {
                touched_packages.insert(pkg);
            }
            if let Ok(rows) = sym_stmt.query_map(params![project_id, path], |r| {
                Ok(IndexedSymbol {
                    path: r.get(0)?,
                    name: r.get(1)?,
                    kind: r.get(2)?,
                    start_line: r.get(3)?,
                    end_line: r.get(4)?,
                    signature: r.get(5)?,
                    is_exported: r.get::<_, i64>(6)? != 0,
                })
            }) {
                for sym in rows.flatten() {
                    symbols.push(sym);
                }
            }
        }

        // 2. Nearest tests (`test_for`), callers (reverse `import`), and direct dependencies (forward `import`).
        let mut rev_stmt = c.prepare("SELECT source_path, kind FROM repo_index_edges WHERE project_id = ?1 AND target_path = ?2")?;
        let mut fwd_stmt = c.prepare("SELECT target_path, kind FROM repo_index_edges WHERE project_id = ?1 AND source_path = ?2")?;

        let mut tests = Vec::new();
        let mut callers = Vec::new();
        let mut deps = Vec::new();

        for path in changed_paths {
            if let Ok(rows) = rev_stmt.query_map(params![project_id, path], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))) {
                for (src, kind) in rows.flatten() {
                    if changed_set.contains(src.as_str()) {
                        continue;
                    }
                    if kind == "test_for" {
                        tests.push((src, path.clone()));
                    } else if kind == "import" {
                        callers.push((src, path.clone()));
                    }
                }
            }
            if let Ok(rows) = fwd_stmt.query_map(params![project_id, path], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))) {
                for (dst, kind) in rows.flatten() {
                    if changed_set.contains(dst.as_str()) {
                        continue;
                    }
                    if kind == "import" {
                        deps.push((dst, path.clone()));
                    }
                }
            }
        }

        // Prioritize: nearest tests first, then callers (blast radius), then direct dependencies.
        for (list, rel) in [(tests, "test"), (callers, "caller"), (deps, "dependency")] {
            for (path, linked_to) in list {
                if related.len() >= max_related {
                    break;
                }
                if seen_related.insert(path.clone()) {
                    if rel == "caller"
                        && let Ok(Some(pkg)) = pkg_stmt.query_row(params![project_id, &path], |r| r.get::<_, Option<String>>(0))
                        && !touched_packages.contains(&pkg)
                    {
                        dependent_packages.insert(pkg);
                    }
                    related.push(RelatedFile { path, relation: rel.into(), linked_to });
                }
            }
        }

        Ok::<_, rusqlite::Error>(())
    });

    IndexContext {
        related_files: related,
        dependent_packages: dependent_packages.into_iter().collect(),
        touched_packages: touched_packages.into_iter().collect(),
        convention_paths,
        symbols,
    }
}

/// Returns import adjacency between a set of changed files (used by graph-aware chunking).
pub fn edges_between(db: &Db, project_id: &str, paths: &[String]) -> Vec<(String, String)> {
    let set: HashSet<&str> = paths.iter().map(String::as_str).collect();
    let mut pairs = Vec::new();
    let _ = db.read(|c| {
        let mut stmt = c.prepare("SELECT source_path, target_path FROM repo_index_edges WHERE project_id = ?1")?;
        let rows = stmt.query_map(params![project_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for (a, b) in rows.flatten() {
            if set.contains(a.as_str()) && set.contains(b.as_str()) {
                pairs.push((a, b));
            }
        }
        Ok::<_, rusqlite::Error>(())
    });
    pairs
}
