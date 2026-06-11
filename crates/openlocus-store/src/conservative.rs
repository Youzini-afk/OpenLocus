//! Conservative chunk store — in-memory metadata + chunk storage.
//!
//! This is the R3 Level0 "conservative track" backend. It stores chunk records
//! from scan_repo filtered records, supports metadata and chunks, but
//! does not implement lexical/vector/graph search. Capabilities are
//! explicit: metadata=true, chunks=true, lexical=false, vector=false,
//! graph=false.
//!
//! Build validates paths via `validate_path`, skips stale records
//! (content_sha mismatch) and empty files (no materializable chunks),
//! and computes content_sha from the same bytes used for line splitting
//! (TOCTOU-safe).

use crate::{
    ChunkKey, ChunkKind, ChunkRecord, ChunkStore, StoreBackend, StoreCapabilities, StoreDebug,
    StoreHealth, StoreResult,
};
use openlocus_repo::scan::FileRecord;
use openlocus_repo::validate_path;
use std::collections::HashMap;
use std::path::Path;

/// Maximum chunk size in lines for storage.
const MAX_CHUNK_LINES: u64 = 30;

/// In-memory conservative chunk store (ephemeral, not persistent).
pub struct ConservativeChunkStore {
    chunks: Vec<ChunkRecord>,
    /// Unique file paths seen
    files: HashMap<String, usize>,
    snapshot_id: Option<String>,
    built: bool,
    /// Count of records skipped during build
    skipped_count: usize,
}

impl Default for ConservativeChunkStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ConservativeChunkStore {
    pub fn new() -> Self {
        Self {
            chunks: Vec::new(),
            files: HashMap::new(),
            snapshot_id: None,
            built: false,
            skipped_count: 0,
        }
    }
}

impl StoreBackend for ConservativeChunkStore {
    fn name(&self) -> &str {
        "conservative"
    }

    fn health(&self) -> StoreHealth {
        StoreHealth {
            available: true,
            backend: self.name().to_string(),
            capabilities: StoreCapabilities {
                metadata: true,
                chunks: true,
                lexical: false,
                vector: false,
                graph: false,
            },
            snapshot_id: self.snapshot_id.clone(),
            error: None,
        }
    }

    fn build(&mut self, repo_root: &Path, records: &[FileRecord]) -> StoreResult<StoreDebug> {
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

            // Create bounded chunks
            let mut chunk_start = 0u64;
            while chunk_start < total_lines {
                let chunk_end = (chunk_start + MAX_CHUNK_LINES).min(total_lines);
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

        self.snapshot_id = Some(format!("snap-{}", chrono::Utc::now().timestamp_millis()));
        self.built = true;

        Ok(StoreDebug {
            backend_name: self.name().to_string(),
            snapshot_id: self.snapshot_id.clone(),
            chunk_count: self.chunks.len(),
            file_count: self.files.len(),
        })
    }

    fn purge(&mut self) -> StoreResult<()> {
        self.chunks.clear();
        self.files.clear();
        self.snapshot_id = None;
        self.built = false;
        self.skipped_count = 0;
        Ok(())
    }
}

impl ChunkStore for ConservativeChunkStore {
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
    fn conservative_build_from_records() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
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

        let mut store = ConservativeChunkStore::new();
        let debug = store.build(root, &records).unwrap();

        assert_eq!(debug.file_count, 2);
        assert!(debug.chunk_count >= 2);
        assert!(store.built);
    }

    #[test]
    fn conservative_capabilities() {
        let store = ConservativeChunkStore::new();
        let health = store.health();
        assert!(health.available);
        assert!(health.capabilities.metadata);
        assert!(health.capabilities.chunks);
        assert!(!health.capabilities.lexical);
        assert!(!health.capabilities.vector);
        assert!(!health.capabilities.graph);
    }

    #[test]
    fn conservative_purge() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("lib.rs"), "fn a() {}\n").unwrap();

        let sha = compute_current_sha(&root.join("lib.rs"));
        let records = vec![FileRecord {
            path: "lib.rs".into(),
            size: 0,
            content_sha: sha,
            language: "rust".into(),
        }];

        let mut store = ConservativeChunkStore::new();
        store.build(root, &records).unwrap();
        assert!(store.chunk_count() > 0);

        store.purge().unwrap();
        assert_eq!(store.chunk_count(), 0);
        assert_eq!(store.file_count(), 0);
        assert!(!store.built);
    }

    #[test]
    fn conservative_ignores_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("app.rs"), "fn app() {}\n").unwrap();
        std::fs::write(root.join(".env"), "SECRET=abc\n").unwrap();

        let sha = compute_current_sha(&root.join("app.rs"));
        let records = vec![FileRecord {
            path: "app.rs".into(),
            size: 0,
            content_sha: sha,
            language: "rust".into(),
        }];

        let mut store = ConservativeChunkStore::new();
        store.build(root, &records).unwrap();
        assert_eq!(store.file_count(), 1);
        assert!(!store.chunks.iter().any(|c| c.key.path.contains(".env")));
    }

    #[test]
    fn conservative_skips_stale_record() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("lib.rs"), "fn a() {}\n").unwrap();

        // Pass a stale content_sha
        let records = vec![FileRecord {
            path: "lib.rs".into(),
            size: 0,
            content_sha: "stale_sha".into(),
            language: "rust".into(),
        }];

        let mut store = ConservativeChunkStore::new();
        let debug = store.build(root, &records).unwrap();
        assert_eq!(debug.file_count, 0, "stale record should be skipped");
        assert_eq!(
            debug.chunk_count, 0,
            "stale record should produce no chunks"
        );
        assert_eq!(store.skipped_count, 1);
    }

    #[test]
    fn conservative_skips_empty_file_no_invalid_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("empty.rs"), "").unwrap();

        let sha = compute_current_sha(&root.join("empty.rs"));
        let records = vec![FileRecord {
            path: "empty.rs".into(),
            size: 0,
            content_sha: sha,
            language: "rust".into(),
        }];

        let mut store = ConservativeChunkStore::new();
        let debug = store.build(root, &records).unwrap();
        // Empty file should not produce any chunks (no start=1,end=0)
        assert_eq!(debug.chunk_count, 0, "empty file should produce no chunks");
        // Should not have any chunk with start > end
        assert!(
            !store
                .chunks
                .iter()
                .any(|c| c.key.start_line > c.key.end_line)
        );
    }

    #[test]
    fn conservative_skips_traversal_record() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
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

        let mut store = ConservativeChunkStore::new();
        let debug = store.build(root, &records).unwrap();
        assert_eq!(debug.file_count, 1, "traversal record should be skipped");
        assert_eq!(store.skipped_count, 1);
    }
}
