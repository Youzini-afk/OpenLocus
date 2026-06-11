//! TriviumDB (TDB) Level0 adapter probe — feature-gated behind `tdb`.
//!
//! This is a *Level0 adapter probe*, not a retrieval quality claim.
//! It proves that TriviumDB can be wired into the StoreBackend / ChunkStore
//! trait hierarchy with honest capability reporting. The adapter uses
//! `dim=1` with a dummy `[0.0]` vector for metadata/chunk persistence only;
//! it does **not** provide meaningful vector, lexical, or graph search.
//!
//! # Capabilities
//!
//! - `metadata = true` — stores chunk metadata as JSON payloads in TDB nodes.
//! - `chunks = true` — maintains in-memory `ChunkRecord` list for conformance.
//! - `lexical = false` — no BM25 / full-text search through TDB.
//! - `vector = false` — dim=1 is a smoke probe, not vector quality.
//! - `graph = false` — no graph edges are created.
//!
//! # Payload schema
//!
//! Each node payload follows `openlocus_schema=tdb_chunk_v1`:
//!
//! ```json
//! {
//!   "openlocus_schema": "tdb_chunk_v1",
//!   "path": "src/lib.rs",
//!   "start_line": 1,
//!   "end_line": 30,
//!   "content_sha": "blake3hex...",
//!   "language": "rust",
//!   "kind": "line_range"
//! }
//! ```
//!
//! # Build discipline
//!
//! Copies ConservativeChunkStore discipline:
//! - `validate_path` on every record before reading.
//! - Read file bytes once; compute `content_sha` from same bytes (TOCTOU-safe).
//! - Skip stale records (content_sha mismatch).
//! - Skip path-traversal records.
//! - Skip empty files (no start=1,end=0 invalid chunks).
//! - Ingest only from `scan_repo` filtered records; never walks filesystem.
//!
//! # Purge safety
//!
//! Purge only deletes the adapter-owned TDB artifact set (`.tdb` plus known
//! sidecars) **after** verifying the marker file contains the adapter's
//! signature. It never deletes unmarked paths.
//!
//! # Materialization
//!
//! If this adapter ever exposes search hits, they must be converted to
//! `StoreHit` and then go through `materialize_evidence()`. TDB hits
//! cannot directly become Evidence.

use crate::{
    ChunkKey, ChunkKind, ChunkRecord, ChunkStore, StoreBackend, StoreCapabilities, StoreDebug,
    StoreHealth, StoreResult,
};
use openlocus_repo::scan::FileRecord;
use openlocus_repo::validate_path;
use std::collections::HashMap;
use std::path::Path;

/// Marker file content written alongside the `.tdb` file.
/// Used to verify purge only deletes adapter-owned paths.
const TDB_MARKER_CONTENT: &str = "openlocus-tdb-adapter-v1";

/// Maximum chunk size in lines for storage (same as ConservativeChunkStore).
const MAX_CHUNK_LINES: u64 = 30;

/// Payload schema identifier.
const TDB_CHUNK_SCHEMA: &str = "tdb_chunk_v1";

/// TriviumDB-backed chunk store (Level0 adapter probe).
///
/// Uses `Database<f32>` with `dim=1` and stores chunk metadata as JSON payloads.
/// The vector `[0.0]` is a smoke probe only — this is NOT vector quality.
pub struct TdbChunkStore {
    db: Option<triviumdb::Database<f32>>,
    chunks: Vec<ChunkRecord>,
    files: HashMap<String, usize>,
    snapshot_id: Option<String>,
    built: bool,
    /// Path to the .tdb file for persistence.
    db_path: String,
    /// Count of records skipped during build.
    skipped_count: usize,
}

impl TdbChunkStore {
    /// Open (or create) a TDB chunk store at the given path.
    ///
    /// The `path` should end with `.tdb`. A marker file at `{path}.openlocus_marker`
    /// is written to identify adapter-owned data.
    pub fn open(path: &Path) -> StoreResult<Self> {
        let db_path = path.to_string_lossy().to_string();

        // Create parent directories before writing the marker or opening TDB.
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| {
                crate::StoreError::Other(anyhow::anyhow!("failed to create TDB parent dir: {}", e))
            })?;
        }

        // Write marker file to identify adapter-owned data
        let marker_path = format!("{}.openlocus_marker", db_path);
        std::fs::write(&marker_path, TDB_MARKER_CONTENT).map_err(|e| {
            crate::StoreError::Other(anyhow::anyhow!(
                "failed to write TDB marker at {}: {}",
                marker_path,
                e
            ))
        })?;

        // Open with dim=1 for metadata/chunk persistence probe
        let db = triviumdb::Database::open(&db_path, 1).map_err(|e| {
            crate::StoreError::Other(anyhow::anyhow!(
                "failed to open TriviumDB at {}: {}",
                db_path,
                e
            ))
        })?;

        Ok(Self {
            db: Some(db),
            chunks: Vec::new(),
            files: HashMap::new(),
            snapshot_id: None,
            built: false,
            db_path,
            skipped_count: 0,
        })
    }

    /// Verify the marker file at the given path contains our signature.
    fn verify_marker(path: &Path) -> bool {
        let marker_path = format!("{}.openlocus_marker", path.to_string_lossy());
        match std::fs::read_to_string(&marker_path) {
            Ok(content) => content == TDB_MARKER_CONTENT,
            Err(_) => false,
        }
    }
}

impl StoreBackend for TdbChunkStore {
    fn name(&self) -> &str {
        "tdb"
    }

    fn health(&self) -> StoreHealth {
        StoreHealth {
            available: self.db.is_some(),
            backend: self.name().to_string(),
            capabilities: StoreCapabilities {
                metadata: true,
                chunks: true,
                lexical: false,
                vector: false,
                graph: false,
            },
            snapshot_id: self.snapshot_id.clone(),
            error: if self.db.is_none() {
                Some("TDB database not opened".into())
            } else {
                None
            },
        }
    }

    fn build(&mut self, repo_root: &Path, records: &[FileRecord]) -> StoreResult<StoreDebug> {
        // Need an open database
        let db = self
            .db
            .as_mut()
            .ok_or_else(|| crate::StoreError::NotAvailable("TDB database not opened".into()))?;

        self.chunks.clear();
        self.files.clear();
        self.skipped_count = 0;

        for record in records {
            // Validate path before reading (rejects absolute, .., symlinks outside repo)
            let full_path = match validate_path(repo_root, &record.path) {
                Ok(p) => p,
                Err(_) => {
                    self.skipped_count += 1;
                    continue;
                }
            };

            if !full_path.exists() || !full_path.is_file() {
                self.skipped_count += 1;
                continue;
            }

            // Read file bytes once (TOCTOU-safe: sha + lines from same read)
            let bytes = match std::fs::read(&full_path) {
                Ok(b) => b,
                Err(_) => {
                    self.skipped_count += 1;
                    continue;
                }
            };

            let current_sha = blake3::hash(&bytes).to_hex().to_string();

            // Skip stale records (content_sha mismatch)
            if !record.content_sha.is_empty() && record.content_sha != current_sha {
                self.skipped_count += 1;
                continue;
            }

            let content = String::from_utf8_lossy(&bytes);
            let lines: Vec<&str> = content.lines().collect();
            let total_lines = lines.len() as u64;

            // Skip empty files — no materializable chunks (start=1,end=0 is invalid)
            if total_lines == 0 {
                self.files.insert(record.path.clone(), 0);
                continue;
            }

            // Create bounded chunks — same discipline as ConservativeChunkStore
            let mut chunk_start = 0u64;
            while chunk_start < total_lines {
                let chunk_end = (chunk_start + MAX_CHUNK_LINES).min(total_lines);

                let payload = serde_json::json!({
                    "openlocus_schema": TDB_CHUNK_SCHEMA,
                    "path": record.path,
                    "start_line": chunk_start + 1,
                    "end_line": chunk_end,
                    "content_sha": current_sha,
                    "language": record.language,
                    "kind": "line_range",
                });

                // Insert into TDB with dim=1 dummy vector [0.0]
                // This is a metadata/chunk persistence probe, NOT vector quality.
                db.insert(&[0.0f32], payload).map_err(|e| {
                    crate::StoreError::Other(anyhow::anyhow!(
                        "failed to insert chunk into TriviumDB: {}",
                        e
                    ))
                })?;

                self.chunks.push(ChunkRecord {
                    key: ChunkKey {
                        path: record.path.clone(),
                        start_line: chunk_start + 1,
                        end_line: chunk_end,
                    },
                    kind: ChunkKind::LineRange,
                    content_sha: current_sha.clone(),
                    language: record.language.clone(),
                    symbol_name: None,
                });

                chunk_start = chunk_end;
            }

            *self.files.entry(record.path.clone()).or_insert(0) += 1;
        }

        db.flush().map_err(|e| {
            crate::StoreError::Other(anyhow::anyhow!("failed to flush TriviumDB: {}", e))
        })?;

        self.snapshot_id = Some(format!(
            "snap-tdb-{}",
            chrono::Utc::now().timestamp_millis()
        ));
        self.built = true;

        Ok(StoreDebug {
            backend_name: self.name().to_string(),
            snapshot_id: self.snapshot_id.clone(),
            chunk_count: self.chunks.len(),
            file_count: self.files.len(),
        })
    }

    fn purge(&mut self) -> StoreResult<()> {
        // Only delete adapter-owned path after marker check
        let path = Path::new(&self.db_path);

        if !path.exists() {
            // Already purged or never created — just clear in-memory state
            self.db = None;
            self.chunks.clear();
            self.files.clear();
            self.snapshot_id = None;
            self.built = false;
            self.skipped_count = 0;
            return Ok(());
        }

        // Verify marker before deletion — never delete arbitrary paths
        if !Self::verify_marker(path) {
            return Err(crate::StoreError::Other(anyhow::anyhow!(
                "TDB purge refused: marker file missing or mismatched at {}.{}. \
                 Will not delete unverified path.",
                self.db_path,
                "openlocus_marker"
            )));
        }

        // Close the database first
        if let Some(db) = self.db.take() {
            // Database::close consumes self and flushes, but we can't call it
            // on an Option<Database>. Just drop it.
            drop(db);
        }

        // Delete the .tdb file
        if path.exists() {
            std::fs::remove_file(path).map_err(|e| {
                crate::StoreError::Other(anyhow::anyhow!("failed to delete TDB file: {}", e))
            })?;
        }

        // Delete the .tdb.lock file if present
        let lock_path = format!("{}.lock", self.db_path);
        let lock = Path::new(&lock_path);
        if lock.exists() {
            let _ = std::fs::remove_file(lock);
        }

        // Delete the .tdb.wal file if present
        let wal_path = format!("{}.wal", self.db_path);
        let wal = Path::new(&wal_path);
        if wal.exists() {
            let _ = std::fs::remove_file(wal);
        }

        // Delete the .tdb.vec file if present (Mmap mode)
        let vec_path = format!("{}.vec", self.db_path);
        let vec_file = Path::new(&vec_path);
        if vec_file.exists() {
            let _ = std::fs::remove_file(vec_file);
        }

        // Delete the marker file
        let marker_path = format!("{}.openlocus_marker", self.db_path);
        let marker = Path::new(&marker_path);
        if marker.exists() {
            let _ = std::fs::remove_file(marker);
        }

        self.chunks.clear();
        self.files.clear();
        self.snapshot_id = None;
        self.built = false;
        self.skipped_count = 0;
        Ok(())
    }
}

impl ChunkStore for TdbChunkStore {
    fn chunks(&self) -> Vec<ChunkRecord> {
        self.chunks.clone()
    }

    fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    fn file_count(&self) -> usize {
        self.files.len()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StoreBackend;

    fn compute_current_sha(path: &std::path::PathBuf) -> String {
        let bytes = std::fs::read(path).unwrap();
        blake3::hash(&bytes).to_hex().to_string()
    }

    #[test]
    fn tdb_adapter_health_available_when_open() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.tdb");
        let store = TdbChunkStore::open(&db_path).unwrap();
        let health = store.health();
        assert!(
            health.available,
            "TDB adapter should be available after open"
        );
        assert_eq!(health.backend, "tdb");
        assert!(health.capabilities.metadata);
        assert!(health.capabilities.chunks);
        assert!(!health.capabilities.lexical);
        assert!(!health.capabilities.vector);
        assert!(!health.capabilities.graph);
        assert!(health.error.is_none());
    }

    #[test]
    fn tdb_adapter_build_from_records() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let db_path = root.join("test.tdb");

        std::fs::write(root.join("lib.rs"), "fn a() {}\nfn b() {}\n").unwrap();
        std::fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();

        let sha1 = compute_current_sha(&root.join("lib.rs"));
        let sha2 = compute_current_sha(&root.join("main.rs"));

        let records = vec![
            FileRecord {
                path: "lib.rs".into(),
                size: 0,
                content_sha: sha1,
                language: "rust".into(),
            },
            FileRecord {
                path: "main.rs".into(),
                size: 0,
                content_sha: sha2,
                language: "rust".into(),
            },
        ];

        let mut store = TdbChunkStore::open(&db_path).unwrap();
        let debug = store.build(root, &records).unwrap();

        assert_eq!(debug.file_count, 2);
        assert!(debug.chunk_count >= 2);
        assert!(store.built);
        assert!(store.snapshot_id.is_some());
    }

    #[test]
    fn tdb_adapter_skips_stale_record() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let db_path = root.join("test.tdb");

        std::fs::write(root.join("lib.rs"), "fn a() {}\n").unwrap();

        // Pass a stale content_sha
        let records = vec![FileRecord {
            path: "lib.rs".into(),
            size: 0,
            content_sha: "stale_sha".into(),
            language: "rust".into(),
        }];

        let mut store = TdbChunkStore::open(&db_path).unwrap();
        let debug = store.build(root, &records).unwrap();
        assert_eq!(debug.file_count, 0, "stale record should be skipped");
        assert_eq!(
            debug.chunk_count, 0,
            "stale record should produce no chunks"
        );
        assert_eq!(store.skipped_count, 1);
    }

    #[test]
    fn tdb_adapter_skips_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let db_path = root.join("test.tdb");

        std::fs::write(root.join("empty.rs"), "").unwrap();

        let sha = compute_current_sha(&root.join("empty.rs"));
        let records = vec![FileRecord {
            path: "empty.rs".into(),
            size: 0,
            content_sha: sha,
            language: "rust".into(),
        }];

        let mut store = TdbChunkStore::open(&db_path).unwrap();
        let debug = store.build(root, &records).unwrap();
        // Empty file should not produce any chunks (no start=1,end=0)
        assert_eq!(debug.chunk_count, 0, "empty file should produce no chunks");
        assert!(
            !store
                .chunks
                .iter()
                .any(|c| c.key.start_line > c.key.end_line)
        );
    }

    #[test]
    fn tdb_adapter_skips_traversal_record() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let db_path = root.join("test.tdb");

        std::fs::write(root.join("lib.rs"), "fn a() {}\n").unwrap();

        let sha = compute_current_sha(&root.join("lib.rs"));
        let records = vec![
            FileRecord {
                path: "lib.rs".into(),
                size: 0,
                content_sha: sha,
                language: "rust".into(),
            },
            FileRecord {
                path: "../etc/passwd".into(),
                size: 0,
                content_sha: "irrelevant".into(),
                language: "unknown".into(),
            },
        ];

        let mut store = TdbChunkStore::open(&db_path).unwrap();
        let debug = store.build(root, &records).unwrap();
        assert_eq!(debug.file_count, 1, "traversal record should be skipped");
        assert_eq!(store.skipped_count, 1);
    }

    #[test]
    fn tdb_adapter_purge_refuses_without_marker() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let db_path = root.join("test2.tdb");

        std::fs::write(root.join("lib.rs"), "fn a() {}\n").unwrap();

        let sha = compute_current_sha(&root.join("lib.rs"));
        let records = vec![FileRecord {
            path: "lib.rs".into(),
            size: 0,
            content_sha: sha,
            language: "rust".into(),
        }];

        let mut store = TdbChunkStore::open(&db_path).unwrap();
        store.build(root, &records).unwrap();

        // Corrupt the marker
        let marker_path = format!("{}.openlocus_marker", db_path.to_string_lossy());
        std::fs::write(&marker_path, "corrupted").unwrap();

        // Purge should refuse
        let result = store.purge();
        assert!(result.is_err(), "purge should refuse without valid marker");
        match result.unwrap_err() {
            crate::StoreError::Other(e) => {
                let msg = format!("{}", e);
                assert!(
                    msg.contains("marker") || msg.contains("refused"),
                    "error should mention marker/refused: got {}",
                    msg
                );
            }
            e => panic!("expected StoreError::Other, got {:?}", e),
        }

        // The .tdb file should still exist
        assert!(
            db_path.exists(),
            ".tdb file should not be deleted after refused purge"
        );
    }

    #[test]
    fn tdb_adapter_purge_deletes_marked_artifact_set() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let db_path = root.join("marked.tdb");

        std::fs::write(root.join("lib.rs"), "fn a() {}\n").unwrap();
        let sha = compute_current_sha(&root.join("lib.rs"));
        let records = vec![FileRecord {
            path: "lib.rs".into(),
            size: 0,
            content_sha: sha,
            language: "rust".into(),
        }];

        let mut store = TdbChunkStore::open(&db_path).unwrap();
        store.build(root, &records).unwrap();

        let lock_path = format!("{}.lock", db_path.to_string_lossy());
        let wal_path = format!("{}.wal", db_path.to_string_lossy());
        let vec_path = format!("{}.vec", db_path.to_string_lossy());
        let marker_path = format!("{}.openlocus_marker", db_path.to_string_lossy());
        std::fs::write(&lock_path, "lock").unwrap();
        std::fs::write(&wal_path, "wal").unwrap();
        std::fs::write(&vec_path, "vec").unwrap();

        store.purge().unwrap();

        assert!(!db_path.exists(), ".tdb file should be deleted");
        assert!(
            !std::path::Path::new(&lock_path).exists(),
            "lock sidecar should be deleted"
        );
        assert!(
            !std::path::Path::new(&wal_path).exists(),
            "wal sidecar should be deleted"
        );
        assert!(
            !std::path::Path::new(&vec_path).exists(),
            "vec sidecar should be deleted"
        );
        assert!(
            !std::path::Path::new(&marker_path).exists(),
            "marker should be deleted"
        );
        assert_eq!(store.chunk_count(), 0);
        assert!(!store.health().available);
    }

    #[test]
    fn tdb_adapter_materialization_conformance() {
        // Test that if we were to produce StoreHits from TDB, they must
        // go through materialize_evidence(). Since this adapter doesn't
        // implement LexicalStore, we verify the principle by checking
        // that the chunk records can be converted to StoreHit and
        // materialized correctly.
        use crate::{StoreHit, StoreSource, materialize_evidence};
        use openlocus_core::Channel;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let db_path = root.join("test.tdb");

        std::fs::write(root.join("app.rs"), "fn auth() {}\n").unwrap();

        let sha = compute_current_sha(&root.join("app.rs"));
        let records = vec![FileRecord {
            path: "app.rs".into(),
            size: 0,
            content_sha: sha,
            language: "rust".into(),
        }];

        let mut store = TdbChunkStore::open(&db_path).unwrap();
        store.build(root, &records).unwrap();

        // Convert a chunk record to StoreHit and materialize
        let chunk = &store.chunks()[0];
        let hit = StoreHit {
            path: chunk.key.path.clone(),
            start_line: chunk.key.start_line,
            end_line: chunk.key.end_line,
            content_sha: chunk.content_sha.clone(),
            score: 1.0,
            source: StoreSource::Tdb,
            language: chunk.language.clone(),
            symbol_name: None,
        };

        let evidence = materialize_evidence(root, &hit, Channel::Bm25).unwrap();
        assert_eq!(evidence.core.path, "app.rs");
        assert_eq!(evidence.core.start_line, 1);
        assert!(evidence.core.start_line <= evidence.core.end_line);
        let meta = evidence.meta.as_ref().unwrap();
        assert_eq!(
            meta.freshness,
            Some(openlocus_core::Freshness::VerifiedCurrent)
        );
    }

    #[test]
    fn tdb_adapter_materialization_rejects_stale() {
        use crate::{StoreHit, StoreSource, materialize_evidence};
        use openlocus_core::Channel;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let db_path = root.join("test.tdb");

        std::fs::write(root.join("lib.rs"), "fn a() {}\n").unwrap();

        let sha = compute_current_sha(&root.join("lib.rs"));
        let records = vec![FileRecord {
            path: "lib.rs".into(),
            size: 0,
            content_sha: sha,
            language: "rust".into(),
        }];

        let mut store = TdbChunkStore::open(&db_path).unwrap();
        store.build(root, &records).unwrap();

        // Now modify the file to make the stored sha stale
        std::fs::write(root.join("lib.rs"), "fn modified() {}\n").unwrap();

        let chunk = &store.chunks()[0];
        let hit = StoreHit {
            path: chunk.key.path.clone(),
            start_line: chunk.key.start_line,
            end_line: chunk.key.end_line,
            content_sha: chunk.content_sha.clone(), // stale sha
            score: 1.0,
            source: StoreSource::Tdb,
            language: chunk.language.clone(),
            symbol_name: None,
        };

        let result = materialize_evidence(root, &hit, Channel::Bm25);
        assert!(result.is_err(), "stale hit should be rejected");
        match result.unwrap_err() {
            crate::StoreError::StaleHit { path } => assert_eq!(path, "lib.rs"),
            e => panic!("expected StaleHit, got {:?}", e),
        }
    }

    #[test]
    fn tdb_adapter_materialization_rejects_empty_sha() {
        use crate::{StoreHit, StoreSource, materialize_evidence};
        use openlocus_core::Channel;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(root.join("lib.rs"), "fn a() {}\n").unwrap();

        let hit = StoreHit {
            path: "lib.rs".into(),
            start_line: 1,
            end_line: 1,
            content_sha: "".into(), // empty sha
            score: 1.0,
            source: StoreSource::Tdb,
            language: "rust".into(),
            symbol_name: None,
        };

        let result = materialize_evidence(root, &hit, Channel::Bm25);
        assert!(result.is_err(), "empty content_sha should be rejected");
    }

    #[test]
    fn tdb_adapter_materialization_rejects_invalid_range() {
        use crate::{StoreHit, StoreSource, materialize_evidence};
        use openlocus_core::Channel;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(root.join("lib.rs"), "fn a() {}\n").unwrap();

        let sha = compute_current_sha(&root.join("lib.rs"));

        // start_line = 0
        let hit = StoreHit {
            path: "lib.rs".into(),
            start_line: 0,
            end_line: 1,
            content_sha: sha.clone(),
            score: 1.0,
            source: StoreSource::Tdb,
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
            source: StoreSource::Tdb,
            language: "rust".into(),
            symbol_name: None,
        };
        let result2 = materialize_evidence(root, &hit2, Channel::Bm25);
        assert!(
            result2.is_err(),
            "end_line > total_lines should be rejected"
        );
    }
}
