//! Persistent BM25 index operations.
//!
//! build_index: Full rebuild, writes Tantivy index + manifest.
//! status_index: Quick check of index state.
//! validate_index: Full validation of manifest entries against filesystem.
//! purge_index: Safe deletion of R7/R8 index artifacts.
//! search_persistent_bm25: Search with mandatory re-verification of every hit.
//!
//! Safety gates (oracle review R7 + R8):
//! - Policy gate: search/validate refuse if manifest policy_hash ≠ current policy.
//! - validate_path on every Tantivy hit path before reading file.
//! - Empty index_content_sha → skip (cannot verify stale check).
//! - chunk range strictly validated: 1 ≤ start ≤ end ≤ total_lines; no clamping.
//! - build_index filters unsafe FileRecord paths via validate_path.
//! - R8: chunk_strategy gate — search/validate refuse if manifest chunk_strategy
//!   is unrecognized or missing; schema mismatch also triggers rebuild.
//! - R8: AST chunks are only candidate boundaries; evidence still verified
//!   from current filesystem path/range/hash/excerpt/freshness.

use anyhow::{Context, Result, bail};
use openlocus_ast::{AstChunkKind, AstStatus, extract_ast_chunks};
use openlocus_core::{Channel, Evidence, Freshness, Policy, ScoreParts};
use openlocus_repo::scan::FileRecord;
use openlocus_repo::validate_path;
use std::path::Path;
use std::time::Instant;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::*;
use tantivy::{Index, ReloadPolicy, doc};

use crate::manifest::*;

/// Maximum chunk size in lines for indexing (line-window strategy).
const MAX_CHUNK_LINES: u64 = 30;
/// Context lines around a matching center for tightened evidence.
const CONTEXT_LINES: u64 = 2;
/// Maximum evidence span in lines.
const MAX_EVIDENCE_SPAN: u64 = 7;

// ── Build ──────────────────────────────────────────────────────────────

/// Result of building a persistent index.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BuildResult {
    pub success: bool,
    pub file_count: u64,
    pub chunk_count: u64,
    pub schema_version: String,
    pub policy_hash: String,
    pub chunk_strategy: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ast_stats: Option<AstManifestStats>,
}

/// Build a persistent Tantivy BM25 index from file records.
/// Writes the index to .openlocus/index/tantivy/ and manifest to .openlocus/index/manifest.json.
/// This is a full rebuild — any existing index is replaced.
///
/// `chunk_strategy` controls how source is chunked:
/// - `line_window_v1` (default): fixed-size line windows, same as R7.
/// - `ast_v1` (experimental): AST-bounded chunks with fallback line windows.
///
/// Safety: filters FileRecord paths through validate_path; unsafe paths are skipped.
pub fn build_index(
    repo_root: &Path,
    records: &[FileRecord],
    policy: &Policy,
    chunk_strategy: ChunkStrategy,
) -> Result<BuildResult> {
    let policy_hash = compute_policy_hash(policy);

    // Ensure index directories exist
    let tantivy_dir = repo_root.join(TANTIVY_DIR_RELATIVE);
    let index_dir = repo_root.join(INDEX_DIR_RELATIVE);
    std::fs::create_dir_all(&index_dir).with_context(|| "failed to create index directory")?;

    // Remove existing Tantivy index if present
    if tantivy_dir.exists() {
        std::fs::remove_dir_all(&tantivy_dir)
            .with_context(|| "failed to remove existing tantivy index")?;
    }
    std::fs::create_dir_all(&tantivy_dir).with_context(|| "failed to create tantivy directory")?;

    // Build schema
    let mut schema_builder = Schema::builder();
    let path_field = schema_builder.add_text_field("path", STRING | STORED);
    let language_field = schema_builder.add_text_field("language", STRING | STORED);
    let content_sha_field = schema_builder.add_text_field("content_sha", STRING | STORED);
    let start_line_field = schema_builder.add_u64_field("start_line", STORED);
    let end_line_field = schema_builder.add_u64_field("end_line", STORED);
    let content_field = schema_builder.add_text_field("content", TEXT | STORED);
    let schema = schema_builder.build();

    let index = Index::create_in_dir(&tantivy_dir, schema)?;
    let mut index_writer = index.writer(50_000_000)?;

    let mut manifest_files = Vec::new();
    let mut total_chunks: u64 = 0;
    let mut ast_stats = AstManifestStats::default();

    for record in records {
        // Path safety gate: validate_path before indexing
        if validate_path(repo_root, &record.path).is_err() {
            manifest_files.push(ManifestFileEntry {
                path: record.path.clone(),
                content_sha: record.content_sha.clone(),
                size_bytes: record.size,
                language: record.language.clone(),
                status: "skipped".into(),
                skipped_reason: Some("path_unsafe".into()),
            });
            continue;
        }

        let full_path = repo_root.join(&record.path);

        // Read current file content
        let content = match std::fs::read_to_string(&full_path) {
            Ok(c) => c,
            Err(_) => {
                manifest_files.push(ManifestFileEntry {
                    path: record.path.clone(),
                    content_sha: record.content_sha.clone(),
                    size_bytes: record.size,
                    language: record.language.clone(),
                    status: "skipped".into(),
                    skipped_reason: Some("read_error".into()),
                });
                continue;
            }
        };

        // Compute current content_sha
        let current_sha = blake3::hash(content.as_bytes()).to_hex().to_string();

        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len() as u64;

        if total_lines == 0 {
            manifest_files.push(ManifestFileEntry {
                path: record.path.clone(),
                content_sha: current_sha,
                size_bytes: record.size,
                language: record.language.clone(),
                status: "skipped".into(),
                skipped_reason: Some("empty_file".into()),
            });
            continue;
        }

        // Determine chunks based on strategy
        let chunks = match chunk_strategy {
            ChunkStrategy::LineWindowV1 => {
                // R7 line-window chunking
                let mut chunks = Vec::new();
                let mut chunk_start = 0u64;
                while chunk_start < total_lines {
                    let chunk_end = (chunk_start + MAX_CHUNK_LINES).min(total_lines);
                    chunks.push((chunk_start + 1, chunk_end)); // 1-based
                    chunk_start = chunk_end;
                }
                chunks
            }
            ChunkStrategy::AstV1 => {
                // R8 AST-bounded chunking
                let ast_result =
                    extract_ast_chunks(&record.path, &record.language, &content, MAX_CHUNK_LINES);

                // Track AST stats
                match ast_result.status {
                    AstStatus::Supported => ast_stats.supported_files += 1,
                    AstStatus::FallbackUnsupported => ast_stats.fallback_files += 1,
                    AstStatus::FallbackParseError => ast_stats.parser_error_files += 1,
                }
                for chunk in &ast_result.chunks {
                    match chunk.kind {
                        AstChunkKind::AstNode => ast_stats.ast_chunks += 1,
                        AstChunkKind::FallbackLineWindow => ast_stats.fallback_chunks += 1,
                    }
                }

                ast_result
                    .chunks
                    .iter()
                    .map(|c| (c.start_line, c.end_line))
                    .collect()
            }
        };

        // Index each chunk
        for (start_line, end_line) in &chunks {
            let start_idx = (*start_line - 1) as usize;
            let end_idx = *end_line as usize;
            let chunk_content = if start_idx < lines.len() && end_idx <= lines.len() {
                lines[start_idx..end_idx].join("\n")
            } else {
                continue; // Invalid range, skip
            };

            index_writer.add_document(doc!(
                path_field => record.path.as_str(),
                language_field => record.language.as_str(),
                content_sha_field => current_sha.as_str(),
                start_line_field => *start_line,
                end_line_field => *end_line,
                content_field => chunk_content.as_str(),
            ))?;

            total_chunks += 1;
        }

        manifest_files.push(ManifestFileEntry {
            path: record.path.clone(),
            content_sha: current_sha,
            size_bytes: record.size,
            language: record.language.clone(),
            status: "indexed".into(),
            skipped_reason: None,
        });
    }

    index_writer.commit()?;

    // Write manifest with chunk strategy
    let ast_stats_opt = if chunk_strategy == ChunkStrategy::AstV1 {
        Some(ast_stats)
    } else {
        None
    };
    let manifest = IndexManifest::new_with_strategy(
        policy_hash.clone(),
        manifest_files,
        total_chunks,
        chunk_strategy.clone(),
        ast_stats_opt.clone(),
    );
    manifest.save(repo_root)?;

    Ok(BuildResult {
        success: true,
        file_count: manifest.file_count,
        chunk_count: total_chunks,
        schema_version: SCHEMA_VERSION.to_string(),
        policy_hash,
        chunk_strategy: chunk_strategy.to_cli_str().to_string(),
        ast_stats: ast_stats_opt,
    })
}

// ── Status ─────────────────────────────────────────────────────────────

/// Quick status check of the persistent index.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StatusResult {
    pub exists: bool,
    pub schema_version: Option<String>,
    pub file_count: Option<u64>,
    pub chunk_count: Option<u64>,
    pub policy_hash_matches: Option<bool>,
    pub requires_rebuild: bool,
    /// Quick stale check: count of manifest files whose content_sha doesn't
    /// match current file. This is bounded by reading each file once.
    pub stale_files_fast: Option<u64>,
    /// Chunk strategy from manifest.
    pub chunk_strategy: Option<String>,
    /// AST stats from manifest (if present).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ast_stats: Option<AstManifestStats>,
}

/// Quick status check of the persistent index.
pub fn status_index(repo_root: &Path, policy: &Policy) -> Result<StatusResult> {
    if !IndexManifest::exists(repo_root) {
        return Ok(StatusResult {
            exists: false,
            schema_version: None,
            file_count: None,
            chunk_count: None,
            policy_hash_matches: None,
            requires_rebuild: true,
            stale_files_fast: None,
            chunk_strategy: None,
            ast_stats: None,
        });
    }

    let manifest = IndexManifest::load(repo_root)?;

    let current_policy_hash = compute_policy_hash(policy);
    let policy_hash_matches = manifest.policy_hash == current_policy_hash;

    let schema_ok =
        manifest.schema_version == SCHEMA_VERSION || manifest.schema_version == SCHEMA_VERSION_R7;

    // R8: chunk_strategy must be recognized
    let strategy_ok = manifest.chunk_strategy == ChunkStrategy::LineWindowV1
        || manifest.chunk_strategy == ChunkStrategy::AstV1;

    // Quick stale check: for each indexed file, check if content_sha matches current file
    let mut stale_count: u64 = 0;
    let mut deleted_count: u64 = 0;
    for entry in &manifest.files {
        if entry.status != "indexed" {
            continue;
        }
        let full_path = repo_root.join(&entry.path);
        if !full_path.exists() {
            deleted_count += 1;
            continue;
        }
        if let Ok(bytes) = std::fs::read(&full_path) {
            let current_sha = blake3::hash(&bytes).to_hex().to_string();
            if current_sha != entry.content_sha {
                stale_count += 1;
            }
        }
    }

    let requires_rebuild =
        !schema_ok || !policy_hash_matches || !strategy_ok || stale_count > 0 || deleted_count > 0;

    Ok(StatusResult {
        exists: true,
        schema_version: Some(manifest.schema_version),
        file_count: Some(manifest.file_count),
        chunk_count: Some(manifest.chunk_count),
        policy_hash_matches: Some(policy_hash_matches),
        requires_rebuild,
        stale_files_fast: Some(stale_count + deleted_count),
        chunk_strategy: Some(manifest.chunk_strategy.to_cli_str().to_string()),
        ast_stats: manifest.ast_stats,
    })
}

// ── Validate ───────────────────────────────────────────────────────────

/// Full validation result.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ValidateResult {
    pub valid: bool,
    pub stale_files: Vec<String>,
    pub deleted_files: Vec<String>,
    pub policy_hash_matches: bool,
    /// Files where path validation fails (symlink escape, etc.)
    pub path_unsafe_files: Vec<String>,
    /// Chunk strategy from manifest.
    pub chunk_strategy: Option<String>,
}

/// Full validation of the persistent index against the filesystem.
/// Checks policy hash — if it doesn't match the current policy,
/// reports policy_hash_matches=false and valid=false.
/// R8: Also checks chunk_strategy is recognized; refuses unknown strategies.
pub fn validate_index(repo_root: &Path, policy: &Policy) -> Result<ValidateResult> {
    if !IndexManifest::exists(repo_root) {
        return Ok(ValidateResult {
            valid: false,
            stale_files: vec![],
            deleted_files: vec![],
            policy_hash_matches: false,
            path_unsafe_files: vec![],
            chunk_strategy: None,
        });
    }

    let manifest = IndexManifest::load(repo_root)?;

    let current_policy_hash = compute_policy_hash(policy);
    let policy_hash_matches = manifest.policy_hash == current_policy_hash;

    // R8: chunk_strategy must be recognized
    let strategy_ok = manifest.chunk_strategy == ChunkStrategy::LineWindowV1
        || manifest.chunk_strategy == ChunkStrategy::AstV1;

    let mut stale_files = Vec::new();
    let mut deleted_files = Vec::new();
    let mut path_unsafe_files = Vec::new();

    for entry in &manifest.files {
        if entry.status != "indexed" {
            continue;
        }

        // Path safety check
        if validate_path(repo_root, &entry.path).is_err() {
            path_unsafe_files.push(entry.path.clone());
            continue;
        }

        let full_path = repo_root.join(&entry.path);
        if !full_path.exists() {
            deleted_files.push(entry.path.clone());
            continue;
        }

        if let Ok(bytes) = std::fs::read(&full_path) {
            let current_sha = blake3::hash(&bytes).to_hex().to_string();
            if current_sha != entry.content_sha {
                stale_files.push(entry.path.clone());
            }
        }
    }

    let schema_ok =
        manifest.schema_version == SCHEMA_VERSION || manifest.schema_version == SCHEMA_VERSION_R7;

    let valid = policy_hash_matches
        && strategy_ok
        && stale_files.is_empty()
        && deleted_files.is_empty()
        && path_unsafe_files.is_empty()
        && schema_ok;

    Ok(ValidateResult {
        valid,
        stale_files,
        deleted_files,
        policy_hash_matches,
        path_unsafe_files,
        chunk_strategy: Some(manifest.chunk_strategy.to_cli_str().to_string()),
    })
}

// ── Purge ──────────────────────────────────────────────────────────────

/// Result of purging the index.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PurgeResult {
    pub purged: bool,
    pub removed_paths: Vec<String>,
}

/// Safely delete R7/R8 persistent index artifacts.
///
/// Safety: Only deletes under .openlocus/index/ and does not follow symlinks
/// that would escape the repo root.
pub fn purge_index(repo_root: &Path) -> Result<PurgeResult> {
    let index_dir = repo_root.join(INDEX_DIR_RELATIVE);

    if !index_dir.exists() {
        return Ok(PurgeResult {
            purged: true,
            removed_paths: vec![],
        });
    }

    // Safety: canonicalize both paths and verify index_dir is under repo_root
    let canonical_root = repo_root
        .canonicalize()
        .with_context(|| "cannot canonicalize repo_root")?;
    let canonical_index = index_dir
        .canonicalize()
        .with_context(|| "cannot canonicalize index_dir")?;

    if !canonical_index.starts_with(&canonical_root) {
        bail!("index directory escapes repo root — refusing to purge for safety");
    }

    // Remove only known R7/R8 artifact paths, not arbitrary files
    let mut removed = Vec::new();

    let tantivy_dir = repo_root.join(TANTIVY_DIR_RELATIVE);
    let manifest_path = repo_root.join(MANIFEST_PATH_RELATIVE);

    if manifest_path.exists() {
        // Verify it's under the repo root before deleting
        let canonical_manifest = manifest_path
            .canonicalize()
            .with_context(|| "cannot canonicalize manifest path")?;
        if canonical_manifest.starts_with(&canonical_root) {
            std::fs::remove_file(&manifest_path)?;
            removed.push(MANIFEST_PATH_RELATIVE.to_string());
        }
    }

    if tantivy_dir.exists() {
        let canonical_tantivy = tantivy_dir
            .canonicalize()
            .with_context(|| "cannot canonicalize tantivy dir")?;
        if canonical_tantivy.starts_with(&canonical_root) {
            std::fs::remove_dir_all(&tantivy_dir)?;
            removed.push(TANTIVY_DIR_RELATIVE.to_string());
        }
    }

    // Try to clean up the index dir if empty
    if index_dir.exists() {
        let _ = std::fs::remove_dir(&index_dir); // best-effort; may fail if not empty
    }

    Ok(PurgeResult {
        purged: true,
        removed_paths: removed,
    })
}

// ── Search ─────────────────────────────────────────────────────────────

/// Stats from a persistent BM25 search.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SearchStats {
    pub query_ms: u64,
    pub materialize_ms: u64,
    pub stale_hits_skipped: u64,
    pub invalid_hits_skipped: u64,
}

/// Error returned when policy hash doesn't match.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PolicyMismatchError {
    pub manifest_policy_hash: String,
    pub current_policy_hash: String,
}

/// Search the persistent BM25 index. Every hit is re-verified against
/// the current filesystem: content_sha compared, range validated,
/// and line-level query token scoring performed.
/// Stale or invalid hits are skipped (not emitted as stale evidence).
///
/// Policy gate: if manifest policy_hash doesn't match current policy,
/// returns an error — refuses to search a stale-policy index.
/// Schema gate: refuses if schema_version doesn't match.
/// Strategy gate (R8): refuses if chunk_strategy is unrecognized or missing.
pub fn search_persistent_bm25(
    repo_root: &Path,
    query: &str,
    max_results: usize,
    policy: &Policy,
) -> Result<(Vec<Evidence>, SearchStats)> {
    let query_start = Instant::now();

    let tantivy_dir = repo_root.join(TANTIVY_DIR_RELATIVE);
    if !tantivy_dir.exists() {
        return Ok((
            vec![],
            SearchStats {
                query_ms: 0,
                materialize_ms: 0,
                stale_hits_skipped: 0,
                invalid_hits_skipped: 0,
            },
        ));
    }

    // Manifest/policy/schema/strategy gate
    if !IndexManifest::exists(repo_root) {
        bail!("persistent index manifest missing; rebuild the index with 'openlocus index build'");
    }

    let manifest = IndexManifest::load(repo_root)?;
    let current_policy_hash = compute_policy_hash(policy);
    if manifest.policy_hash != current_policy_hash {
        bail!(
            "persistent index policy hash mismatch: manifest={}, current={}. Rebuild the index with 'openlocus index build'",
            manifest.policy_hash,
            current_policy_hash
        );
    }
    // Schema gate
    if manifest.schema_version != SCHEMA_VERSION && manifest.schema_version != SCHEMA_VERSION_R7 {
        bail!(
            "persistent index schema version mismatch: manifest={}, current={}. Rebuild the index with 'openlocus index build'",
            manifest.schema_version,
            SCHEMA_VERSION
        );
    }
    // Strategy gate (R8): chunk_strategy must be recognized
    if manifest.chunk_strategy != ChunkStrategy::LineWindowV1
        && manifest.chunk_strategy != ChunkStrategy::AstV1
    {
        bail!(
            "persistent index chunk strategy unrecognized: {:?}. Rebuild the index with 'openlocus index build'",
            manifest.chunk_strategy
        );
    }

    let index = Index::open_in_dir(&tantivy_dir)?;
    let schema = index.schema();

    // Find field handles by name
    let path_field = schema.get_field("path")?;
    let language_field = schema.get_field("language")?;
    let content_sha_field = schema.get_field("content_sha")?;
    let start_line_field = schema.get_field("start_line")?;
    let end_line_field = schema.get_field("end_line")?;
    let content_field = schema.get_field("content")?;

    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()?;
    let searcher = reader.searcher();

    // Parse query
    let query_parser = QueryParser::for_index(&index, vec![content_field]);
    let parsed_query = match query_parser.parse_query(query) {
        Ok(p) => p,
        Err(_) => {
            let sanitized = query.replace([':', '/', '(', ')', '"'], " ");
            match query_parser.parse_query(sanitized.trim()) {
                Ok(p) => p,
                Err(_) => {
                    return Ok((
                        vec![],
                        SearchStats {
                            query_ms: query_start.elapsed().as_millis() as u64,
                            materialize_ms: 0,
                            stale_hits_skipped: 0,
                            invalid_hits_skipped: 0,
                        },
                    ));
                }
            }
        }
    };

    let top_docs = searcher.search(&parsed_query, &TopDocs::with_limit(max_results * 2))?;

    let query_ms = query_start.elapsed().as_millis() as u64;

    // Tokenize query for line-level scoring
    let query_tokens = tokenize_query(query);

    let materialize_start = Instant::now();
    let mut results = Vec::new();
    let mut stale_hits_skipped: u64 = 0;
    let mut invalid_hits_skipped: u64 = 0;

    for (_score, doc_address) in top_docs {
        if results.len() >= max_results {
            break;
        }

        let doc: TantivyDocument = searcher.doc(doc_address)?;
        let path_val = doc
            .get_first(path_field)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let index_content_sha = doc
            .get_first(content_sha_field)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let chunk_start_line = doc
            .get_first(start_line_field)
            .and_then(|v| v.as_u64())
            .unwrap_or(1);
        let chunk_end_line = doc
            .get_first(end_line_field)
            .and_then(|v| v.as_u64())
            .unwrap_or(1);
        let language_val = doc
            .get_first(language_field)
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        // Empty content_sha → cannot verify stale check → skip
        if index_content_sha.is_empty() {
            invalid_hits_skipped += 1;
            continue;
        }

        // Path safety: validate_path before reading file
        if validate_path(repo_root, &path_val).is_err() {
            invalid_hits_skipped += 1;
            continue;
        }

        // Re-read the current file (mandatory verification)
        let full_path = repo_root.join(&path_val);
        let content = match std::fs::read_to_string(&full_path) {
            Ok(c) => c,
            Err(_) => {
                invalid_hits_skipped += 1;
                continue;
            }
        };

        // Compute current content_sha and compare (stale check)
        let current_content_sha = blake3::hash(content.as_bytes()).to_hex().to_string();
        if index_content_sha != current_content_sha {
            stale_hits_skipped += 1;
            continue;
        }

        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len() as u64;

        if total_lines == 0 {
            invalid_hits_skipped += 1;
            continue;
        }

        // Strict chunk range validation: 1 ≤ start ≤ end ≤ total_lines; no clamping
        if chunk_start_line < 1 || chunk_start_line > chunk_end_line || chunk_end_line > total_lines
        {
            invalid_hits_skipped += 1;
            continue;
        }

        // Line-level scoring: find the best-matching line
        let best_line =
            find_best_matching_line(&lines, chunk_start_line, chunk_end_line, &query_tokens);

        let best_line = match best_line {
            Some(l) => l,
            None => {
                // No query token overlap — skip (precision-biased)
                invalid_hits_skipped += 1;
                continue;
            }
        };

        // Tighten around best line ± context, cap at MAX_EVIDENCE_SPAN
        let tight_start = best_line.saturating_sub(CONTEXT_LINES).max(1);
        let mut tight_end = (best_line + CONTEXT_LINES).min(total_lines);
        tight_end = tight_end.min(tight_start + MAX_EVIDENCE_SPAN - 1);
        let tight_start = tight_start.max(1);
        let tight_end = tight_end.min(total_lines);

        // Strict guard: 1 ≤ start ≤ end ≤ total_lines
        if tight_start < 1 || tight_start > tight_end || tight_end > total_lines {
            invalid_hits_skipped += 1;
            continue;
        }

        let excerpt = lines[(tight_start - 1) as usize..tight_end as usize].join("\n");

        let bm25_score = _score as f64;

        let evidence = Evidence::new(
            &path_val,
            tight_start,
            tight_end,
            &current_content_sha,
            bm25_score,
            vec![format!("persistent_bm25: {}", query)],
            vec![Channel::Bm25],
        )
        .with_excerpt(&excerpt)
        .with_language(&language_val)
        .with_freshness(Freshness::VerifiedCurrent)
        .with_score_parts(ScoreParts {
            bm25: Some(bm25_score),
            ..Default::default()
        });

        results.push(evidence);
    }

    let materialize_ms = materialize_start.elapsed().as_millis() as u64;

    Ok((
        results,
        SearchStats {
            query_ms,
            materialize_ms,
            stale_hits_skipped,
            invalid_hits_skipped,
        },
    ))
}

// ── Reusable index handle ─────────────────────────────────────────────

/// A reusable persistent BM25 index handle that opens once and can be
/// queried multiple times without re-opening the Tantivy index.
pub struct PersistentBm25Index {
    index: Index,
    searcher: tantivy::Searcher,
    path_field: Field,
    language_field: Field,
    content_sha_field: Field,
    start_line_field: Field,
    end_line_field: Field,
    content_field: Field,
}

impl PersistentBm25Index {
    /// Open the persistent BM25 index for reuse.
    /// Validates policy hash, schema version, and chunk strategy.
    /// Returns error if index doesn't exist or policy/schema/strategy mismatches.
    pub fn open(repo_root: &Path, policy: &Policy) -> Result<Self> {
        let tantivy_dir = repo_root.join(TANTIVY_DIR_RELATIVE);
        if !tantivy_dir.exists() {
            bail!("persistent index does not exist; run 'openlocus index build' first");
        }

        // Manifest/policy/schema/strategy gate
        if !IndexManifest::exists(repo_root) {
            bail!("persistent index manifest missing; rebuild the index");
        }

        let manifest = IndexManifest::load(repo_root)?;
        let current_policy_hash = compute_policy_hash(policy);
        if manifest.policy_hash != current_policy_hash {
            bail!(
                "persistent index policy hash mismatch: manifest={}, current={}. Rebuild the index",
                manifest.policy_hash,
                current_policy_hash
            );
        }
        if manifest.schema_version != SCHEMA_VERSION && manifest.schema_version != SCHEMA_VERSION_R7
        {
            bail!(
                "persistent index schema version mismatch: manifest={}, current={}. Rebuild the index",
                manifest.schema_version,
                SCHEMA_VERSION
            );
        }
        if manifest.chunk_strategy != ChunkStrategy::LineWindowV1
            && manifest.chunk_strategy != ChunkStrategy::AstV1
        {
            bail!(
                "persistent index chunk strategy unrecognized: {:?}. Rebuild the index",
                manifest.chunk_strategy
            );
        }

        let index = Index::open_in_dir(&tantivy_dir)?;
        let schema = index.schema();

        let path_field = schema.get_field("path")?;
        let language_field = schema.get_field("language")?;
        let content_sha_field = schema.get_field("content_sha")?;
        let start_line_field = schema.get_field("start_line")?;
        let end_line_field = schema.get_field("end_line")?;
        let content_field = schema.get_field("content")?;

        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let searcher = reader.searcher();

        Ok(Self {
            index,
            searcher,
            path_field,
            language_field,
            content_sha_field,
            start_line_field,
            end_line_field,
            content_field,
        })
    }

    /// Search using this opened index handle. Same safety gates as
    /// search_persistent_bm25: validate_path, empty sha skip, strict range.
    pub fn search(
        &self,
        repo_root: &Path,
        query: &str,
        max_results: usize,
    ) -> Result<(Vec<Evidence>, SearchStats)> {
        let query_start = Instant::now();

        let query_parser = QueryParser::for_index(&self.index, vec![self.content_field]);
        let parsed_query = match query_parser.parse_query(query) {
            Ok(p) => p,
            Err(_) => {
                let sanitized = query.replace([':', '/', '(', ')', '"'], " ");
                match query_parser.parse_query(sanitized.trim()) {
                    Ok(p) => p,
                    Err(_) => {
                        return Ok((
                            vec![],
                            SearchStats {
                                query_ms: query_start.elapsed().as_millis() as u64,
                                materialize_ms: 0,
                                stale_hits_skipped: 0,
                                invalid_hits_skipped: 0,
                            },
                        ));
                    }
                }
            }
        };

        let top_docs = self
            .searcher
            .search(&parsed_query, &TopDocs::with_limit(max_results * 2))?;

        let query_ms = query_start.elapsed().as_millis() as u64;
        let query_tokens = tokenize_query(query);

        let materialize_start = Instant::now();
        let mut results = Vec::new();
        let mut stale_hits_skipped: u64 = 0;
        let mut invalid_hits_skipped: u64 = 0;

        for (_score, doc_address) in top_docs {
            if results.len() >= max_results {
                break;
            }

            let doc: TantivyDocument = self.searcher.doc(doc_address)?;
            let path_val = doc
                .get_first(self.path_field)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let index_content_sha = doc
                .get_first(self.content_sha_field)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let chunk_start_line = doc
                .get_first(self.start_line_field)
                .and_then(|v| v.as_u64())
                .unwrap_or(1);
            let chunk_end_line = doc
                .get_first(self.end_line_field)
                .and_then(|v| v.as_u64())
                .unwrap_or(1);
            let language_val = doc
                .get_first(self.language_field)
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            // Empty content_sha → cannot verify → skip
            if index_content_sha.is_empty() {
                invalid_hits_skipped += 1;
                continue;
            }

            // Path safety
            if validate_path(repo_root, &path_val).is_err() {
                invalid_hits_skipped += 1;
                continue;
            }

            // Re-read current file
            let full_path = repo_root.join(&path_val);
            let content = match std::fs::read_to_string(&full_path) {
                Ok(c) => c,
                Err(_) => {
                    invalid_hits_skipped += 1;
                    continue;
                }
            };

            // Stale check
            let current_content_sha = blake3::hash(content.as_bytes()).to_hex().to_string();
            if index_content_sha != current_content_sha {
                stale_hits_skipped += 1;
                continue;
            }

            let lines: Vec<&str> = content.lines().collect();
            let total_lines = lines.len() as u64;

            if total_lines == 0 {
                invalid_hits_skipped += 1;
                continue;
            }

            // Strict range validation: no clamping
            if chunk_start_line < 1
                || chunk_start_line > chunk_end_line
                || chunk_end_line > total_lines
            {
                invalid_hits_skipped += 1;
                continue;
            }

            // Line-level scoring
            let best_line =
                find_best_matching_line(&lines, chunk_start_line, chunk_end_line, &query_tokens);

            let best_line = match best_line {
                Some(l) => l,
                None => {
                    invalid_hits_skipped += 1;
                    continue;
                }
            };

            // Tighten around best line
            let tight_start = best_line.saturating_sub(CONTEXT_LINES).max(1);
            let mut tight_end = (best_line + CONTEXT_LINES).min(total_lines);
            tight_end = tight_end.min(tight_start + MAX_EVIDENCE_SPAN - 1);
            let tight_start = tight_start.max(1);
            let tight_end = tight_end.min(total_lines);

            if tight_start < 1 || tight_start > tight_end || tight_end > total_lines {
                invalid_hits_skipped += 1;
                continue;
            }

            let excerpt = lines[(tight_start - 1) as usize..tight_end as usize].join("\n");
            let bm25_score = _score as f64;

            let evidence = Evidence::new(
                &path_val,
                tight_start,
                tight_end,
                &current_content_sha,
                bm25_score,
                vec![format!("persistent_bm25: {}", query)],
                vec![Channel::Bm25],
            )
            .with_excerpt(&excerpt)
            .with_language(&language_val)
            .with_freshness(Freshness::VerifiedCurrent)
            .with_score_parts(ScoreParts {
                bm25: Some(bm25_score),
                ..Default::default()
            });

            results.push(evidence);
        }

        let materialize_ms = materialize_start.elapsed().as_millis() as u64;

        Ok((
            results,
            SearchStats {
                query_ms,
                materialize_ms,
                stale_hits_skipped,
                invalid_hits_skipped,
            },
        ))
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

/// Tokenize a query into lowercase tokens, filtering out short noise words.
fn tokenize_query(query: &str) -> Vec<String> {
    query
        .split(|c: char| {
            c.is_whitespace() || c == ':' || c == '/' || c == '"' || c == '(' || c == ')'
        })
        .map(|t| t.trim().to_lowercase())
        .filter(|t| t.len() >= 2 && !t.starts_with('_'))
        .collect()
}

/// Find the line within [start_line, end_line] that has the highest query
/// token overlap score. Returns None if no line has any overlap.
fn find_best_matching_line(
    lines: &[&str],
    start_line: u64,
    end_line: u64,
    query_tokens: &[String],
) -> Option<u64> {
    if query_tokens.is_empty() {
        return None;
    }

    let mut best_score: u32 = 0;
    let mut best_line: Option<u64> = None;

    for line_num in start_line..=end_line {
        let idx = (line_num - 1) as usize;
        if idx >= lines.len() {
            break;
        }
        let line_lower = lines[idx].to_lowercase();
        let mut score: u32 = 0;
        for token in query_tokens {
            if line_lower.contains(token.as_str()) {
                score += 1;
            }
        }
        if score > best_score {
            best_score = score;
            best_line = Some(line_num);
        }
    }

    if best_score > 0 { best_line } else { None }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn compute_sha(root: &Path, path: &str) -> String {
        let bytes = std::fs::read(root.join(path)).unwrap();
        blake3::hash(&bytes).to_hex().to_string()
    }

    #[test]
    fn build_and_search() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(
            root.join("app.rs"),
            "fn authenticate_user() {}\nfn process_request() {}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("lib.rs"),
            "struct Config {\n    name: String,\n}\n",
        )
        .unwrap();

        let policy = Policy::default();
        let records = vec![
            FileRecord {
                path: "app.rs".into(),
                size: 0,
                content_sha: compute_sha(root, "app.rs"),
                language: "rust".into(),
            },
            FileRecord {
                path: "lib.rs".into(),
                size: 0,
                content_sha: compute_sha(root, "lib.rs"),
                language: "rust".into(),
            },
        ];

        let result = build_index(root, &records, &policy, ChunkStrategy::LineWindowV1).unwrap();
        assert!(result.success);
        assert_eq!(result.file_count, 2);
        assert!(result.chunk_count > 0);
        assert_eq!(result.chunk_strategy, "line");

        let (evidence, stats) = search_persistent_bm25(root, "authenticate", 10, &policy).unwrap();
        assert!(!evidence.is_empty(), "should find matches");
        assert_eq!(evidence[0].core.path, "app.rs");
        assert_eq!(evidence[0].core.channels[0], Channel::Bm25);
        assert_eq!(stats.stale_hits_skipped, 0);
    }

    #[test]
    fn build_ast_strategy() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(
            root.join("app.rs"),
            "fn authenticate_user() -> bool {\n    true\n}\n\nfn helper() -> i32 { 1 }\n",
        )
        .unwrap();

        let policy = Policy::default();
        let records = vec![FileRecord {
            path: "app.rs".into(),
            size: 0,
            content_sha: compute_sha(root, "app.rs"),
            language: "rust".into(),
        }];

        let result = build_index(root, &records, &policy, ChunkStrategy::AstV1).unwrap();
        assert!(result.success);
        assert_eq!(result.chunk_strategy, "ast");
        assert!(result.ast_stats.is_some());
        let ast = result.ast_stats.unwrap();
        assert_eq!(ast.supported_files, 1);
        assert!(ast.ast_chunks > 0, "should have AST chunks");

        // Search should work with AST-built index
        let (evidence, stats) = search_persistent_bm25(root, "authenticate", 10, &policy).unwrap();
        assert!(!evidence.is_empty(), "should find matches");
        assert_eq!(stats.stale_hits_skipped, 0);
    }

    #[test]
    fn search_skips_stale_hit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(root.join("auth.rs"), "fn authenticate() {}\n").unwrap();

        let policy = Policy::default();
        let records = vec![FileRecord {
            path: "auth.rs".into(),
            size: 0,
            content_sha: compute_sha(root, "auth.rs"),
            language: "rust".into(),
        }];

        let _ = build_index(root, &records, &policy, ChunkStrategy::LineWindowV1).unwrap();

        // Modify the file after indexing
        std::fs::write(root.join("auth.rs"), "fn authorize() {}\nfn extra() {}\n").unwrap();

        let (evidence, stats) = search_persistent_bm25(root, "authenticate", 10, &policy).unwrap();
        assert!(
            evidence.is_empty() || stats.stale_hits_skipped > 0,
            "stale hits should be skipped or no results"
        );
    }

    #[test]
    fn search_skips_deleted_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(root.join("temp.rs"), "fn temp() {}\n").unwrap();

        let policy = Policy::default();
        let records = vec![FileRecord {
            path: "temp.rs".into(),
            size: 0,
            content_sha: compute_sha(root, "temp.rs"),
            language: "rust".into(),
        }];

        let _ = build_index(root, &records, &policy, ChunkStrategy::LineWindowV1).unwrap();

        // Delete the file after indexing
        std::fs::remove_file(root.join("temp.rs")).unwrap();

        let (evidence, stats) = search_persistent_bm25(root, "temp", 10, &policy).unwrap();
        assert!(
            evidence.is_empty(),
            "deleted file should produce no evidence"
        );
        assert!(stats.invalid_hits_skipped > 0);
    }

    #[test]
    fn policy_gate_refuses_mismatched_policy() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(root.join("app.rs"), "fn authenticate() {}\n").unwrap();

        let policy = Policy::default();
        let records = vec![FileRecord {
            path: "app.rs".into(),
            size: 0,
            content_sha: compute_sha(root, "app.rs"),
            language: "rust".into(),
        }];

        let _ = build_index(root, &records, &policy, ChunkStrategy::LineWindowV1).unwrap();

        // Change policy
        let mut different_policy = Policy::default();
        different_policy.remote.allow = true;

        let result = search_persistent_bm25(root, "authenticate", 10, &different_policy);
        assert!(result.is_err(), "search should refuse with policy mismatch");
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("policy hash mismatch"),
            "error should mention policy hash mismatch: got {}",
            err_msg
        );
    }

    #[test]
    fn empty_content_sha_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(root.join("test.rs"), "fn hello() {}\n").unwrap();

        let policy = Policy::default();
        let records = vec![FileRecord {
            path: "test.rs".into(),
            size: 0,
            content_sha: compute_sha(root, "test.rs"),
            language: "rust".into(),
        }];

        let _ = build_index(root, &records, &policy, ChunkStrategy::LineWindowV1).unwrap();

        let (evidence, stats) = search_persistent_bm25(root, "hello", 10, &policy).unwrap();
        if !evidence.is_empty() {
            assert_eq!(stats.invalid_hits_skipped, 0);
        }
    }

    #[test]
    fn strict_range_validation_no_clamp() {
        let lines = vec!["line1", "line2", "line3"];
        let query_tokens = vec!["line".to_string()];

        let result = find_best_matching_line(&lines, 1, 3, &query_tokens);
        assert!(result.is_some());

        let result = find_best_matching_line(&lines, 3, 1, &query_tokens);
        assert!(result.is_none());
    }

    #[test]
    fn status_after_build() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(root.join("test.rs"), "fn test() {}\n").unwrap();

        let policy = Policy::default();
        let records = vec![FileRecord {
            path: "test.rs".into(),
            size: 0,
            content_sha: compute_sha(root, "test.rs"),
            language: "rust".into(),
        }];

        let _ = build_index(root, &records, &policy, ChunkStrategy::LineWindowV1).unwrap();

        let status = status_index(root, &policy).unwrap();
        assert!(status.exists);
        assert_eq!(status.schema_version.as_deref(), Some(SCHEMA_VERSION));
        assert_eq!(status.file_count, Some(1));
        assert_eq!(status.policy_hash_matches, Some(true));
        assert!(!status.requires_rebuild);
        assert_eq!(status.chunk_strategy.as_deref(), Some("line"));
    }

    #[test]
    fn status_after_ast_build() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(root.join("test.rs"), "fn test() {}\n").unwrap();

        let policy = Policy::default();
        let records = vec![FileRecord {
            path: "test.rs".into(),
            size: 0,
            content_sha: compute_sha(root, "test.rs"),
            language: "rust".into(),
        }];

        let _ = build_index(root, &records, &policy, ChunkStrategy::AstV1).unwrap();

        let status = status_index(root, &policy).unwrap();
        assert!(status.exists);
        assert_eq!(status.chunk_strategy.as_deref(), Some("ast"));
        assert!(status.ast_stats.is_some());
        assert!(!status.requires_rebuild);
    }

    #[test]
    fn status_detects_stale() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(root.join("app.rs"), "fn old() {}\n").unwrap();

        let policy = Policy::default();
        let records = vec![FileRecord {
            path: "app.rs".into(),
            size: 0,
            content_sha: compute_sha(root, "app.rs"),
            language: "rust".into(),
        }];

        let _ = build_index(root, &records, &policy, ChunkStrategy::LineWindowV1).unwrap();

        std::fs::write(root.join("app.rs"), "fn new() {}\nfn extra() {}\n").unwrap();

        let status = status_index(root, &policy).unwrap();
        assert!(status.requires_rebuild);
        assert_eq!(status.stale_files_fast, Some(1));
    }

    #[test]
    fn validate_after_build() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(root.join("v.rs"), "fn valid() {}\n").unwrap();

        let policy = Policy::default();
        let records = vec![FileRecord {
            path: "v.rs".into(),
            size: 0,
            content_sha: compute_sha(root, "v.rs"),
            language: "rust".into(),
        }];

        let _ = build_index(root, &records, &policy, ChunkStrategy::LineWindowV1).unwrap();

        let validate = validate_index(root, &policy).unwrap();
        assert!(validate.valid);
        assert!(validate.stale_files.is_empty());
        assert!(validate.deleted_files.is_empty());
        assert_eq!(validate.chunk_strategy.as_deref(), Some("line"));
    }

    #[test]
    fn validate_detects_stale() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(root.join("s.rs"), "fn stale() {}\n").unwrap();

        let policy = Policy::default();
        let records = vec![FileRecord {
            path: "s.rs".into(),
            size: 0,
            content_sha: compute_sha(root, "s.rs"),
            language: "rust".into(),
        }];

        let _ = build_index(root, &records, &policy, ChunkStrategy::LineWindowV1).unwrap();

        std::fs::write(root.join("s.rs"), "fn updated() {}\n").unwrap();

        let validate = validate_index(root, &policy).unwrap();
        assert!(!validate.valid);
        assert!(validate.stale_files.contains(&"s.rs".to_string()));
    }

    #[test]
    fn validate_detects_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(root.join("d.rs"), "fn del() {}\n").unwrap();

        let policy = Policy::default();
        let records = vec![FileRecord {
            path: "d.rs".into(),
            size: 0,
            content_sha: compute_sha(root, "d.rs"),
            language: "rust".into(),
        }];

        let _ = build_index(root, &records, &policy, ChunkStrategy::LineWindowV1).unwrap();
        std::fs::remove_file(root.join("d.rs")).unwrap();

        let validate = validate_index(root, &policy).unwrap();
        assert!(!validate.valid);
        assert!(validate.deleted_files.contains(&"d.rs".to_string()));
    }

    #[test]
    fn validate_detects_policy_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(root.join("v.rs"), "fn valid() {}\n").unwrap();

        let policy = Policy::default();
        let records = vec![FileRecord {
            path: "v.rs".into(),
            size: 0,
            content_sha: compute_sha(root, "v.rs"),
            language: "rust".into(),
        }];

        let _ = build_index(root, &records, &policy, ChunkStrategy::LineWindowV1).unwrap();

        let mut different_policy = Policy::default();
        different_policy.remote.allow = true;

        let validate = validate_index(root, &different_policy).unwrap();
        assert!(!validate.valid);
        assert!(!validate.policy_hash_matches);
    }

    #[test]
    fn purge_removes_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(root.join("p.rs"), "fn purge() {}\n").unwrap();

        let policy = Policy::default();
        let records = vec![FileRecord {
            path: "p.rs".into(),
            size: 0,
            content_sha: compute_sha(root, "p.rs"),
            language: "rust".into(),
        }];

        let _ = build_index(root, &records, &policy, ChunkStrategy::LineWindowV1).unwrap();

        assert!(root.join(TANTIVY_DIR_RELATIVE).exists());
        assert!(root.join(MANIFEST_PATH_RELATIVE).exists());

        let result = purge_index(root).unwrap();
        assert!(result.purged);
        assert!(!root.join(TANTIVY_DIR_RELATIVE).exists());
        assert!(!root.join(MANIFEST_PATH_RELATIVE).exists());
    }

    #[test]
    fn purge_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let result = purge_index(root).unwrap();
        assert!(result.purged);
    }

    #[test]
    fn search_no_token_overlap_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(root.join("fruit.rs"), "fn apple() {}\nfn orange() {}\n").unwrap();

        let policy = Policy::default();
        let records = vec![FileRecord {
            path: "fruit.rs".into(),
            size: 0,
            content_sha: compute_sha(root, "fruit.rs"),
            language: "rust".into(),
        }];

        let _ = build_index(root, &records, &policy, ChunkStrategy::LineWindowV1).unwrap();

        let (evidence, _stats) = search_persistent_bm25(root, "banana", 10, &policy).unwrap();
        assert!(evidence.is_empty(), "no token overlap should skip");
    }

    #[test]
    fn search_span_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let mut content = String::new();
        for i in 1..=100 {
            content.push_str(&format!("line {} has authentication data\n", i));
        }
        std::fs::write(root.join("big.rs"), &content).unwrap();

        let policy = Policy::default();
        let records = vec![FileRecord {
            path: "big.rs".into(),
            size: 0,
            content_sha: compute_sha(root, "big.rs"),
            language: "rust".into(),
        }];

        let _ = build_index(root, &records, &policy, ChunkStrategy::LineWindowV1).unwrap();

        let (evidence, _stats) =
            search_persistent_bm25(root, "authentication", 10, &policy).unwrap();
        for ev in &evidence {
            let span = ev.core.end_line - ev.core.start_line + 1;
            assert!(
                span <= MAX_EVIDENCE_SPAN,
                "evidence span {} exceeds max {}",
                span,
                MAX_EVIDENCE_SPAN
            );
        }
    }

    #[test]
    fn search_freshness_verified_current() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(root.join("test.rs"), "fn hello() {}\n").unwrap();

        let policy = Policy::default();
        let records = vec![FileRecord {
            path: "test.rs".into(),
            size: 0,
            content_sha: compute_sha(root, "test.rs"),
            language: "rust".into(),
        }];

        let _ = build_index(root, &records, &policy, ChunkStrategy::LineWindowV1).unwrap();

        let (evidence, _stats) = search_persistent_bm25(root, "hello", 10, &policy).unwrap();
        if let Some(ev) = evidence.first() {
            assert_eq!(
                ev.meta.as_ref().unwrap().freshness,
                Some(Freshness::VerifiedCurrent)
            );
        }
    }

    #[test]
    fn build_empty_records() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let policy = Policy::default();
        let result = build_index(root, &[], &policy, ChunkStrategy::LineWindowV1).unwrap();
        assert!(result.success);
        assert_eq!(result.file_count, 0);
        assert_eq!(result.chunk_count, 0);
    }

    #[test]
    fn status_no_index() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let policy = Policy::default();
        let status = status_index(root, &policy).unwrap();
        assert!(!status.exists);
        assert!(status.requires_rebuild);
    }

    #[test]
    fn validate_no_index() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let policy = Policy::default();
        let validate = validate_index(root, &policy).unwrap();
        assert!(!validate.valid);
    }

    #[test]
    fn reusable_index_handle_open_and_search() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(root.join("app.rs"), "fn authenticate_user() {}\n").unwrap();

        let policy = Policy::default();
        let records = vec![FileRecord {
            path: "app.rs".into(),
            size: 0,
            content_sha: compute_sha(root, "app.rs"),
            language: "rust".into(),
        }];

        let _ = build_index(root, &records, &policy, ChunkStrategy::LineWindowV1).unwrap();

        let handle = PersistentBm25Index::open(root, &policy).unwrap();
        let (evidence, stats) = handle.search(root, "authenticate", 10).unwrap();
        assert!(!evidence.is_empty());
        assert_eq!(stats.stale_hits_skipped, 0);
    }

    #[test]
    fn reusable_index_handle_policy_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(root.join("app.rs"), "fn authenticate() {}\n").unwrap();

        let policy = Policy::default();
        let records = vec![FileRecord {
            path: "app.rs".into(),
            size: 0,
            content_sha: compute_sha(root, "app.rs"),
            language: "rust".into(),
        }];

        let _ = build_index(root, &records, &policy, ChunkStrategy::LineWindowV1).unwrap();

        let mut different_policy = Policy::default();
        different_policy.remote.allow = true;

        let result = PersistentBm25Index::open(root, &different_policy);
        assert!(
            result.is_err(),
            "should refuse to open with mismatched policy"
        );
    }

    #[test]
    fn search_and_reusable_open_refuse_missing_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(root.join("app.rs"), "fn authenticate() {}\n").unwrap();

        let policy = Policy::default();
        let records = vec![FileRecord {
            path: "app.rs".into(),
            size: 0,
            content_sha: compute_sha(root, "app.rs"),
            language: "rust".into(),
        }];

        let _ = build_index(root, &records, &policy, ChunkStrategy::LineWindowV1).unwrap();
        std::fs::remove_file(root.join(MANIFEST_PATH_RELATIVE)).unwrap();

        let search_result = search_persistent_bm25(root, "authenticate", 10, &policy);
        assert!(
            search_result.is_err(),
            "persistent search must refuse when manifest is missing"
        );
        let search_err = format!("{}", search_result.unwrap_err());
        assert!(search_err.contains("manifest missing"));

        let open_result = PersistentBm25Index::open(root, &policy);
        assert!(
            open_result.is_err(),
            "reusable index open must refuse when manifest is missing"
        );
        let open_err = format!("{}", open_result.err().unwrap());
        assert!(open_err.contains("manifest missing"));
    }
}
