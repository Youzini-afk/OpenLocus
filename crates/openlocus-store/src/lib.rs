//! OpenLocus Store — storage backend traits, types, and materialization.
//!
//! Core design constraint: Store backends never directly output authoritative
//! Evidence. They produce `StoreHit` records which must be materialized through
//! `materialize_evidence()` — this reads the current filesystem, computes
//! content_sha, validates range, and builds excerpt. This ensures citation
//! validity regardless of store backend staleness.
//!
//! All ingest comes from `scan_repo` filtered records; stores never walk the
//! filesystem themselves.

pub mod conservative;
#[cfg(feature = "tdb")]
pub mod tdb_adapter;
pub mod tdb_placeholder;

use openlocus_core::{Channel, Evidence, Freshness, ScoreParts};
use openlocus_repo::scan::FileRecord;
use openlocus_repo::validate_path;
use serde::{Deserialize, Serialize};
use std::path::Path;

// ── Store types ───────────────────────────────────────────────────────

/// Unique identifier for a store snapshot (index build).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotId(pub String);

/// Kind of chunk stored in a chunk store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkKind {
    /// Full file content
    File,
    /// Bounded line range within a file
    LineRange,
    /// Symbol definition span
    SymbolDef,
    /// Paragraph / prose block
    Paragraph,
    /// Other / unknown
    Other,
}

/// Key for a chunk within a store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkKey {
    pub path: String,
    pub start_line: u64,
    pub end_line: u64,
}

/// A stored chunk record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkRecord {
    pub key: ChunkKey,
    pub kind: ChunkKind,
    pub content_sha: String,
    pub language: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_name: Option<String>,
}

/// Source of a store hit (which backend produced it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreSource {
    Conservative,
    Tdb,
    External(String),
}

/// A hit from a store backend. Must be materialized to become Evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreHit {
    pub path: String,
    pub start_line: u64,
    pub end_line: u64,
    /// content_sha at index time (may be stale)
    pub content_sha: String,
    pub score: f64,
    pub source: StoreSource,
    pub language: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_name: Option<String>,
}

/// Debug information about a store backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreDebug {
    pub backend_name: String,
    pub snapshot_id: Option<String>,
    pub chunk_count: usize,
    pub file_count: usize,
}

// ── Capabilities & Health ─────────────────────────────────────────────

/// Capabilities a store backend advertises.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreCapabilities {
    pub metadata: bool,
    pub chunks: bool,
    pub lexical: bool,
    pub vector: bool,
    pub graph: bool,
}

impl StoreCapabilities {
    pub fn none() -> Self {
        Self {
            metadata: false,
            chunks: false,
            lexical: false,
            vector: false,
            graph: false,
        }
    }
}

/// Health status of a store backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreHealth {
    pub available: bool,
    pub backend: String,
    pub capabilities: StoreCapabilities,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ── Store error ───────────────────────────────────────────────────────

/// Store-specific errors.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("store not available: {0}")]
    NotAvailable(String),
    #[error("unsupported operation: {0}")]
    Unsupported(String),
    #[error("stale hit: content_sha mismatch for {path}")]
    StaleHit { path: String },
    #[error("invalid range: {0}")]
    InvalidRange(String),
    #[error("store error: {0}")]
    Other(#[from] anyhow::Error),
}

pub type StoreResult<T> = std::result::Result<T, StoreError>;

// ── Store traits ──────────────────────────────────────────────────────

/// Base trait for all store backends.
pub trait StoreBackend: Send + Sync {
    /// Name of this backend (e.g. "conservative", "tdb").
    fn name(&self) -> &str;

    /// Current health status.
    fn health(&self) -> StoreHealth;

    /// Build the store from filtered file records.
    fn build(&mut self, repo_root: &Path, records: &[FileRecord]) -> StoreResult<StoreDebug>;

    /// Purge all stored data.
    fn purge(&mut self) -> StoreResult<()>;
}

/// Chunk-level storage (metadata + chunks).
pub trait ChunkStore: StoreBackend {
    /// Return all chunk records.
    fn chunks(&self) -> Vec<ChunkRecord>;

    /// Return chunk count.
    fn chunk_count(&self) -> usize;

    /// Return file count (unique paths).
    fn file_count(&self) -> usize;
}

/// Lexical search (BM25-style) over stored chunks.
pub trait LexicalStore: ChunkStore {
    /// Search with a query, returning ranked hits.
    fn search(&self, query: &str, max_results: usize) -> StoreResult<Vec<StoreHit>>;
}

/// Vector/dense search (placeholder).
pub trait VectorStore: StoreBackend {
    /// Search with a dense query vector.
    fn search_dense(&self, _query: &[f32], _max_results: usize) -> StoreResult<Vec<StoreHit>> {
        Err(StoreError::Unsupported(
            "vector search not implemented".into(),
        ))
    }
}

/// Graph search (placeholder).
pub trait GraphStore: StoreBackend {
    /// Search graph neighbors.
    fn search_neighbors(&self, _node_id: &str, _depth: usize) -> StoreResult<Vec<StoreHit>> {
        Err(StoreError::Unsupported(
            "graph search not implemented".into(),
        ))
    }
}

// ── Materialization ───────────────────────────────────────────────────

/// Materialize a StoreHit into a citation-valid Evidence by reading the
/// current filesystem. This is the critical gate: store backends never
/// produce authoritative Evidence directly.
///
/// Steps:
/// 0. Reject empty content_sha (cannot verify VerifiedCurrent without index-time hash).
/// 1. Validate path via repo validate_path (symlink/escape protection).
/// 2. Read file bytes once; compute content_sha from same bytes (TOCTOU-safe).
/// 3. Compare hit.content_sha with current; reject if stale.
/// 4. Decode same bytes for line content; validate range: 1 ≤ start ≤ end ≤ total_lines.
/// 5. Build excerpt from decoded content.
/// 6. Return Evidence with freshness=VerifiedCurrent.
pub fn materialize_evidence(
    repo_root: &Path,
    hit: &StoreHit,
    channel: Channel,
) -> StoreResult<Evidence> {
    // 0. Reject empty content_sha — cannot verify VerifiedCurrent without index-time hash
    if hit.content_sha.is_empty() {
        return Err(StoreError::Other(anyhow::anyhow!(
            "StoreHit.content_sha is empty: cannot materialize VerifiedCurrent evidence without index-time hash for {}",
            hit.path
        )));
    }

    // 1. Validate path (symlink/escape protection)
    let full_path = validate_path(repo_root, &hit.path).map_err(|e| {
        StoreError::Other(e.context(format!("path validation failed for {}", hit.path)))
    })?;

    if !full_path.exists() {
        return Err(StoreError::Other(anyhow::anyhow!(
            "file not found: {}",
            hit.path
        )));
    }

    // 2. Read file bytes once (TOCTOU-safe: sha + content from same read)
    let bytes = std::fs::read(&full_path)
        .map_err(|e| StoreError::Other(anyhow::anyhow!("failed to read {}: {}", hit.path, e)))?;
    let current_sha = blake3::hash(&bytes).to_hex().to_string();

    // 3. Stale check
    if hit.content_sha != current_sha {
        return Err(StoreError::StaleHit {
            path: hit.path.clone(),
        });
    }

    // 4. Decode same bytes for content/lines
    let content = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len() as u64;

    // 5. Range validation (1 ≤ start ≤ end ≤ total_lines)
    if hit.start_line < 1 {
        return Err(StoreError::InvalidRange(format!(
            "start_line {} < 1",
            hit.start_line
        )));
    }
    if hit.start_line > hit.end_line {
        return Err(StoreError::InvalidRange(format!(
            "start_line {} > end_line {}",
            hit.start_line, hit.end_line
        )));
    }
    if hit.end_line > total_lines {
        return Err(StoreError::InvalidRange(format!(
            "end_line {} > total_lines {}",
            hit.end_line, total_lines
        )));
    }

    // 6. Build excerpt from same decoded content
    let start_idx = (hit.start_line - 1) as usize;
    let end_idx = hit.end_line as usize;
    let excerpt = lines[start_idx..end_idx].join("\n");

    // 7. Build Evidence
    let why = match &hit.symbol_name {
        Some(name) => vec![format!("store_hit: {} from {}", name, hit.source.as_json())],
        None => vec![format!("store_hit from {}", hit.source.as_json())],
    };

    let score_parts = match channel {
        Channel::Bm25 => ScoreParts {
            bm25: Some(hit.score),
            ..Default::default()
        },
        _ => ScoreParts::default(),
    };

    let evidence = Evidence::new(
        &hit.path,
        hit.start_line,
        hit.end_line,
        &current_sha,
        hit.score,
        why,
        vec![channel],
    )
    .with_excerpt(&excerpt)
    .with_language(&hit.language)
    .with_freshness(Freshness::VerifiedCurrent)
    .with_score_parts(score_parts);

    Ok(evidence)
}

impl StoreSource {
    fn as_json(&self) -> &str {
        match self {
            StoreSource::Conservative => "conservative",
            StoreSource::Tdb => "tdb",
            StoreSource::External(s) => s.as_str(),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn compute_sha(path: &std::path::PathBuf) -> String {
        let bytes = std::fs::read(path).unwrap();
        blake3::hash(&bytes).to_hex().to_string()
    }

    #[test]
    fn materialize_valid_hit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("lib.rs"), "fn hello() {}\nfn world() {}\n").unwrap();

        let sha = compute_sha(&root.join("lib.rs"));
        let hit = StoreHit {
            path: "lib.rs".into(),
            start_line: 1,
            end_line: 2,
            content_sha: sha,
            score: 1.0,
            source: StoreSource::Conservative,
            language: "rust".into(),
            symbol_name: None,
        };

        let evidence = materialize_evidence(root, &hit, Channel::Bm25).unwrap();
        assert_eq!(evidence.core.path, "lib.rs");
        assert_eq!(evidence.core.start_line, 1);
        assert_eq!(evidence.core.end_line, 2);
        let meta = evidence.meta.as_ref().unwrap();
        assert_eq!(meta.freshness, Some(Freshness::VerifiedCurrent));
    }

    #[test]
    fn materialize_rejects_stale_hit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("lib.rs"), "fn hello() {}\n").unwrap();

        let hit = StoreHit {
            path: "lib.rs".into(),
            start_line: 1,
            end_line: 1,
            content_sha: "stale_sha".into(),
            score: 1.0,
            source: StoreSource::Conservative,
            language: "rust".into(),
            symbol_name: None,
        };

        let result = materialize_evidence(root, &hit, Channel::Bm25);
        assert!(result.is_err());
        match result.unwrap_err() {
            StoreError::StaleHit { path } => assert_eq!(path, "lib.rs"),
            e => panic!("expected StaleHit, got {:?}", e),
        }
    }

    #[test]
    fn materialize_rejects_empty_content_sha() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("lib.rs"), "fn hello() {}\n").unwrap();

        let hit = StoreHit {
            path: "lib.rs".into(),
            start_line: 1,
            end_line: 1,
            content_sha: "".into(),
            score: 1.0,
            source: StoreSource::Conservative,
            language: "rust".into(),
            symbol_name: None,
        };

        let result = materialize_evidence(root, &hit, Channel::Bm25);
        assert!(result.is_err(), "empty content_sha should be rejected");
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("empty"),
            "error should mention empty: got {}",
            err_msg
        );
    }

    #[test]
    fn materialize_rejects_invalid_range() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("lib.rs"), "fn hello() {}\n").unwrap();

        let sha = compute_sha(&root.join("lib.rs"));

        // start_line = 0
        let hit = StoreHit {
            path: "lib.rs".into(),
            start_line: 0,
            end_line: 1,
            content_sha: sha.clone(),
            score: 1.0,
            source: StoreSource::Conservative,
            language: "rust".into(),
            symbol_name: None,
        };
        let result = materialize_evidence(root, &hit, Channel::Bm25);
        assert!(result.is_err(), "start_line=0 should be rejected");

        // end_line > total_lines
        let hit2 = StoreHit {
            path: "lib.rs".into(),
            start_line: 1,
            end_line: 999,
            content_sha: sha,
            score: 1.0,
            source: StoreSource::Conservative,
            language: "rust".into(),
            symbol_name: None,
        };
        let result2 = materialize_evidence(root, &hit2, Channel::Bm25);
        assert!(
            result2.is_err(),
            "end_line > total_lines should be rejected"
        );
    }

    #[test]
    fn materialize_citation_valid() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("app.rs"), "fn auth() {}\n").unwrap();

        let sha = compute_sha(&root.join("app.rs"));
        let hit = StoreHit {
            path: "app.rs".into(),
            start_line: 1,
            end_line: 1,
            content_sha: sha,
            score: 0.9,
            source: StoreSource::Conservative,
            language: "rust".into(),
            symbol_name: Some("auth".into()),
        };

        let evidence = materialize_evidence(root, &hit, Channel::Bm25).unwrap();
        assert!(!evidence.core.path.is_empty());
        assert!(!evidence.core.content_sha.is_empty());
        assert!(evidence.core.start_line >= 1);
        assert!(evidence.core.start_line <= evidence.core.end_line);
        let meta = evidence.meta.as_ref().unwrap();
        assert_eq!(meta.freshness, Some(Freshness::VerifiedCurrent));
        assert!(meta.excerpt.is_some());
    }

    #[test]
    fn materialize_sha_and_excerpt_from_same_bytes() {
        // Verify TOCTOU safety: sha and excerpt come from the same read.
        // If they came from separate reads, a file modification between reads
        // could cause sha/excerpt mismatch.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let content = "line1\nline2\nline3\n";
        std::fs::write(root.join("test.rs"), content).unwrap();

        let sha = compute_sha(&root.join("test.rs"));
        let hit = StoreHit {
            path: "test.rs".into(),
            start_line: 1,
            end_line: 3,
            content_sha: sha,
            score: 1.0,
            source: StoreSource::Conservative,
            language: "rust".into(),
            symbol_name: None,
        };

        let evidence = materialize_evidence(root, &hit, Channel::Regex).unwrap();
        // The excerpt should match the content we wrote
        assert_eq!(
            evidence.meta.as_ref().unwrap().excerpt.as_deref(),
            Some("line1\nline2\nline3")
        );
        // The sha in evidence should match the sha we computed
        assert_eq!(
            evidence.core.content_sha,
            compute_sha(&root.join("test.rs"))
        );
    }

    #[test]
    fn capabilities_none() {
        let caps = StoreCapabilities::none();
        assert!(!caps.metadata);
        assert!(!caps.chunks);
        assert!(!caps.lexical);
        assert!(!caps.vector);
        assert!(!caps.graph);
    }
}
