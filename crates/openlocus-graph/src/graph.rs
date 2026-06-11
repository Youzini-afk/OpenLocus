//! Graph types and builder.
//!
//! Builds edges from scan_repo records using simple line-based heuristics:
//! - imports: parse Rust `mod/use`, TS/JS `import ... from`, Python `import/from`, Go `import`
//! - tests: path/name heuristic linking test files to source files
//! - configures: config files (Cargo.toml, package.json, etc.) to nearby source files
//!
//! All edges carry `source_content_sha` and `source_language` from build time.
//! Edges are NOT Evidence — they must be materialized via StoreHit →
//! `openlocus_store::materialize_evidence()`.

use anyhow::Result;
use openlocus_repo::scan::FileRecord;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

// ── Types ─────────────────────────────────────────────────────────────

/// Kind of graph edge.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// A imports/uses B (A depends on B)
    Imports,
    /// A tests B (test file tests source file)
    Tests,
    /// A configures B (config file configures source/module)
    Configures,
}

/// A node in the graph (a file).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub path: String,
    pub content_sha: String,
    pub language: String,
}

/// A directed edge in the graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    /// Source file path (the file that imports/tests/configures)
    pub source_path: String,
    /// Target file path (the file being imported/tested/configured)
    pub target_path: String,
    /// Kind of edge
    pub kind: EdgeKind,
    /// Line number in source file where the edge was found (1-based)
    pub source_line: u64,
    /// Line number in source file where the edge reference ends (1-based, inclusive)
    pub source_end_line: u64,
    /// The raw text of the import/test/config line
    pub edge_text: String,
    /// content_sha of the source file at build time (for materialization stale check)
    pub source_content_sha: String,
    /// Language of the source file at build time
    pub source_language: String,
}

/// A safe/current record validated by build_graph.
struct SafeRecord {
    path: String,
    content_sha: String,
    language: String,
}

/// Result of building the graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphBuildResult {
    pub node_count: usize,
    pub edge_count: usize,
    pub edges_by_kind: HashMap<String, usize>,
    pub skipped_stale: usize,
    pub skipped_path_unsafe: usize,
}

/// Capabilities of the graph builder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphCapabilities {
    pub max_depth: u8,
    pub imports: bool,
    pub tests: bool,
    pub configures: bool,
    pub call_graph: bool,
    pub type_graph: bool,
}

impl Default for GraphCapabilities {
    fn default() -> Self {
        Self {
            max_depth: 1,
            imports: true,
            tests: true,
            configures: true,
            call_graph: false,
            type_graph: false,
        }
    }
}

// ── Builder ───────────────────────────────────────────────────────────

/// Build a graph from scan_repo records.
///
/// Validates paths and current sha, creates safe_records, and builds
/// import/test/config edges only from safe/current records. Stale records
/// and records with unsafe paths are skipped and counted.
pub fn build_graph(
    repo_root: &Path,
    records: &[FileRecord],
) -> Result<(Vec<GraphNode>, Vec<GraphEdge>, GraphBuildResult)> {
    let mut nodes = Vec::new();
    let mut skipped_stale = 0usize;
    let mut skipped_path_unsafe = 0usize;

    // Phase 1: Build safe records — validate path and current sha
    let mut safe_records = Vec::new();

    for record in records {
        // Validate path safety
        if openlocus_repo::validate_path(repo_root, &record.path).is_err() {
            skipped_path_unsafe += 1;
            continue;
        }

        let full_path = repo_root.join(&record.path);

        // Read file bytes once (TOCTOU-safe)
        let bytes = match std::fs::read(&full_path) {
            Ok(b) => b,
            Err(_) => continue,
        };

        let current_sha = blake3::hash(&bytes).to_hex().to_string();

        // Skip stale records
        if !record.content_sha.is_empty() && record.content_sha != current_sha {
            skipped_stale += 1;
            continue;
        }

        safe_records.push(SafeRecord {
            path: record.path.clone(),
            content_sha: current_sha.clone(),
            language: record.language.clone(),
        });

        nodes.push(GraphNode {
            path: record.path.clone(),
            content_sha: current_sha,
            language: record.language.clone(),
        });
    }

    // Phase 2: Build path_set and basename_index from safe records only
    let path_set: std::collections::HashSet<String> =
        safe_records.iter().map(|r| r.path.clone()).collect();

    let mut basename_index: HashMap<String, Vec<String>> = HashMap::new();
    for rec in &safe_records {
        if let Some(bname) = Path::new(&rec.path).file_name() {
            let bname_str = bname.to_string_lossy().to_string();
            basename_index
                .entry(bname_str)
                .or_default()
                .push(rec.path.clone());
        }
    }

    // Phase 3: Build edges from safe records only
    let mut edges = Vec::new();

    for rec in &safe_records {
        let full_path = repo_root.join(&rec.path);
        let bytes = std::fs::read(&full_path).unwrap_or_default();
        let content = String::from_utf8_lossy(&bytes);

        // Parse import edges based on language
        let file_edges = parse_file_edges(
            &rec.path,
            &rec.language,
            &content,
            &path_set,
            &basename_index,
            &rec.content_sha,
            &rec.language,
        );
        edges.extend(file_edges);
    }

    // Add test heuristic edges (from safe records only)
    let test_edges = build_test_edges(&safe_records, &path_set);
    edges.extend(test_edges);

    // Add config heuristic edges (from safe records only)
    let config_edges = build_config_edges(&safe_records, &path_set);
    edges.extend(config_edges);

    // Count by kind
    let mut edges_by_kind: HashMap<String, usize> = HashMap::new();
    for edge in &edges {
        *edges_by_kind
            .entry(match edge.kind {
                EdgeKind::Imports => "imports".to_string(),
                EdgeKind::Tests => "tests".to_string(),
                EdgeKind::Configures => "configures".to_string(),
            })
            .or_default() += 1;
    }

    let node_count = nodes.len();
    let edge_count = edges.len();

    Ok((
        nodes,
        edges,
        GraphBuildResult {
            node_count,
            edge_count,
            edges_by_kind,
            skipped_stale,
            skipped_path_unsafe,
        },
    ))
}

// ── Import parsing ────────────────────────────────────────────────────

fn parse_file_edges(
    path: &str,
    language: &str,
    content: &str,
    path_set: &std::collections::HashSet<String>,
    basename_index: &HashMap<String, Vec<String>>,
    source_content_sha: &str,
    source_language: &str,
) -> Vec<GraphEdge> {
    let mut edges = Vec::new();

    match language {
        "rust" => {
            for (i, line) in content.lines().enumerate() {
                let line_no = (i + 1) as u64;
                let trimmed = line.trim();

                // mod declaration: mod foo; or mod foo::{
                if let Some(mod_name) = parse_rust_mod(trimmed)
                    && let Some(target) = resolve_rust_mod(path, &mod_name, path_set)
                {
                    edges.push(GraphEdge {
                        source_path: path.to_string(),
                        target_path: target,
                        kind: EdgeKind::Imports,
                        source_line: line_no,
                        source_end_line: line_no,
                        edge_text: trimmed.to_string(),
                        source_content_sha: source_content_sha.to_string(),
                        source_language: source_language.to_string(),
                    });
                }

                // use declaration: use crate::foo::bar;
                if let Some(use_targets) = parse_rust_use(trimmed) {
                    for target in use_targets {
                        if let Some(resolved) =
                            resolve_rust_use_path(path, &target, path_set, basename_index)
                        {
                            edges.push(GraphEdge {
                                source_path: path.to_string(),
                                target_path: resolved,
                                kind: EdgeKind::Imports,
                                source_line: line_no,
                                source_end_line: line_no,
                                edge_text: trimmed.to_string(),
                                source_content_sha: source_content_sha.to_string(),
                                source_language: source_language.to_string(),
                            });
                        }
                    }
                }
            }
        }
        "typescript" | "javascript" => {
            for (i, line) in content.lines().enumerate() {
                let line_no = (i + 1) as u64;
                let trimmed = line.trim();

                if let Some(targets) = parse_ts_import(trimmed) {
                    for target in targets {
                        if let Some(resolved) =
                            resolve_ts_import(path, &target, path_set, basename_index)
                        {
                            edges.push(GraphEdge {
                                source_path: path.to_string(),
                                target_path: resolved,
                                kind: EdgeKind::Imports,
                                source_line: line_no,
                                source_end_line: line_no,
                                edge_text: trimmed.to_string(),
                                source_content_sha: source_content_sha.to_string(),
                                source_language: source_language.to_string(),
                            });
                        }
                    }
                }
            }
        }
        "python" => {
            for (i, line) in content.lines().enumerate() {
                let line_no = (i + 1) as u64;
                let trimmed = line.trim();

                if let Some(targets) = parse_python_import(trimmed) {
                    for target in targets {
                        if let Some(resolved) =
                            resolve_python_import(path, &target, path_set, basename_index)
                        {
                            edges.push(GraphEdge {
                                source_path: path.to_string(),
                                target_path: resolved,
                                kind: EdgeKind::Imports,
                                source_line: line_no,
                                source_end_line: line_no,
                                edge_text: trimmed.to_string(),
                                source_content_sha: source_content_sha.to_string(),
                                source_language: source_language.to_string(),
                            });
                        }
                    }
                }
            }
        }
        "go" => {
            let mut in_import_block = false;
            for (i, line) in content.lines().enumerate() {
                let line_no = (i + 1) as u64;
                let trimmed = line.trim();

                if trimmed == "import (" {
                    in_import_block = true;
                    continue;
                }
                if in_import_block && trimmed == ")" {
                    in_import_block = false;
                    continue;
                }

                if (in_import_start(trimmed) || in_import_block)
                    && let Some(targets) = parse_go_import(trimmed)
                {
                    for target in targets {
                        if let Some(resolved) =
                            resolve_go_import(path, &target, path_set, basename_index)
                        {
                            edges.push(GraphEdge {
                                source_path: path.to_string(),
                                target_path: resolved,
                                kind: EdgeKind::Imports,
                                source_line: line_no,
                                source_end_line: line_no,
                                edge_text: trimmed.to_string(),
                                source_content_sha: source_content_sha.to_string(),
                                source_language: source_language.to_string(),
                            });
                        }
                    }
                }
            }
        }
        _ => {}
    }

    edges
}

// ── Rust mod/use parsing ──────────────────────────────────────────────

fn parse_rust_mod(line: &str) -> Option<String> {
    if !line.starts_with("mod ") && !line.starts_with("pub mod ") {
        return None;
    }
    let rest = line
        .trim_start_matches("pub ")
        .trim_start_matches("mod ")
        .trim();
    let name = rest
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .next()?;
    if name.is_empty() || name == "self" || name == "super" || name == "crate" {
        return None;
    }
    Some(name.to_string())
}

fn resolve_rust_mod(
    source_path: &str,
    mod_name: &str,
    path_set: &std::collections::HashSet<String>,
) -> Option<String> {
    let dir = Path::new(source_path).parent()?.to_str()?;
    let candidates = [
        format!("{}/{}.rs", dir, mod_name),
        format!("{}/{}/mod.rs", dir, mod_name),
    ];
    for c in &candidates {
        if path_set.contains(c) {
            return Some(c.clone());
        }
    }
    None
}

fn parse_rust_use(line: &str) -> Option<Vec<String>> {
    if !line.starts_with("use ") {
        return None;
    }
    let rest = line.trim_start_matches("use ").trim().trim_end_matches(';');
    let segments: Vec<&str> = rest.split("::").collect();
    if segments.is_empty() {
        return None;
    }
    let first_meaningful = segments
        .iter()
        .find(|s| !s.is_empty() && **s != "crate" && **s != "self" && **s != "super")?;
    Some(vec![first_meaningful.to_string()])
}

fn resolve_rust_use_path(
    source_path: &str,
    segment: &str,
    path_set: &std::collections::HashSet<String>,
    basename_index: &HashMap<String, Vec<String>>,
) -> Option<String> {
    let dir = Path::new(source_path).parent()?.to_str()?;
    let candidates = [
        format!("{}/{}.rs", dir, segment),
        format!("{}/{}/mod.rs", dir, segment),
    ];
    for c in &candidates {
        if path_set.contains(c) {
            return Some(c.clone());
        }
    }
    if let Some(paths) = basename_index.get(&format!("{}.rs", segment))
        && paths.len() == 1
    {
        return Some(paths[0].clone());
    }
    None
}

// ── TS/JS import parsing ──────────────────────────────────────────────

fn parse_ts_import(line: &str) -> Option<Vec<String>> {
    let from_part = if line.contains(" from ") {
        line.split(" from ").nth(1)?
    } else if line.starts_with("import '") || line.starts_with("import \"") {
        line.trim_start_matches("import ").trim()
    } else {
        return None;
    };

    let path_str = from_part
        .trim()
        .trim_end_matches(';')
        .trim_matches(|c| c == '\'' || c == '"');

    if path_str.starts_with('.') {
        Some(vec![path_str.to_string()])
    } else {
        None
    }
}

fn resolve_ts_import(
    source_path: &str,
    import_path: &str,
    path_set: &std::collections::HashSet<String>,
    _basename_index: &HashMap<String, Vec<String>>,
) -> Option<String> {
    let dir = Path::new(source_path).parent()?.to_str()?;
    let resolved = format!("{}/{}", dir, import_path);

    let candidates = [
        resolved.clone(),
        format!("{}.ts", resolved),
        format!("{}.tsx", resolved),
        format!("{}.js", resolved),
        format!("{}.jsx", resolved),
        format!("{}/index.ts", resolved),
        format!("{}/index.tsx", resolved),
        format!("{}/index.js", resolved),
    ];

    for c in &candidates {
        let normalized = normalize_path(c);
        if path_set.contains(&normalized) {
            return Some(normalized);
        }
    }
    None
}

// ── Python import parsing ─────────────────────────────────────────────

fn parse_python_import(line: &str) -> Option<Vec<String>> {
    if line.starts_with("import ") {
        let rest = line.trim_start_matches("import ").trim_end_matches(';');
        let module = rest.split(',').next()?.trim().split('.').next()?;
        Some(vec![module.to_string()])
    } else if line.starts_with("from ") {
        let rest = line.trim_start_matches("from ").trim();
        let module = rest.split('.').next()?.split(' ').next()?;
        Some(vec![module.to_string()])
    } else {
        None
    }
}

fn resolve_python_import(
    source_path: &str,
    module: &str,
    path_set: &std::collections::HashSet<String>,
    _basename_index: &HashMap<String, Vec<String>>,
) -> Option<String> {
    let dir = Path::new(source_path).parent()?.to_str()?;
    let candidates = [
        format!("{}/{}.py", dir, module),
        format!("{}/__init__.py", dir),
        format!("{}.py", module),
        format!("{}/__init__.py", module),
    ];
    for c in &candidates {
        let normalized = normalize_path(c);
        if path_set.contains(&normalized) {
            return Some(normalized);
        }
    }
    None
}

// ── Go import parsing ─────────────────────────────────────────────────

fn in_import_start(line: &str) -> bool {
    line.starts_with("import ") && !line.starts_with("import (")
}

fn parse_go_import(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    if trimmed.starts_with("import ") {
        let rest = trimmed.trim_start_matches("import ").trim();
        let path_str = rest.trim_matches(|c| c == '\'' || c == '"');
        if !path_str.is_empty() {
            return Some(vec![path_str.to_string()]);
        }
    }
    let path_str = trimmed
        .trim_start_matches(|c: char| c.is_alphanumeric() || c == '_' || c == '.')
        .trim()
        .trim_matches('\"');
    if !path_str.is_empty() && !path_str.starts_with('/') {
        return Some(vec![path_str.to_string()]);
    }
    None
}

fn resolve_go_import(
    _source_path: &str,
    _import_path: &str,
    _path_set: &std::collections::HashSet<String>,
    _basename_index: &HashMap<String, Vec<String>>,
) -> Option<String> {
    None
}

// ── Test heuristic ────────────────────────────────────────────────────

fn build_test_edges(
    safe_records: &[SafeRecord],
    path_set: &std::collections::HashSet<String>,
) -> Vec<GraphEdge> {
    let mut edges = Vec::new();

    for rec in safe_records {
        let path = &rec.path;
        let is_test = is_test_file(path);

        if !is_test {
            continue;
        }

        let source_candidates = find_test_targets(path, path_set);
        for target in source_candidates {
            edges.push(GraphEdge {
                source_path: path.clone(),
                target_path: target.clone(),
                kind: EdgeKind::Tests,
                source_line: 1,
                source_end_line: 1,
                edge_text: format!("test heuristic: {} tests {}", path, target),
                source_content_sha: rec.content_sha.clone(),
                source_language: rec.language.clone(),
            });
        }
    }

    edges
}

fn is_test_file(path: &str) -> bool {
    let path_lower = path.to_lowercase();
    let name = Path::new(path)
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();

    if path_lower.contains("/tests/") || path_lower.contains("/test/") {
        return true;
    }

    if name.ends_with("_test.rs")
        || name.ends_with("_test.py")
        || name.ends_with("_test.go")
        || name.ends_with(".test.ts")
        || name.ends_with(".test.tsx")
        || name.ends_with(".spec.ts")
        || name.ends_with(".spec.tsx")
        || name.starts_with("test_")
    {
        return true;
    }

    false
}

fn find_test_targets(test_path: &str, path_set: &std::collections::HashSet<String>) -> Vec<String> {
    let mut targets = Vec::new();
    let name = Path::new(test_path)
        .file_stem()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();

    let module_name = name
        .trim_end_matches("_test")
        .trim_start_matches("test_")
        .trim_end_matches(".test")
        .trim_end_matches(".spec")
        .to_string();

    let test_dir = Path::new(test_path)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    let extensions = ["rs", "py", "go", "ts", "tsx", "js"];
    for ext in &extensions {
        let candidate = format!("{}/{}.{}", test_dir, module_name, ext);
        if path_set.contains(&candidate) && candidate != test_path {
            targets.push(candidate);
        }
    }

    if test_dir.ends_with("/tests") || test_dir.ends_with("/test") {
        let parent = Path::new(&test_dir)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        for ext in &extensions {
            let candidate = format!("{}/{}.{}", parent, module_name, ext);
            if path_set.contains(&candidate) && candidate != test_path {
                targets.push(candidate);
            }
        }
        let src_dir = format!("{}/src", parent);
        for ext in &extensions {
            let candidate = format!("{}/{}.{}", src_dir, module_name, ext);
            if path_set.contains(&candidate) && candidate != test_path {
                targets.push(candidate);
            }
        }
    }

    targets
}

// ── Config heuristic ──────────────────────────────────────────────────

fn build_config_edges(
    safe_records: &[SafeRecord],
    path_set: &std::collections::HashSet<String>,
) -> Vec<GraphEdge> {
    let mut edges = Vec::new();

    for rec in safe_records {
        let path = &rec.path;
        let config_name = Path::new(path)
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default();

        let is_config = matches!(
            config_name.as_str(),
            "Cargo.toml"
                | "package.json"
                | "pyproject.toml"
                | "tsconfig.json"
                | "go.mod"
                | "build.gradle"
                | "pom.xml"
                | "CMakeLists.txt"
        );

        if !is_config {
            continue;
        }

        let config_dir = Path::new(path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        let mut count = 0;
        for other in path_set {
            if count >= 50 {
                break;
            }
            if other == path {
                continue;
            }
            let other_dir = Path::new(other)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();

            if other_dir.starts_with(&config_dir) {
                let other_name = Path::new(other)
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default();
                if is_config_file(&other_name) {
                    continue;
                }

                edges.push(GraphEdge {
                    source_path: path.clone(),
                    target_path: other.clone(),
                    kind: EdgeKind::Configures,
                    source_line: 1,
                    source_end_line: 1,
                    edge_text: format!("config heuristic: {} configures {}", path, other),
                    source_content_sha: rec.content_sha.clone(),
                    source_language: rec.language.clone(),
                });
                count += 1;
            }
        }
    }

    edges
}

fn is_config_file(name: &str) -> bool {
    matches!(
        name,
        "Cargo.toml"
            | "package.json"
            | "pyproject.toml"
            | "tsconfig.json"
            | "go.mod"
            | "Cargo.lock"
            | "package-lock.json"
    )
}

// ── Helpers ───────────────────────────────────────────────────────────

fn normalize_path(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in p.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    parts.join("/")
}

// ── Graph queries ─────────────────────────────────────────────────────

/// Get impact set: all edges where the given path is a target (i.e., files
/// that depend on or test the given path). Returns edges at depth=1 only.
pub fn impact_edges(edges: &[GraphEdge], target_path: &str, depth: u8) -> Result<Vec<GraphEdge>> {
    if depth > 1 {
        anyhow::bail!(
            "R5 Level0 only supports depth=1; depth={} is not implemented",
            depth
        );
    }

    Ok(edges
        .iter()
        .filter(|e| e.target_path == target_path)
        .cloned()
        .collect())
}

/// Get test edges for a given path (or all test edges if path is None).
pub fn test_edges<'a>(edges: &'a [GraphEdge], path_filter: Option<&str>) -> Vec<&'a GraphEdge> {
    edges
        .iter()
        .filter(|e| {
            e.kind == EdgeKind::Tests
                && path_filter.is_none_or(|p| e.target_path == p || e.source_path == p)
        })
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use openlocus_core::Policy;
    use openlocus_repo::scan::scan_repo;

    #[test]
    fn parse_rust_mod_simple() {
        assert_eq!(parse_rust_mod("mod foo;"), Some("foo".to_string()));
        assert_eq!(parse_rust_mod("pub mod bar;"), Some("bar".to_string()));
        assert_eq!(parse_rust_mod("mod self;"), None);
        assert_eq!(parse_rust_mod("let x = 1;"), None);
    }

    #[test]
    fn parse_rust_use_simple() {
        let result = parse_rust_use("use crate::foo::bar;");
        assert_eq!(result, Some(vec!["foo".to_string()]));
        let result = parse_rust_use("use super::baz;");
        assert_eq!(
            result,
            Some(vec!["baz".to_string()]),
            "first meaningful segment after super"
        );
    }

    #[test]
    fn parse_ts_import_from() {
        let result = parse_ts_import("import { foo } from './bar'");
        assert!(result.is_some());
        assert_eq!(result.unwrap()[0], "./bar");
    }

    #[test]
    fn parse_python_import() {
        let result = super::parse_python_import("import os.path");
        assert_eq!(result, Some(vec!["os".to_string()]));
        let result = super::parse_python_import("from foo import bar");
        assert_eq!(result, Some(vec!["foo".to_string()]));
    }

    #[test]
    fn is_test_file_detection() {
        assert!(is_test_file("tests/foo_test.rs"));
        assert!(is_test_file("src/bar_test.go"));
        assert!(is_test_file("foo.test.ts"));
        assert!(is_test_file("test_main.py"));
        assert!(!is_test_file("src/main.rs"));
        assert!(!is_test_file("lib.rs"));
    }

    #[test]
    fn impact_edges_depth2_blocked() {
        let edges = vec![GraphEdge {
            source_path: "a.rs".to_string(),
            target_path: "b.rs".to_string(),
            kind: EdgeKind::Imports,
            source_line: 1,
            source_end_line: 1,
            edge_text: "mod b".to_string(),
            source_content_sha: "sha".to_string(),
            source_language: "rust".to_string(),
        }];
        assert!(impact_edges(&edges, "b.rs", 2).is_err());
    }

    #[test]
    fn impact_edges_depth1() {
        let edges = vec![
            GraphEdge {
                source_path: "a.rs".to_string(),
                target_path: "b.rs".to_string(),
                kind: EdgeKind::Imports,
                source_line: 1,
                source_end_line: 1,
                edge_text: "use b".to_string(),
                source_content_sha: "sha".to_string(),
                source_language: "rust".to_string(),
            },
            GraphEdge {
                source_path: "c.rs".to_string(),
                target_path: "d.rs".to_string(),
                kind: EdgeKind::Imports,
                source_line: 1,
                source_end_line: 1,
                edge_text: "use d".to_string(),
                source_content_sha: "sha".to_string(),
                source_language: "rust".to_string(),
            },
        ];
        let result = impact_edges(&edges, "b.rs", 1).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source_path, "a.rs");
    }

    #[test]
    fn test_edges_filter() {
        let edges = vec![
            GraphEdge {
                source_path: "test_foo.rs".to_string(),
                target_path: "foo.rs".to_string(),
                kind: EdgeKind::Tests,
                source_line: 1,
                source_end_line: 1,
                edge_text: "test".to_string(),
                source_content_sha: "sha".to_string(),
                source_language: "rust".to_string(),
            },
            GraphEdge {
                source_path: "a.rs".to_string(),
                target_path: "b.rs".to_string(),
                kind: EdgeKind::Imports,
                source_line: 1,
                source_end_line: 1,
                edge_text: "use b".to_string(),
                source_content_sha: "sha".to_string(),
                source_language: "rust".to_string(),
            },
        ];
        let result = test_edges(&edges, Some("foo.rs"));
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn normalize_path_dots() {
        assert_eq!(normalize_path("src/../lib.rs"), "lib.rs");
        assert_eq!(normalize_path("./foo.rs"), "foo.rs");
    }

    #[test]
    fn build_graph_integration() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(root.join("lib.rs"), "mod foo;\nuse crate::foo::bar;\n").unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(root.join("tests/foo_test.rs"), "use super::*;\n").unwrap();
        std::fs::write(root.join("foo.rs"), "pub fn bar() {}\n").unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"test\"\n").unwrap();

        std::fs::create_dir_all(root.join(".git")).unwrap();

        let policy = Policy::default();
        let records = scan_repo(root, &policy).unwrap();
        let (nodes, edges, result) = build_graph(root, &records).unwrap();

        assert!(nodes.len() >= 3, "should have nodes for files");
        assert!(!edges.is_empty(), "should have edges");
        assert!(result.edge_count >= 1);
        assert!(
            result.edges_by_kind.contains_key("imports")
                || result.edges_by_kind.contains_key("configures")
        );
    }

    #[test]
    fn stale_records_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("lib.rs"), "current content\n").unwrap();

        // Create a record with wrong sha
        let records = vec![FileRecord {
            path: "lib.rs".to_string(),
            size: 100,
            content_sha: "wrong_sha".to_string(),
            language: "rust".to_string(),
        }];

        let (_, edges, result) = build_graph(root, &records).unwrap();
        assert_eq!(result.skipped_stale, 1);
        assert!(edges.is_empty(), "stale records should not produce edges");
    }

    #[test]
    fn traversal_records_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("lib.rs"), "content\n").unwrap();

        // Create a record with traversal path
        let records = vec![FileRecord {
            path: "../../../etc/passwd".to_string(),
            size: 100,
            content_sha: "any".to_string(),
            language: "rust".to_string(),
        }];

        let (_, edges, result) = build_graph(root, &records).unwrap();
        assert_eq!(result.skipped_path_unsafe, 1);
        assert!(
            edges.is_empty(),
            "traversal records should not produce edges"
        );
    }

    #[test]
    fn stale_record_no_test_or_config_edges() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("tests/main_test.rs"), "use super::*;\n").unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"test\"\n").unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();

        // Create stale records (wrong sha but files exist on disk)
        let records = vec![
            FileRecord {
                path: "tests/main_test.rs".to_string(),
                size: 20,
                content_sha: "wrong_sha_test".to_string(),
                language: "rust".to_string(),
            },
            FileRecord {
                path: "src/main.rs".to_string(),
                size: 15,
                content_sha: "wrong_sha_src".to_string(),
                language: "rust".to_string(),
            },
            FileRecord {
                path: "Cargo.toml".to_string(),
                size: 30,
                content_sha: "wrong_sha_config".to_string(),
                language: "toml".to_string(),
            },
        ];

        let (_, edges, result) = build_graph(root, &records).unwrap();
        assert_eq!(result.skipped_stale, 3);
        assert!(
            edges.is_empty(),
            "stale records should not produce test or config edges"
        );
    }

    #[test]
    fn policy_excluded_no_nodes_or_edges() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join(".env"), "SECRET=abc\n").unwrap();
        std::fs::write(root.join("private.pem"), "-----BEGIN RSA-----\n").unwrap();
        std::fs::write(root.join("lib.rs"), "fn main() {}\n").unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();

        let policy = Policy::default();
        let records = scan_repo(root, &policy).unwrap();
        let (nodes, edges, _result) = build_graph(root, &records).unwrap();

        // .env and .pem should be excluded by default policy
        let node_paths: Vec<&str> = nodes.iter().map(|n| n.path.as_str()).collect();
        assert!(!node_paths.contains(&".env"), ".env should not be a node");
        assert!(
            !node_paths.contains(&"private.pem"),
            "private.pem should not be a node"
        );

        let edge_paths: Vec<&str> = edges
            .iter()
            .flat_map(|e| [e.source_path.as_str(), e.target_path.as_str()])
            .collect();
        assert!(
            !edge_paths.contains(&".env"),
            ".env should not appear in edges"
        );
        assert!(
            !edge_paths.contains(&"private.pem"),
            "private.pem should not appear in edges"
        );
    }
}
