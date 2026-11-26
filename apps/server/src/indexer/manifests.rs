//! Package manifest discovery and file-to-package ownership mapping.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

#[derive(Clone, Debug)]
pub struct PackageInfo {
    pub root: String,
    pub manifest_path: String,
    pub deps: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct PackageMap {
    /// Sorted by descending path depth so longest-prefix matching wins.
    packages: Vec<PackageInfo>,
    by_name: HashMap<String, String>,
}

impl PackageMap {
    pub fn package_count(&self) -> usize {
        self.packages.len()
    }

    pub fn owner_of(&self, path: &str) -> Option<&str> {
        for pkg in &self.packages {
            if pkg.root.is_empty() {
                return Some("");
            }
            if path == pkg.root || path.starts_with(&format!("{}/", pkg.root)) {
                return Some(pkg.root.as_str());
            }
        }
        None
    }

    /// Emits `(dependent_manifest, dependency_manifest)` edges between workspace packages.
    pub fn package_dep_edges(&self) -> Vec<(String, String)> {
        let mut edges = Vec::new();
        for pkg in &self.packages {
            for dep in &pkg.deps {
                if let Some(target_manifest) = self.by_name.get(dep)
                    && target_manifest != &pkg.manifest_path
                {
                    edges.push((pkg.manifest_path.clone(), target_manifest.clone()));
                }
            }
        }
        edges
    }
}

pub fn is_manifest(path: &str) -> bool {
    let name = Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or("");
    matches!(name, "Cargo.toml" | "package.json" | "go.mod" | "pyproject.toml" | "build.gradle.kts" | "Package.swift")
}

pub fn build_package_map(manifests: &BTreeMap<String, String>, _all_paths: &BTreeSet<String>) -> PackageMap {
    let mut packages = Vec::new();
    let mut by_name = HashMap::new();
    for (path, content) in manifests {
        let root = Path::new(path).parent().and_then(|p| p.to_str()).unwrap_or("").to_owned();
        let name = Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or("");
        let (pkg_name, deps) = match name {
            "Cargo.toml" => parse_cargo_toml(content, &root),
            "package.json" => parse_package_json(content, &root),
            "go.mod" => parse_go_mod(content, &root),
            "pyproject.toml" => parse_pyproject_toml(content, &root),
            "Package.swift" => parse_swift_package(content, &root),
            "build.gradle.kts" => parse_gradle_kts(content, &root),
            _ => continue,
        };
        if !pkg_name.is_empty() {
            by_name.entry(pkg_name.clone()).or_insert_with(|| path.clone());
        }
        packages.push(PackageInfo { root, manifest_path: path.clone(), deps });
    }
    packages.sort_by(|a, b| b.root.len().cmp(&a.root.len()).then_with(|| a.root.cmp(&b.root)));
    PackageMap { packages, by_name }
}

fn fallback_name(root: &str) -> String {
    if root.is_empty() { "root".into() } else { root.rsplit('/').next().unwrap_or(root).to_owned() }
}

fn parse_cargo_toml(content: &str, root: &str) -> (String, Vec<String>) {
    let mut section = String::new();
    let mut name = String::new();
    let mut deps = Vec::new();
    for raw in content.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line.trim_matches(['[', ']']).trim().to_owned();
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let key = k.trim();
            let val = v.trim();
            if section == "package" && key == "name" {
                name = val.trim_matches('"').to_owned();
            } else if section.ends_with("dependencies") {
                deps.push(key.trim_matches('"').to_owned());
            }
        }
    }
    if name.is_empty() {
        name = fallback_name(root);
    }
    (name, deps)
}

fn parse_package_json(content: &str, root: &str) -> (String, Vec<String>) {
    #[derive(serde::Deserialize, Default)]
    struct Pkg {
        #[serde(default)]
        name: String,
        #[serde(default)]
        dependencies: BTreeMap<String, String>,
        #[serde(default, rename = "devDependencies")]
        dev_dependencies: BTreeMap<String, String>,
        #[serde(default, rename = "peerDependencies")]
        peer_dependencies: BTreeMap<String, String>,
    }
    let parsed: Pkg = serde_json::from_str(content).unwrap_or_default();
    let name = if parsed.name.is_empty() { fallback_name(root) } else { parsed.name };
    let deps = parsed.dependencies.into_keys().chain(parsed.dev_dependencies.into_keys()).chain(parsed.peer_dependencies.into_keys()).collect();
    (name, deps)
}

fn parse_go_mod(content: &str, root: &str) -> (String, Vec<String>) {
    let mut name = String::new();
    let mut deps = Vec::new();
    for raw in content.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("module ") {
            name = rest.trim().to_owned();
        } else if let Some(rest) = line.strip_prefix("require ") {
            if let Some(dep) = rest.split_whitespace().next() {
                deps.push(dep.to_owned());
            }
        }
    }
    if name.is_empty() {
        name = fallback_name(root);
    }
    (name, deps)
}

fn parse_pyproject_toml(content: &str, root: &str) -> (String, Vec<String>) {
    let mut section = String::new();
    let mut name = String::new();
    for raw in content.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') && line.ends_with(']') {
            section = line.trim_matches(['[', ']']).trim().to_owned();
        } else if section == "project"
            && let Some((k, v)) = line.split_once('=')
            && k.trim() == "name"
        {
            name = v.trim().trim_matches(['"', '\'']).to_owned();
        }
    }
    if name.is_empty() {
        name = fallback_name(root);
    }
    (name, Vec::new())
}

fn parse_swift_package(content: &str, root: &str) -> (String, Vec<String>) {
    let name = content
        .lines()
        .find_map(|l| l.split("name:").nth(1)?.trim().strip_prefix('"')?.split('"').next().map(str::to_owned))
        .unwrap_or_else(|| fallback_name(root));
    (name, Vec::new())
}

fn parse_gradle_kts(content: &str, root: &str) -> (String, Vec<String>) {
    let mut deps = Vec::new();
    for line in content.lines() {
        if let Some(rest) = line.split("project(\":").nth(1)
            && let Some(dep) = rest.split('"').next()
        {
            deps.push(dep.to_owned());
        }
    }
    (fallback_name(root), deps)
}
