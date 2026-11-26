//! Import-graph-aware chunk planning for large pull requests.
//!
//! Partitions changed files into connected components along the indexer's
//! import graph so tightly coupled files (definitions, callers, and tests)
//! stay together in the same review chunk.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewChunk {
    pub id: usize,
    pub files: Vec<String>,
}

/// Computes the effective chunk size for a provider (smaller chunks for slower OpenCode models).
pub fn effective_chunk_size(configured: u32, is_opencode: bool) -> usize {
    let base = if configured == 0 { 8 } else { configured.clamp(2, 20) as usize };
    if is_opencode { base.min(3) } else { base }
}

/// Decides whether to split a review into parallel chunks.
pub fn should_chunk(file_count: usize, chunk_size: usize, depth_mode: &str, is_opencode: bool) -> bool {
    if file_count <= chunk_size {
        return false;
    }
    match depth_mode {
        "economic" => false,
        "deep" => true,
        "auto" if is_opencode => true,
        _ => file_count > chunk_size,
    }
}

/// Partitions `reviewable_files` into chunks of at most `max_chunk_size`,
/// grouping graph-connected files together first.
pub fn plan_chunks(reviewable_files: &[String], edges: &[(String, String)], max_chunk_size: usize) -> Vec<ReviewChunk> {
    if reviewable_files.is_empty() {
        return Vec::new();
    }
    let cap = max_chunk_size.max(1);
    if reviewable_files.len() <= cap {
        return vec![ReviewChunk { id: 1, files: reviewable_files.to_vec() }];
    }

    // Build undirected adjacency among `reviewable_files`.
    let mut adj: HashMap<&str, BTreeSet<&str>> = HashMap::new();
    for f in reviewable_files {
        adj.entry(f.as_str()).or_default();
    }
    for (a, b) in edges {
        if adj.contains_key(a.as_str()) && adj.contains_key(b.as_str()) {
            adj.entry(a.as_str()).or_default().insert(b.as_str());
            adj.entry(b.as_str()).or_default().insert(a.as_str());
        }
    }

    // Extract connected components via BFS, preserving deterministic order.
    let mut visited: BTreeSet<&str> = BTreeSet::new();
    let mut components: Vec<Vec<String>> = Vec::new();
    for f in reviewable_files {
        if !visited.insert(f.as_str()) {
            continue;
        }
        let mut comp = Vec::new();
        let mut q = VecDeque::new();
        q.push_back(f.as_str());
        while let Some(cur) = q.pop_front() {
            comp.push(cur.to_owned());
            if let Some(neighbors) = adj.get(cur) {
                for next in neighbors {
                    if visited.insert(next) {
                        q.push_back(next);
                    }
                }
            }
        }
        components.push(comp);
    }

    // Split oversized components by directory prefix, then bin-pack smaller components into chunks <= cap.
    let mut blocks: Vec<Vec<String>> = Vec::new();
    for comp in components {
        if comp.len() <= cap {
            blocks.push(comp);
        } else {
            let mut by_dir: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for path in comp {
                let dir = path.rsplit_once('/').map_or("", |(d, _)| d).to_owned();
                by_dir.entry(dir).or_default().push(path);
            }
            for (_, group) in by_dir {
                for slice in group.chunks(cap) {
                    blocks.push(slice.to_vec());
                }
            }
        }
    }

    // Pack blocks greedily into chunks without exceeding `cap`.
    blocks.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    let mut chunks: Vec<Vec<String>> = Vec::new();
    for block in blocks {
        if let Some(slot) = chunks.iter_mut().find(|c| c.len() + block.len() <= cap) {
            slot.extend(block);
        } else {
            chunks.push(block);
        }
    }

    chunks.into_iter().enumerate().map(|(i, files)| ReviewChunk { id: i + 1, files }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_connected_files_in_same_chunk() {
        let files: Vec<String> = ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs"].into_iter().map(str::to_owned).collect();
        let edges = vec![("a.rs".into(), "b.rs".into()), ("b.rs".into(), "c.rs".into())];
        let chunks = plan_chunks(&files, &edges, 3);
        assert_eq!(chunks.len(), 2);
        assert!(chunks.iter().any(|c| c.files.contains(&"a.rs".into()) && c.files.contains(&"b.rs".into()) && c.files.contains(&"c.rs".into())));
    }
}
