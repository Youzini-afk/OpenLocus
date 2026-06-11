//! Materialize graph edges into citation-valid Evidence.
//!
//! Graph edges are NOT Evidence. They must be converted to StoreHit and
//! materialized via `openlocus_store::materialize_evidence()`, which reads
//! the current filesystem, computes content_sha, validates ranges, and builds
//! excerpts. Invalid ranges are rejected (not clamped).

use crate::graph::{EdgeKind, GraphEdge};
use anyhow::Result;
use openlocus_core::{Channel, Evidence, ScoreParts};
use openlocus_store::{StoreHit, StoreSource, materialize_evidence};
use std::path::Path;

/// Convert a GraphEdge to a StoreHit for materialization.
fn edge_to_store_hit(edge: &GraphEdge) -> StoreHit {
    StoreHit {
        path: edge.source_path.clone(),
        start_line: edge.source_line,
        end_line: edge.source_end_line,
        content_sha: edge.source_content_sha.clone(),
        score: 0.5,
        source: StoreSource::External("graph".to_string()),
        language: edge.source_language.clone(),
        symbol_name: None,
    }
}

/// Materialize a graph edge into a citation-valid Evidence.
///
/// Converts the edge to a StoreHit and delegates to
/// `openlocus_store::materialize_evidence()`. If materialization succeeds,
/// adds graph-specific why/score_parts without changing span/hash.
/// Invalid ranges are rejected (not clamped) by the store materializer.
pub fn materialize_graph_evidence(repo_root: &Path, edge: &GraphEdge) -> Result<Evidence> {
    let hit = edge_to_store_hit(edge);
    let channel = Channel::Graph;

    let mut evidence = materialize_evidence(repo_root, &hit, channel)
        .map_err(|e| anyhow::anyhow!("materialization failed for {}: {}", edge.source_path, e))?;

    // Add graph-specific why and score_parts without changing span/hash
    let graph_why = format!(
        "graph_{}: {} -> {} (line {})",
        match edge.kind {
            EdgeKind::Imports => "imports",
            EdgeKind::Tests => "tests",
            EdgeKind::Configures => "configures",
        },
        edge.source_path,
        edge.target_path,
        edge.source_line,
    );
    evidence.core.why.push(graph_why);

    if let Some(ref mut meta) = evidence.meta {
        if let Some(ref mut sp) = meta.score_parts {
            sp.graph = Some(0.5);
        } else {
            meta.score_parts = Some(ScoreParts {
                graph: Some(0.5),
                ..Default::default()
            });
        }
    }

    Ok(evidence)
}

/// Materialize multiple graph edges into Evidence items.
/// Skips edges that fail materialization (stale/invalid), returns the rest.
pub fn materialize_graph_edges(repo_root: &Path, edges: &[GraphEdge]) -> (Vec<Evidence>, usize) {
    let mut evidence = Vec::new();
    let mut skipped = 0;

    for edge in edges {
        match materialize_graph_evidence(repo_root, edge) {
            Ok(ev) => evidence.push(ev),
            Err(_) => skipped += 1,
        }
    }

    (evidence, skipped)
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::GraphEdge;

    #[allow(dead_code)]
    fn make_edge(path: &str, line: u64) -> GraphEdge {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join(path), "line1\nline2\nline3\n").unwrap();
        let bytes = std::fs::read(root.join(path)).unwrap();
        let sha = blake3::hash(&bytes).to_hex().to_string();

        GraphEdge {
            source_path: path.to_string(),
            target_path: "other.rs".to_string(),
            kind: EdgeKind::Imports,
            source_line: line,
            source_end_line: line,
            edge_text: "use other".to_string(),
            source_content_sha: sha,
            source_language: "rust".to_string(),
        }
    }

    #[test]
    fn materialize_valid_edge() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("lib.rs"), "fn foo() {}\nfn bar() {}\n").unwrap();
        let bytes = std::fs::read(root.join("lib.rs")).unwrap();
        let sha = blake3::hash(&bytes).to_hex().to_string();

        let edge = GraphEdge {
            source_path: "lib.rs".to_string(),
            target_path: "other.rs".to_string(),
            kind: EdgeKind::Imports,
            source_line: 1,
            source_end_line: 1,
            edge_text: "use other".to_string(),
            source_content_sha: sha,
            source_language: "rust".to_string(),
        };

        let result = materialize_graph_evidence(root, &edge).unwrap();
        assert_eq!(result.core.path, "lib.rs");
        assert_eq!(result.core.start_line, 1);
        assert_eq!(result.core.end_line, 1);
        assert_eq!(
            result.meta.as_ref().unwrap().freshness,
            Some(openlocus_core::Freshness::VerifiedCurrent)
        );
        assert!(!result.core.content_sha.is_empty());
        // Verify graph why was added
        assert!(
            result
                .core
                .why
                .iter()
                .any(|w| w.starts_with("graph_imports:"))
        );
        // Verify graph score_parts
        assert_eq!(
            result
                .meta
                .as_ref()
                .unwrap()
                .score_parts
                .as_ref()
                .unwrap()
                .graph,
            Some(0.5)
        );
    }

    #[test]
    fn materialize_stale_edge_fails() {
        let dir = tempfile::tempdir().unwrap();
        let edge = GraphEdge {
            source_path: "nonexistent.rs".to_string(),
            target_path: "other.rs".to_string(),
            kind: EdgeKind::Imports,
            source_line: 1,
            source_end_line: 1,
            edge_text: "use other".to_string(),
            source_content_sha: "sha".to_string(),
            source_language: "rust".to_string(),
        };

        assert!(materialize_graph_evidence(dir.path(), &edge).is_err());
    }

    #[test]
    fn materialize_invalid_range_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("lib.rs"), "fn foo() {}\n").unwrap();
        let bytes = std::fs::read(root.join("lib.rs")).unwrap();
        let sha = blake3::hash(&bytes).to_hex().to_string();

        // Range exceeds file lines
        let edge = GraphEdge {
            source_path: "lib.rs".to_string(),
            target_path: "other.rs".to_string(),
            kind: EdgeKind::Imports,
            source_line: 1,
            source_end_line: 99,
            edge_text: "use other".to_string(),
            source_content_sha: sha,
            source_language: "rust".to_string(),
        };

        // Should be rejected (not clamped) by materialize_evidence
        assert!(materialize_graph_evidence(root, &edge).is_err());
    }

    #[test]
    fn materialize_batch_skips_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("lib.rs"), "fn foo() {}\n").unwrap();
        let bytes = std::fs::read(root.join("lib.rs")).unwrap();
        let sha = blake3::hash(&bytes).to_hex().to_string();

        let valid_edge = GraphEdge {
            source_path: "lib.rs".to_string(),
            target_path: "other.rs".to_string(),
            kind: EdgeKind::Imports,
            source_line: 1,
            source_end_line: 1,
            edge_text: "use other".to_string(),
            source_content_sha: sha,
            source_language: "rust".to_string(),
        };

        let invalid_edge = GraphEdge {
            source_path: "nonexistent.rs".to_string(),
            target_path: "other.rs".to_string(),
            kind: EdgeKind::Imports,
            source_line: 1,
            source_end_line: 1,
            edge_text: "use other".to_string(),
            source_content_sha: "sha".to_string(),
            source_language: "rust".to_string(),
        };

        let (evidence, skipped) = materialize_graph_edges(root, &[valid_edge, invalid_edge]);
        assert_eq!(evidence.len(), 1);
        assert_eq!(skipped, 1);
    }
}
