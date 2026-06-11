//! Dense JSONL vector store.
//!
//! Path: `.openlocus/embeddings/vectors.jsonl`
//! Stores `EmbeddingRecord` records; no raw text.
//! Build records from scanned FileRecords with metadata-only views.
//! Search via cosine similarity.

use crate::audit;
use crate::cache::compute_cache_key;
use crate::gate::gate_embed_input;
use crate::model::{EmbedInput, EmbeddingAuditEvent, EmbeddingRecord, ProviderMetadata};
use crate::provider::EmbeddingProvider;
use anyhow::Result;
use openlocus_repo::scan::FileRecord;
use openlocus_store::StoreHit;
use std::fs;
use std::path::Path;

/// Relative path to the JSONL vector store.
pub const STORE_RELATIVE_PATH: &str = ".openlocus/embeddings/vectors.jsonl";

/// JSONL-based embedding store.
pub struct JsonlEmbeddingStore;

impl JsonlEmbeddingStore {
    /// Build embedding records from scanned file records.
    /// For each nonempty file, create a data_level=0 text view from metadata only:
    /// `path:<path> language:<language> basename:<stem> words:<path tokens>`
    /// No code snippet is included.
    pub fn build(
        repo_root: &Path,
        records: &[FileRecord],
        provider: &dyn EmbeddingProvider,
        metadata: &ProviderMetadata,
        policy: &openlocus_core::Policy,
    ) -> Result<BuildResult> {
        let mut built = Vec::new();
        let mut skipped = 0usize;
        let mut blocked = 0usize;
        let request_id = format!("build-{}", chrono::Utc::now().timestamp_millis());

        for record in records {
            // Skip empty files
            if record.size == 0 {
                skipped += 1;
                continue;
            }

            // Count actual lines in the file to compute a valid span.
            // Use 1..min(total_lines, 8); skip file if total_lines < 1.
            let file_abs = repo_root.join(&record.path);
            let total_lines = match fs::read_to_string(&file_abs) {
                Ok(content) => content.lines().count() as u64,
                Err(_) => {
                    // Cannot read file; skip
                    skipped += 1;
                    continue;
                }
            };
            if total_lines == 0 {
                skipped += 1;
                continue;
            }
            let start_line = 1u64;
            let end_line = std::cmp::min(total_lines, 8u64);

            // Build metadata-only text view (no code snippets)
            let stem = Path::new(&record.path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            let path_tokens: Vec<&str> = record.path.split(['/', '\\']).collect();
            let view_text = format!(
                "path:{} language:{} basename:{} words:{}",
                record.path,
                record.language,
                stem,
                path_tokens.join(" ")
            );
            let text_sha = blake3::hash(view_text.as_bytes()).to_hex().to_string();
            let bytes_selected = view_text.len();

            let input = EmbedInput {
                input_id: format!("{}:{}-{}", record.path, start_line, end_line),
                path: record.path.clone(),
                start_line,
                end_line,
                source_content_sha: record.content_sha.clone(),
                language: record.language.clone(),
                view_kind: "metadata".into(),
                text: view_text.clone(),
                text_sha: text_sha.clone(),
                data_level: 0,
                policy_mode: "local_only".into(),
                purpose: "index".into(),
            };

            // Gate the input
            let decision = gate_embed_input(policy, metadata, &input);

            if !decision.allowed {
                blocked += 1;
                let audit_event = EmbeddingAuditEvent {
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    event: "block".into(),
                    request_id: request_id.clone(),
                    provider_id: metadata.provider_id.clone(),
                    model_id: metadata.model_id.clone(),
                    locality: metadata.locality.clone(),
                    purpose: "index".into(),
                    path: Some(record.path.clone()),
                    line_range: Some(format!("{}-{}", start_line, end_line)),
                    data_level: 0,
                    view_kind: "metadata".into(),
                    bytes_selected,
                    text_sha: text_sha.clone(),
                    secret_scan: decision.secret_scan.clone(),
                    policy_decision: "block".into(),
                    cache_key: String::new(),
                    outbound_attempted: false,
                    reason: Some(decision.reason.clone()),
                };
                let _ = audit::append_audit_event(repo_root, &audit_event);
                continue;
            }

            // Compute cache key
            let cache_key = compute_cache_key(
                &metadata.provider_id,
                &metadata.model_id,
                metadata.dimensions,
                "metadata",
                &text_sha,
                &record.content_sha,
                "local_only",
                0,
            );

            // Embed
            let vector = match provider.embed(&view_text, &text_sha) {
                Ok(v) => v,
                Err(e) => {
                    blocked += 1;
                    let audit_event = EmbeddingAuditEvent {
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        event: "provider_unavailable".into(),
                        request_id: request_id.clone(),
                        provider_id: metadata.provider_id.clone(),
                        model_id: metadata.model_id.clone(),
                        locality: metadata.locality.clone(),
                        purpose: "index".into(),
                        path: Some(record.path.clone()),
                        line_range: Some(format!("{}-{}", start_line, end_line)),
                        data_level: 0,
                        view_kind: "metadata".into(),
                        bytes_selected,
                        text_sha,
                        secret_scan: decision.secret_scan,
                        policy_decision: "provider_error".into(),
                        cache_key,
                        outbound_attempted: false,
                        reason: Some(e.to_string()),
                    };
                    let _ = audit::append_audit_event(repo_root, &audit_event);
                    continue;
                }
            };

            let embedding_record = EmbeddingRecord {
                cache_key,
                provider_id: metadata.provider_id.clone(),
                model_id: metadata.model_id.clone(),
                dimensions: metadata.dimensions,
                path: record.path.clone(),
                start_line,
                end_line,
                source_content_sha: record.content_sha.clone(),
                language: record.language.clone(),
                view_kind: "metadata".into(),
                text_sha,
                policy_mode: "local_only".into(),
                data_level: 0,
                vector,
            };

            // Write audit allow event
            let audit_event = EmbeddingAuditEvent {
                timestamp: chrono::Utc::now().to_rfc3339(),
                event: "allow".into(),
                request_id: request_id.clone(),
                provider_id: metadata.provider_id.clone(),
                model_id: metadata.model_id.clone(),
                locality: metadata.locality.clone(),
                purpose: "index".into(),
                path: Some(record.path.clone()),
                line_range: Some(format!("{}-{}", start_line, end_line)),
                data_level: 0,
                view_kind: "metadata".into(),
                bytes_selected,
                text_sha: embedding_record.text_sha.clone(),
                secret_scan: decision.secret_scan,
                policy_decision: "allow".into(),
                cache_key: embedding_record.cache_key.clone(),
                outbound_attempted: false,
                reason: None,
            };
            let _ = audit::append_audit_event(repo_root, &audit_event);

            built.push(embedding_record);
        }

        // Write records to JSONL
        let store_path = repo_root.join(STORE_RELATIVE_PATH);
        if let Some(parent) = store_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut file = fs::File::create(&store_path)?;
        use std::io::Write;
        for record in &built {
            let line = serde_json::to_string(record)? + "\n";
            file.write_all(line.as_bytes())?;
        }

        Ok(BuildResult {
            record_count: built.len(),
            skipped,
            blocked,
        })
    }

    /// Search the store with a query, returning ranked StoreHits.
    pub fn search(
        repo_root: &Path,
        query: &str,
        provider: &dyn EmbeddingProvider,
        metadata: &ProviderMetadata,
        policy: &openlocus_core::Policy,
        limit: usize,
    ) -> Result<SearchResult> {
        let store_path = repo_root.join(STORE_RELATIVE_PATH);
        if !store_path.exists() {
            return Ok(SearchResult {
                hits: Vec::new(),
                skipped: 0,
                blocked: false,
                reason: Some("embedding store not found; run 'dense build' first".into()),
            });
        }

        // Read records
        let records = Self::list(repo_root)?;
        if records.is_empty() {
            return Ok(SearchResult {
                hits: Vec::new(),
                skipped: 0,
                blocked: false,
                reason: Some("embedding store is empty".into()),
            });
        }

        // Build query embedding
        let request_id = format!("search-{}", chrono::Utc::now().timestamp_millis());
        let query_text = format!("query:{}", query);
        let query_text_sha = blake3::hash(query_text.as_bytes()).to_hex().to_string();

        let query_input = EmbedInput {
            input_id: "<query>".into(),
            path: "<query>".into(),
            start_line: 0,
            end_line: 0,
            source_content_sha: String::new(),
            language: "query".into(),
            view_kind: "query".into(),
            text: query_text.clone(),
            text_sha: query_text_sha.clone(),
            data_level: 0,
            policy_mode: "local_only".into(),
            purpose: "query".into(),
        };

        // Gate the query
        let decision = gate_embed_input(policy, metadata, &query_input);
        if !decision.allowed {
            // Write audit block event
            let cache_key = compute_cache_key(
                &metadata.provider_id,
                &metadata.model_id,
                metadata.dimensions,
                "query",
                &query_text_sha,
                "",
                "local_only",
                0,
            );
            let audit_event = EmbeddingAuditEvent {
                timestamp: chrono::Utc::now().to_rfc3339(),
                event: "block".into(),
                request_id,
                provider_id: metadata.provider_id.clone(),
                model_id: metadata.model_id.clone(),
                locality: metadata.locality.clone(),
                purpose: "query".into(),
                path: None,
                line_range: None,
                data_level: 0,
                view_kind: "query".into(),
                bytes_selected: query_text.len(),
                text_sha: query_text_sha,
                secret_scan: decision.secret_scan,
                policy_decision: "block".into(),
                cache_key,
                outbound_attempted: false,
                reason: Some(decision.reason.clone()),
            };
            let _ = audit::append_audit_event(repo_root, &audit_event);

            return Ok(SearchResult {
                hits: Vec::new(),
                skipped: 0,
                blocked: true,
                reason: Some(decision.reason),
            });
        }

        let query_vector = match provider.embed(&query_text, &query_text_sha) {
            Ok(v) => v,
            Err(e) => {
                // Write audit event for provider unavailable
                let cache_key = compute_cache_key(
                    &metadata.provider_id,
                    &metadata.model_id,
                    metadata.dimensions,
                    "query",
                    &query_text_sha,
                    "",
                    "local_only",
                    0,
                );
                let audit_event = EmbeddingAuditEvent {
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    event: "provider_unavailable".into(),
                    request_id,
                    provider_id: metadata.provider_id.clone(),
                    model_id: metadata.model_id.clone(),
                    locality: metadata.locality.clone(),
                    purpose: "query".into(),
                    path: None,
                    line_range: None,
                    data_level: 0,
                    view_kind: "query".into(),
                    bytes_selected: 0,
                    text_sha: query_text_sha,
                    secret_scan: decision.secret_scan,
                    policy_decision: "provider_error".into(),
                    cache_key,
                    outbound_attempted: false,
                    reason: Some(e.to_string()),
                };
                let _ = audit::append_audit_event(repo_root, &audit_event);

                return Ok(SearchResult {
                    hits: Vec::new(),
                    skipped: 0,
                    blocked: false,
                    reason: Some(format!("provider error: {}", e)),
                });
            }
        };

        // Write audit allow event for query
        let cache_key = compute_cache_key(
            &metadata.provider_id,
            &metadata.model_id,
            metadata.dimensions,
            "query",
            &query_text_sha,
            "",
            "local_only",
            0,
        );
        let audit_event = EmbeddingAuditEvent {
            timestamp: chrono::Utc::now().to_rfc3339(),
            event: "query_embed".into(),
            request_id,
            provider_id: metadata.provider_id.clone(),
            model_id: metadata.model_id.clone(),
            locality: metadata.locality.clone(),
            purpose: "query".into(),
            path: None,
            line_range: None,
            data_level: 0,
            view_kind: "query".into(),
            bytes_selected: query_text.len(),
            text_sha: query_text_sha,
            secret_scan: decision.secret_scan,
            policy_decision: "allow".into(),
            cache_key,
            outbound_attempted: false,
            reason: None,
        };
        let _ = audit::append_audit_event(repo_root, &audit_event);

        // Compute cosine similarity and rank
        let mut scored: Vec<(f64, &EmbeddingRecord)> = Vec::new();
        for record in &records {
            let score = cosine_similarity(&query_vector, &record.vector);
            scored.push((score, record));
        }
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        let mut hits = Vec::new();
        let skipped = 0usize;
        for (score, record) in scored.iter().take(limit) {
            hits.push(StoreHit {
                path: record.path.clone(),
                start_line: record.start_line,
                end_line: record.end_line,
                content_sha: record.source_content_sha.clone(),
                score: *score,
                source: openlocus_store::StoreSource::External("dense_mock".into()),
                language: record.language.clone(),
                symbol_name: None,
            });
        }

        Ok(SearchResult {
            hits,
            skipped,
            blocked: false,
            reason: None,
        })
    }

    /// List all embedding records.
    pub fn list(repo_root: &Path) -> Result<Vec<EmbeddingRecord>> {
        let store_path = repo_root.join(STORE_RELATIVE_PATH);
        if !store_path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(&store_path)?;
        let mut records = Vec::new();
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<EmbeddingRecord>(line) {
                Ok(r) => records.push(r),
                Err(_) => continue,
            }
        }
        Ok(records)
    }

    /// Purge the embedding store.
    pub fn purge(repo_root: &Path) -> Result<usize> {
        let store_path = repo_root.join(STORE_RELATIVE_PATH);
        if !store_path.exists() {
            return Ok(0);
        }
        let records = Self::list(repo_root)?;
        let count = records.len();
        fs::remove_file(&store_path)?;
        Ok(count)
    }
}

/// Compute cosine similarity between two vectors.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f64 = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| (*x as f64) * (*y as f64))
        .sum();
    let norm_a: f64 = a.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    let norm_b: f64 = b.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

/// Result of building the embedding store.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BuildResult {
    pub record_count: usize,
    pub skipped: usize,
    pub blocked: usize,
}

/// Result of searching the embedding store.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub hits: Vec<StoreHit>,
    pub skipped: usize,
    pub blocked: bool,
    pub reason: Option<String>,
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockEmbeddingProvider;

    #[test]
    fn cosine_similarity_identical() {
        let v = vec![1.0f32, 0.0, 0.0];
        let sim = cosine_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 0.001);
    }

    #[test]
    fn cosine_similarity_orthogonal() {
        let a = vec![1.0f32, 0.0];
        let b = vec![0.0f32, 1.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 0.001);
    }

    #[test]
    fn cosine_similarity_zero_vector() {
        let a = vec![0.0f32, 0.0];
        let b = vec![1.0f32, 1.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 0.001);
    }

    #[test]
    fn cosine_similarity_len_mismatch() {
        let a = vec![1.0f32];
        let b = vec![1.0f32, 2.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 0.001);
    }

    #[test]
    fn cosine_similarity_empty() {
        let a: Vec<f32> = vec![];
        let b: Vec<f32> = vec![];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 0.001);
    }

    #[test]
    fn build_and_search() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("lib.rs"), "fn hello() {}\nfn world() {}\n").unwrap();

        let records = vec![FileRecord {
            path: "lib.rs".into(),
            size: 28,
            content_sha: blake3::hash(b"fn hello() {}\nfn world() {}\n")
                .to_hex()
                .to_string(),
            language: "rust".into(),
        }];

        let provider = MockEmbeddingProvider::new();
        let metadata = provider.metadata().clone();
        let policy = openlocus_core::Policy::default();

        let result =
            JsonlEmbeddingStore::build(root, &records, &provider, &metadata, &policy).unwrap();
        assert_eq!(result.record_count, 1);
        assert_eq!(result.skipped, 0);

        // Search
        let search_result =
            JsonlEmbeddingStore::search(root, "hello world", &provider, &metadata, &policy, 10)
                .unwrap();
        assert!(!search_result.blocked);
        assert_eq!(search_result.hits.len(), 1);
        assert_eq!(search_result.hits[0].path, "lib.rs");
    }

    #[test]
    fn search_orders_by_score() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("a.rs"), "fn alpha() {}\n").unwrap();
        std::fs::write(root.join("b.rs"), "fn beta() {}\n").unwrap();

        let sha_a = blake3::hash(b"fn alpha() {}\n").to_hex().to_string();
        let sha_b = blake3::hash(b"fn beta() {}\n").to_hex().to_string();

        let records = vec![
            FileRecord {
                path: "a.rs".into(),
                size: 15,
                content_sha: sha_a,
                language: "rust".into(),
            },
            FileRecord {
                path: "b.rs".into(),
                size: 14,
                content_sha: sha_b,
                language: "rust".into(),
            },
        ];

        let provider = MockEmbeddingProvider::new();
        let metadata = provider.metadata().clone();
        let policy = openlocus_core::Policy::default();

        JsonlEmbeddingStore::build(root, &records, &provider, &metadata, &policy).unwrap();

        let search_result =
            JsonlEmbeddingStore::search(root, "alpha", &provider, &metadata, &policy, 10).unwrap();
        // Results should be sorted by score descending
        for i in 1..search_result.hits.len() {
            assert!(search_result.hits[i - 1].score >= search_result.hits[i].score);
        }
    }

    #[test]
    fn store_no_raw_text() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("lib.rs"), "fn hello() {}\n").unwrap();

        let records = vec![FileRecord {
            path: "lib.rs".into(),
            size: 15,
            content_sha: blake3::hash(b"fn hello() {}\n").to_hex().to_string(),
            language: "rust".into(),
        }];

        let provider = MockEmbeddingProvider::new();
        let metadata = provider.metadata().clone();
        let policy = openlocus_core::Policy::default();

        JsonlEmbeddingStore::build(root, &records, &provider, &metadata, &policy).unwrap();

        // Read raw JSONL and verify no "text" field (only text_sha)
        let raw = fs::read_to_string(root.join(STORE_RELATIVE_PATH)).unwrap();
        let json: serde_json::Value = serde_json::from_str(raw.trim()).unwrap();
        assert!(
            json.get("text").is_none(),
            "store should not contain raw text field"
        );
        assert!(
            json.get("text_sha").is_some(),
            "store should contain text_sha"
        );
    }

    #[test]
    fn purge_removes_store() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("lib.rs"), "fn test() {}\n").unwrap();

        let records = vec![FileRecord {
            path: "lib.rs".into(),
            size: 14,
            content_sha: blake3::hash(b"fn test() {}\n").to_hex().to_string(),
            language: "rust".into(),
        }];

        let provider = MockEmbeddingProvider::new();
        let metadata = provider.metadata().clone();
        let policy = openlocus_core::Policy::default();

        JsonlEmbeddingStore::build(root, &records, &provider, &metadata, &policy).unwrap();
        let count = JsonlEmbeddingStore::purge(root).unwrap();
        assert_eq!(count, 1);
        assert!(!root.join(STORE_RELATIVE_PATH).exists());
    }

    #[test]
    fn search_missing_store_returns_graceful() {
        let dir = tempfile::tempdir().unwrap();
        let provider = MockEmbeddingProvider::new();
        let metadata = provider.metadata().clone();
        let policy = openlocus_core::Policy::default();

        let result =
            JsonlEmbeddingStore::search(dir.path(), "test", &provider, &metadata, &policy, 10)
                .unwrap();
        assert!(!result.blocked);
        assert!(result.reason.is_some());
        assert!(result.hits.is_empty());
    }

    #[test]
    fn build_skips_empty_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("empty.rs"), "").unwrap();
        std::fs::write(root.join("lib.rs"), "fn test() {}\n").unwrap();

        let sha_lib = blake3::hash(b"fn test() {}\n").to_hex().to_string();

        let records = vec![
            FileRecord {
                path: "empty.rs".into(),
                size: 0,
                content_sha: blake3::hash(b"").to_hex().to_string(),
                language: "rust".into(),
            },
            FileRecord {
                path: "lib.rs".into(),
                size: 14,
                content_sha: sha_lib,
                language: "rust".into(),
            },
        ];

        let provider = MockEmbeddingProvider::new();
        let metadata = provider.metadata().clone();
        let policy = openlocus_core::Policy::default();

        let result =
            JsonlEmbeddingStore::build(root, &records, &provider, &metadata, &policy).unwrap();
        assert_eq!(result.record_count, 1);
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn short_file_range_does_not_exceed_total_lines() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        // 2-line file: end_line should be min(2, 8) = 2, not 8
        std::fs::write(root.join("short.rs"), "fn a() {}\nfn b() {}\n").unwrap();

        let records = vec![FileRecord {
            path: "short.rs".into(),
            size: 22,
            content_sha: blake3::hash(b"fn a() {}\nfn b() {}\n").to_hex().to_string(),
            language: "rust".into(),
        }];

        let provider = MockEmbeddingProvider::new();
        let metadata = provider.metadata().clone();
        let policy = openlocus_core::Policy::default();

        let result =
            JsonlEmbeddingStore::build(root, &records, &provider, &metadata, &policy).unwrap();
        assert_eq!(result.record_count, 1);

        // Read the stored record and verify end_line <= total_lines
        let records = JsonlEmbeddingStore::list(root).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].start_line, 1);
        assert_eq!(
            records[0].end_line, 2,
            "end_line should be min(total_lines, 8) = 2, not 8"
        );
    }

    #[test]
    fn single_line_file_range_is_1_to_1() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        // 1-line file: end_line should be 1
        std::fs::write(root.join("one.rs"), "fn single() {}\n").unwrap();

        let records = vec![FileRecord {
            path: "one.rs".into(),
            size: 16,
            content_sha: blake3::hash(b"fn single() {}\n").to_hex().to_string(),
            language: "rust".into(),
        }];

        let provider = MockEmbeddingProvider::new();
        let metadata = provider.metadata().clone();
        let policy = openlocus_core::Policy::default();

        let result =
            JsonlEmbeddingStore::build(root, &records, &provider, &metadata, &policy).unwrap();
        assert_eq!(result.record_count, 1);

        let records = JsonlEmbeddingStore::list(root).unwrap();
        assert_eq!(records[0].start_line, 1);
        assert_eq!(records[0].end_line, 1, "1-line file should have end_line=1");
    }

    #[test]
    fn query_embed_audit_event_not_cache_hit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        // Need a file with enough lines
        let content = (0..10)
            .map(|i| format!("fn line{}() {{}}", i))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(root.join("lib.rs"), &content).unwrap();

        let records = vec![FileRecord {
            path: "lib.rs".into(),
            size: content.len() as u64,
            content_sha: blake3::hash(content.as_bytes()).to_hex().to_string(),
            language: "rust".into(),
        }];

        let provider = MockEmbeddingProvider::new();
        let metadata = provider.metadata().clone();
        let policy = openlocus_core::Policy::default();

        JsonlEmbeddingStore::build(root, &records, &provider, &metadata, &policy).unwrap();
        JsonlEmbeddingStore::search(root, "hello", &provider, &metadata, &policy, 10).unwrap();

        // Read audit events and verify no "cache_hit" events
        let audit_events = audit::read_audit_events(root).unwrap();
        for event in &audit_events {
            assert_ne!(
                event.event, "cache_hit",
                "audit event should not be 'cache_hit'; use 'query_embed' for query embedding"
            );
        }
        // Verify query event uses "query_embed"
        let query_events: Vec<_> = audit_events
            .iter()
            .filter(|e| e.purpose == "query" && e.event == "query_embed")
            .collect();
        assert!(
            !query_events.is_empty(),
            "should have at least one 'query_embed' audit event for query"
        );
    }

    #[test]
    fn audit_no_raw_query_text() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let content = (0..10)
            .map(|i| format!("fn line{}() {{}}", i))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(root.join("lib.rs"), &content).unwrap();

        let records = vec![FileRecord {
            path: "lib.rs".into(),
            size: content.len() as u64,
            content_sha: blake3::hash(content.as_bytes()).to_hex().to_string(),
            language: "rust".into(),
        }];

        let provider = MockEmbeddingProvider::new();
        let metadata = provider.metadata().clone();
        let policy = openlocus_core::Policy::default();

        JsonlEmbeddingStore::build(root, &records, &provider, &metadata, &policy).unwrap();
        JsonlEmbeddingStore::search(root, "my test query", &provider, &metadata, &policy, 10)
            .unwrap();

        // Read raw audit file and verify no raw query text
        let raw = fs::read_to_string(root.join(audit::AUDIT_RELATIVE_PATH)).unwrap();
        assert!(
            !raw.contains("my test query"),
            "audit should not contain raw query text"
        );
    }
}
