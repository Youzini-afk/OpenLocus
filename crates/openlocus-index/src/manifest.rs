//! Manifest for persistent BM25 index.
//!
//! Tracks schema_version, file/chunk counts, policy hash, and per-file
//! metadata (path, content_sha, size_bytes, language, indexed/skipped_reason).
//!
//! R8 adds `chunk_strategy` and AST stats to the manifest.
//! Schema version `r8-bm25-v2` is required for AST-built indexes.
//! The manifest loader refuses indexes with unrecognized schema versions
//! or chunk strategies.

use anyhow::{Context, Result, bail};
use openlocus_core::Policy;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Current schema version for R8 persistent BM25 index.
pub const SCHEMA_VERSION: &str = "r8-bm25-v2";

/// Legacy R7 schema version (still accepted for `line_window_v1` strategy).
pub const SCHEMA_VERSION_R7: &str = "r7-bm25-v1";

/// Relative path to the index directory within .openlocus.
pub const INDEX_DIR_RELATIVE: &str = ".openlocus/index";

/// Relative path to the Tantivy index data.
pub const TANTIVY_DIR_RELATIVE: &str = ".openlocus/index/tantivy";

/// Relative path to the manifest file.
pub const MANIFEST_PATH_RELATIVE: &str = ".openlocus/index/manifest.json";

/// Chunk strategy used when building the index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkStrategy {
    /// Fixed-size line windows (R7 default).
    LineWindowV1,
    /// AST-bounded chunks with fallback line windows (R8 experimental).
    AstV1,
}

impl ChunkStrategy {
    /// Parse from CLI string: "line" or "ast".
    pub fn from_cli_str(s: &str) -> Option<Self> {
        match s {
            "line" => Some(Self::LineWindowV1),
            "ast" => Some(Self::AstV1),
            _ => None,
        }
    }

    /// Short CLI string.
    pub fn to_cli_str(&self) -> &'static str {
        match self {
            Self::LineWindowV1 => "line",
            Self::AstV1 => "ast",
        }
    }
}

/// Per-file entry in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestFileEntry {
    pub path: String,
    pub content_sha: String,
    pub size_bytes: u64,
    pub language: String,
    /// "indexed" or "skipped"
    pub status: String,
    /// None for indexed files; Some(reason) for skipped files
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<String>,
}

/// AST-related stats stored in the manifest (only for ast_v1 strategy).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AstManifestStats {
    pub supported_files: u64,
    pub fallback_files: u64,
    pub parser_error_files: u64,
    pub ast_chunks: u64,
    pub fallback_chunks: u64,
}

/// Index manifest tracking all indexed files and policy hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexManifest {
    pub schema_version: String,
    pub file_count: u64,
    pub chunk_count: u64,
    pub policy_hash: String,
    pub files: Vec<ManifestFileEntry>,
    /// Chunk strategy used when building the index.
    #[serde(default = "default_chunk_strategy")]
    pub chunk_strategy: ChunkStrategy,
    /// AST stats (present only for ast_v1 strategy).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ast_stats: Option<AstManifestStats>,
}

fn default_chunk_strategy() -> ChunkStrategy {
    ChunkStrategy::LineWindowV1
}

impl IndexManifest {
    /// Create a new manifest with the given fields.
    pub fn new(policy_hash: String, files: Vec<ManifestFileEntry>, chunk_count: u64) -> Self {
        let file_count = files.iter().filter(|f| f.status == "indexed").count() as u64;
        Self {
            schema_version: SCHEMA_VERSION.to_string(),
            file_count,
            chunk_count,
            policy_hash,
            files,
            chunk_strategy: ChunkStrategy::LineWindowV1,
            ast_stats: None,
        }
    }

    /// Create a new manifest with explicit chunk strategy and AST stats.
    pub fn new_with_strategy(
        policy_hash: String,
        files: Vec<ManifestFileEntry>,
        chunk_count: u64,
        chunk_strategy: ChunkStrategy,
        ast_stats: Option<AstManifestStats>,
    ) -> Self {
        let file_count = files.iter().filter(|f| f.status == "indexed").count() as u64;
        Self {
            schema_version: SCHEMA_VERSION.to_string(),
            file_count,
            chunk_count,
            policy_hash,
            files,
            chunk_strategy,
            ast_stats,
        }
    }

    /// Load manifest from the repo's .openlocus/index/manifest.json.
    /// Validates schema version and chunk strategy; refuses unrecognized.
    pub fn load(repo_root: &Path) -> Result<Self> {
        let path = repo_root.join(MANIFEST_PATH_RELATIVE);
        let content =
            std::fs::read_to_string(&path).with_context(|| "failed to read manifest.json")?;
        let manifest: IndexManifest =
            serde_json::from_str(&content).with_context(|| "failed to parse manifest.json")?;

        // Validate schema version
        if manifest.schema_version != SCHEMA_VERSION && manifest.schema_version != SCHEMA_VERSION_R7
        {
            bail!(
                "unrecognized manifest schema_version: {}. Expected {} or {}. Rebuild the index.",
                manifest.schema_version,
                SCHEMA_VERSION,
                SCHEMA_VERSION_R7
            );
        }

        Ok(manifest)
    }

    /// Save manifest to the repo's .openlocus/index/manifest.json.
    pub fn save(&self, repo_root: &Path) -> Result<()> {
        let path = repo_root.join(MANIFEST_PATH_RELATIVE);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content =
            serde_json::to_string_pretty(self).with_context(|| "failed to serialize manifest")?;
        std::fs::write(&path, content).with_context(|| "failed to write manifest.json")?;
        Ok(())
    }

    /// Check if the manifest exists.
    pub fn exists(repo_root: &Path) -> bool {
        repo_root.join(MANIFEST_PATH_RELATIVE).exists()
    }
}

/// Compute a policy hash from the policy TOML representation.
/// Uses blake3 of the canonical TOML serialization.
pub fn compute_policy_hash(policy: &Policy) -> String {
    // Serialize policy to TOML for a stable, canonical representation
    let toml_str = toml::to_string(policy).unwrap_or_default();
    blake3::hash(toml_str.as_bytes()).to_hex().to_string()
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let manifest = IndexManifest::new(
            "fake_policy_hash".to_string(),
            vec![
                ManifestFileEntry {
                    path: "src/main.rs".into(),
                    content_sha: "abc123".into(),
                    size_bytes: 100,
                    language: "rust".into(),
                    status: "indexed".into(),
                    skipped_reason: None,
                },
                ManifestFileEntry {
                    path: ".env".into(),
                    content_sha: "def456".into(),
                    size_bytes: 50,
                    language: "unknown".into(),
                    status: "skipped".into(),
                    skipped_reason: Some("policy excluded".into()),
                },
            ],
            5,
        );

        manifest.save(root).unwrap();
        let loaded = IndexManifest::load(root).unwrap();

        assert_eq!(loaded.schema_version, SCHEMA_VERSION);
        assert_eq!(loaded.file_count, 1); // only indexed files
        assert_eq!(loaded.chunk_count, 5);
        assert_eq!(loaded.files.len(), 2);
        assert_eq!(loaded.policy_hash, "fake_policy_hash");
        assert_eq!(loaded.chunk_strategy, ChunkStrategy::LineWindowV1);
    }

    #[test]
    fn manifest_with_ast_strategy() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let manifest = IndexManifest::new_with_strategy(
            "fake_policy_hash".to_string(),
            vec![ManifestFileEntry {
                path: "src/main.rs".into(),
                content_sha: "abc123".into(),
                size_bytes: 100,
                language: "rust".into(),
                status: "indexed".into(),
                skipped_reason: None,
            }],
            3,
            ChunkStrategy::AstV1,
            Some(AstManifestStats {
                supported_files: 1,
                fallback_files: 0,
                parser_error_files: 0,
                ast_chunks: 2,
                fallback_chunks: 1,
            }),
        );

        manifest.save(root).unwrap();
        let loaded = IndexManifest::load(root).unwrap();

        assert_eq!(loaded.chunk_strategy, ChunkStrategy::AstV1);
        assert!(loaded.ast_stats.is_some());
        assert_eq!(loaded.ast_stats.unwrap().ast_chunks, 2);
    }

    #[test]
    fn manifest_exists_check() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        assert!(!IndexManifest::exists(root));

        let manifest = IndexManifest::new("hash".into(), vec![], 0);
        manifest.save(root).unwrap();

        assert!(IndexManifest::exists(root));
    }

    #[test]
    fn policy_hash_deterministic() {
        let p1 = Policy::default();
        let p2 = Policy::default();
        assert_eq!(compute_policy_hash(&p1), compute_policy_hash(&p2));
    }

    #[test]
    fn policy_hash_changes_with_policy() {
        let p1 = Policy::default();
        let mut p2 = Policy::default();
        p2.remote.allow = true;
        assert_ne!(compute_policy_hash(&p1), compute_policy_hash(&p2));
    }

    #[test]
    fn r7_manifest_loads() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Write an R7-style manifest (no chunk_strategy field)
        let r7_manifest = r#"{
            "schema_version": "r7-bm25-v1",
            "file_count": 1,
            "chunk_count": 3,
            "policy_hash": "fake",
            "files": []
        }"#;
        let path = root.join(MANIFEST_PATH_RELATIVE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r7_manifest).unwrap();

        let loaded = IndexManifest::load(root).unwrap();
        assert_eq!(loaded.schema_version, SCHEMA_VERSION_R7);
        // Default chunk_strategy for R7 manifests
        assert_eq!(loaded.chunk_strategy, ChunkStrategy::LineWindowV1);
    }

    #[test]
    fn unknown_schema_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let bad_manifest = r#"{
            "schema_version": "r99-bm25-v99",
            "file_count": 0,
            "chunk_count": 0,
            "policy_hash": "fake",
            "files": []
        }"#;
        let path = root.join(MANIFEST_PATH_RELATIVE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bad_manifest).unwrap();

        let result = IndexManifest::load(root);
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("unrecognized manifest schema_version"));
    }

    #[test]
    fn chunk_strategy_from_cli() {
        assert_eq!(
            ChunkStrategy::from_cli_str("line"),
            Some(ChunkStrategy::LineWindowV1)
        );
        assert_eq!(
            ChunkStrategy::from_cli_str("ast"),
            Some(ChunkStrategy::AstV1)
        );
        assert_eq!(ChunkStrategy::from_cli_str("other"), None);
    }
}
