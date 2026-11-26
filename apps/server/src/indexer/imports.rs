//! Lightweight import graph extraction and test-to-source edge inference.

use std::collections::{BTreeSet, HashSet};
use std::path::{Component, Path, PathBuf};

/// Extracts repo-relative import targets from `content` for `source_path`.
pub fn extract_imports(source_path: &str, language: &str, content: &str, all_paths: &BTreeSet<String>) -> Vec<String> {
    let mut out = HashSet::new();
    match language {
        "rust" => extract_rust(source_path, content, all_paths, &mut out),
        "typescript" | "typescriptreact" | "javascript" | "javascriptreact" => extract_js_ts(source_path, content, all_paths, &mut out),
        "python" => extract_python(source_path, content, all_paths, &mut out),
        "ruby" => extract_ruby(source_path, content, all_paths, &mut out),
        "go" | "swift" | "kotlin" => extract_package_local(source_path, content, all_paths, &mut out),
        _ => {}
    }
    let mut sorted: Vec<String> = out.into_iter().collect();
    sorted.sort();
    sorted
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

fn join_norm(base_dir: &str, rel: &str) -> String {
    let mut joined = if base_dir.is_empty() { PathBuf::from(rel) } else { Path::new(base_dir).join(rel) };
    let mut parts: Vec<String> = Vec::new();
    for comp in joined.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(os) => parts.push(os.to_string_lossy().into_owned()),
            _ => {}
        }
    }
    joined.clear();
    parts.join("/")
}

fn extract_rust(source_path: &str, content: &str, all_paths: &BTreeSet<String>, out: &mut HashSet<String>) {
    let dir = parent_dir(source_path);
    // Find nearest `src/` root for `crate::` imports.
    let src_root = source_path.find("/src/").map(|i| &source_path[..i + 4]).or_else(|| source_path.starts_with("src/").then_some("src"));

    for raw in content.lines() {
        let line = raw.trim();
        if line.starts_with("//") {
            continue;
        }
        // `mod foo;` or `pub mod foo;`
        if let Some(rest) = line.strip_prefix("pub(crate) mod ").or_else(|| line.strip_prefix("pub mod ")).or_else(|| line.strip_prefix("mod "))
            && let Some(name) = rest.strip_suffix(';').map(str::trim)
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            for cand in [join_norm(dir, &format!("{name}.rs")), join_norm(dir, &format!("{name}/mod.rs"))] {
                if all_paths.contains(&cand) {
                    out.insert(cand);
                }
            }
        }
        // `use crate::foo::bar`
        if let Some(idx) = line.find("crate::")
            && let Some(root) = src_root
        {
            let rest = &line[idx + "crate::".len()..];
            let segs: Vec<&str> = rest
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
                .next()
                .unwrap_or("")
                .split("::")
                .filter(|s| !s.is_empty())
                .collect();
            for depth in (1..=segs.len().min(4)).rev() {
                let prefix = segs[..depth].join("/");
                for cand in [format!("{root}/{prefix}.rs"), format!("{root}/{prefix}/mod.rs")] {
                    if all_paths.contains(&cand) {
                        out.insert(cand);
                        break;
                    }
                }
            }
        }
        // `use super::foo`
        if let Some(idx) = line.find("super::") {
            let rest = &line[idx + "super::".len()..];
            if let Some(first) = rest.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).next()
                && !first.is_empty()
            {
                let up = parent_dir(dir);
                for cand in [
                    join_norm(dir, &format!("{first}.rs")),
                    join_norm(up, &format!("{first}.rs")),
                    join_norm(up, &format!("{first}/mod.rs")),
                ] {
                    if all_paths.contains(&cand) {
                        out.insert(cand);
                    }
                }
            }
        }
    }
}

fn extract_js_ts(source_path: &str, content: &str, all_paths: &BTreeSet<String>, out: &mut HashSet<String>) {
    let dir = parent_dir(source_path);
    const EXTS: [&str; 9] = ["", ".ts", ".tsx", ".js", ".jsx", "/index.ts", "/index.tsx", "/index.js", "/index.jsx"];
    for raw in content.lines() {
        let line = raw.trim();
        if line.starts_with("//") {
            continue;
        }
        for marker in [" from '", " from \"", "import '", "import \"", "require('", "require(\""] {
            if let Some(rest) = line.split(marker).nth(1) {
                let quote = if marker.ends_with('\'') { '\'' } else { '"' };
                if let Some(spec) = rest.split(quote).next()
                    && (spec.starts_with("./") || spec.starts_with("../"))
                {
                    let base = join_norm(dir, spec);
                    let stem = base.strip_suffix(".js").or_else(|| base.strip_suffix(".mjs")).unwrap_or(&base);
                    for ext in EXTS {
                        let cand = format!("{stem}{ext}");
                        if all_paths.contains(&cand) {
                            out.insert(cand);
                            break;
                        }
                    }
                }
            }
        }
    }
}

fn extract_python(source_path: &str, content: &str, all_paths: &BTreeSet<String>, out: &mut HashSet<String>) {
    let dir = parent_dir(source_path);
    for raw in content.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("from ")
            && let Some(module) = rest.split_whitespace().next()
        {
            if let Some(rel) = module.strip_prefix('.') {
                let dots = module.chars().take_while(|c| *c == '.').count();
                let mut base = dir;
                for _ in 1..dots {
                    base = parent_dir(base);
                }
                let sub = rel.trim_start_matches('.').replace('.', "/");
                for cand in [join_norm(base, &format!("{sub}.py")), join_norm(base, &format!("{sub}/__init__.py"))] {
                    if all_paths.contains(&cand) {
                        out.insert(cand);
                    }
                }
            } else {
                let sub = module.replace('.', "/");
                for cand in [format!("{sub}.py"), format!("{sub}/__init__.py")] {
                    if all_paths.contains(&cand) {
                        out.insert(cand);
                    }
                }
            }
        }
    }
}

fn extract_ruby(source_path: &str, content: &str, all_paths: &BTreeSet<String>, out: &mut HashSet<String>) {
    let dir = parent_dir(source_path);
    for raw in content.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("require_relative ") {
            let spec = rest.trim().trim_matches(['"', '\'', '(', ')']);
            let owned = if spec.ends_with(".rb") { spec.to_owned() } else { format!("{spec}.rb") };
            let cand = join_norm(dir, &owned);
            if all_paths.contains(&cand) {
                out.insert(cand);
            }
        }
    }
}

fn extract_package_local(source_path: &str, content: &str, all_paths: &BTreeSet<String>, out: &mut HashSet<String>) {
    // Match explicit `import foo.bar.Type` to `<Type>.kt` / `<Type>.swift` in the same repo.
    let _ = source_path;
    for raw in content.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("import ") {
            let ident = rest.trim().trim_end_matches(';').rsplit('.').next().unwrap_or("");
            if ident.len() >= 3 && ident.chars().next().is_some_and(char::is_uppercase) {
                let kt = format!("/{ident}.kt");
                let swift = format!("/{ident}.swift");
                for path in all_paths {
                    if path.ends_with(&kt) || path.ends_with(&swift) {
                        out.insert(path.clone());
                    }
                }
            }
        }
    }
}

/// Infers `(test_path, source_path)` pairs based on standard naming conventions.
pub fn infer_test_edges(all_paths: &BTreeSet<String>) -> Vec<(String, String)> {
    let mut edges = Vec::new();
    for path in all_paths {
        if !super::is_test_path(path) {
            continue;
        }
        let dir = parent_dir(path);
        let Some(file) = Path::new(path).file_name().and_then(|n| n.to_str()) else { continue };
        let mut stems = Vec::new();
        for suffix in ["_tests.rs", "_test.rs"] {
            if let Some(stem) = file.strip_suffix(suffix) {
                stems.push(format!("{stem}.rs"));
            }
        }
        for suffix in [".test.ts", ".spec.ts"] {
            if let Some(stem) = file.strip_suffix(suffix) {
                stems.push(format!("{stem}.ts"));
            }
        }
        for suffix in [".test.tsx", ".spec.tsx"] {
            if let Some(stem) = file.strip_suffix(suffix) {
                stems.push(format!("{stem}.tsx"));
            }
        }
        if let Some(stem) = file.strip_suffix("_test.go") {
            stems.push(format!("{stem}.go"));
        }
        if let Some(stem) = file.strip_suffix("Tests.swift") {
            stems.push(format!("{stem}.swift"));
        }
        if let Some(stem) = file.strip_suffix("Test.kt") {
            stems.push(format!("{stem}.kt"));
        }
        for target_name in stems {
            let sibling = join_norm(dir, &target_name);
            if all_paths.contains(&sibling) && sibling != *path {
                edges.push((path.clone(), sibling));
                continue;
            }
            let suffix = format!("/{target_name}");
            if let Some(found) = all_paths.iter().find(|cand| cand.ends_with(&suffix) && !super::is_test_path(cand)) {
                edges.push((path.clone(), found.clone()));
            }
        }
    }
    edges
}
