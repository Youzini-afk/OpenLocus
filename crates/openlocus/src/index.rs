use crate::model::{BuildSummary, Candidate, IndexStatus, UpdateSummary};
use crate::policy::Policy;
use crate::repo::{canonical_source_root, normalize_relative, scan, validate_source_path};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::*;
use tantivy::{Index, IndexReader, ReloadPolicy, TantivyDocument, Term, doc};

const INDEX_FORMAT: u32 = 1;
const INDEX_DIR: &str = "tantivy";
const BUILDING_DIR: &str = "tantivy.building";
const PREVIOUS_DIR: &str = "tantivy.previous";
const MANIFEST_FILE: &str = "manifest.json";
const MANIFEST_BUILDING_FILE: &str = "manifest.building.json";
const MAX_CHUNK_LINES: usize = 30;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestFile {
    content_sha: String,
    chunks: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: u32,
    source_id: String,
    policy_hash: String,
    generation: u64,
    files: BTreeMap<String, ManifestFile>,
}

impl Manifest {
    fn file_count(&self) -> u64 {
        self.files.len() as u64
    }

    fn chunk_count(&self) -> u64 {
        self.files.values().map(|file| file.chunks).sum()
    }
}

#[derive(Clone, Copy)]
struct Fields {
    path: Field,
    content_sha: Field,
    start_line: Field,
    end_line: Field,
    content: Field,
}

impl Fields {
    fn from_schema(schema: &Schema) -> Result<Self> {
        Ok(Self {
            path: schema.get_field("path")?,
            content_sha: schema.get_field("content_sha")?,
            start_line: schema.get_field("start_line")?,
            end_line: schema.get_field("end_line")?,
            content: schema.get_field("content")?,
        })
    }
}

pub(crate) struct IndexStore {
    source_root: PathBuf,
    state_root: PathBuf,
    policy: Policy,
    index: Index,
    reader: IndexReader,
    fields: Fields,
    manifest: Manifest,
}

impl IndexStore {
    pub(crate) fn open(
        source_root: &Path,
        state_root: &Path,
        policy: &Policy,
    ) -> Result<Option<Self>> {
        let (source_root, state_root) = prepare_roots(source_root, state_root)?;
        let index_path = state_root.join(INDEX_DIR);
        let manifest_path = state_root.join(MANIFEST_FILE);
        if !index_path.exists() && !manifest_path.exists() {
            return Ok(None);
        }
        if !index_path.is_dir() || !manifest_path.is_file() {
            bail!("index state is incomplete; rebuild the index");
        }
        reject_link(&index_path)?;
        reject_link(&manifest_path)?;
        let manifest: Manifest = serde_json::from_str(
            &fs::read_to_string(&manifest_path).context("failed to read index manifest")?,
        )
        .context("failed to parse index manifest; rebuild the index")?;
        validate_manifest(&manifest, &source_root, policy)?;

        let index = Index::open_in_dir(&index_path).context("failed to open Tantivy index")?;
        let metadata = index
            .load_metas()
            .context("failed to read Tantivy metadata")?;
        let expected_generation = manifest.generation.to_string();
        if metadata.payload.as_deref() != Some(expected_generation.as_str()) {
            bail!("index generation does not match manifest; rebuild the index");
        }
        let fields = Fields::from_schema(&index.schema()).context("index schema mismatch")?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        Ok(Some(Self {
            source_root,
            state_root,
            policy: policy.clone(),
            index,
            reader,
            fields,
            manifest,
        }))
    }

    pub(crate) fn build(
        source_root: &Path,
        state_root: &Path,
        policy: &Policy,
    ) -> Result<(Self, BuildSummary)> {
        let (source_root, state_root) = prepare_roots(source_root, state_root)?;
        let building_path = state_root.join(BUILDING_DIR);
        let previous_path = state_root.join(PREVIOUS_DIR);
        safe_remove_dir(&state_root, &building_path)?;
        safe_remove_dir(&state_root, &previous_path)?;
        fs::create_dir(&building_path).context("failed to create temporary index directory")?;

        let (schema, fields) = create_schema();
        let index = Index::create_in_dir(&building_path, schema)?;
        let mut writer = index.writer(50_000_000)?;
        let mut files = BTreeMap::new();

        for record in scan(&source_root, policy)? {
            let path = match validate_source_path(&source_root, &record.path) {
                Ok(path) => path,
                Err(_) => continue,
            };
            let bytes = match fs::read(path) {
                Ok(bytes)
                    if bytes.len() as u64 == record.size
                        && blake3::hash(&bytes).to_hex().as_str() == record.content_sha =>
                {
                    bytes
                }
                Err(_) => continue,
                _ => continue,
            };
            let content = match std::str::from_utf8(&bytes) {
                Ok(content) => content,
                Err(_) => continue,
            };
            let content_sha = record.content_sha;
            let chunks =
                add_file_documents(&mut writer, fields, &record.path, &content_sha, content)?;
            if chunks > 0 {
                files.insert(
                    record.path,
                    ManifestFile {
                        content_sha,
                        chunks,
                    },
                );
            }
        }

        let mut prepared = writer.prepare_commit()?;
        let generation = prepared.opstamp();
        prepared.set_payload(&generation.to_string());
        prepared.commit()?;
        drop(writer);
        drop(index);

        let manifest = Manifest {
            format: INDEX_FORMAT,
            source_id: source_id(&source_root),
            policy_hash: policy.hash()?,
            generation,
            files,
        };
        write_json_file(&state_root.join(MANIFEST_BUILDING_FILE), &manifest)?;
        swap_built_index(&state_root)?;

        let summary = BuildSummary {
            files_indexed: manifest.file_count(),
            chunks_indexed: manifest.chunk_count(),
        };
        let store = Self::open(&source_root, &state_root, policy)?
            .context("newly built index could not be reopened")?;
        Ok((store, summary))
    }

    pub(crate) fn status(&self) -> IndexStatus {
        IndexStatus {
            ready: true,
            files_indexed: self.manifest.file_count(),
            chunks_indexed: self.manifest.chunk_count(),
            reason: None,
        }
    }

    pub(crate) fn search(&self, query: &str, limit: usize) -> Result<Vec<Candidate>> {
        let parser = QueryParser::for_index(&self.index, vec![self.fields.content]);
        let (query, _) = parser.parse_query_lenient(query);
        let searcher = self.reader.searcher();
        let fetch_limit = limit.saturating_mul(4).min(512).max(limit);
        let top_docs = searcher.search(&query, &TopDocs::with_limit(fetch_limit))?;
        let mut candidates = Vec::new();
        for (score, address) in top_docs {
            let document: TantivyDocument = searcher.doc(address)?;
            let Some(path) = document
                .get_first(self.fields.path)
                .and_then(|value| value.as_str())
            else {
                continue;
            };
            let Some(expected_sha) = document
                .get_first(self.fields.content_sha)
                .and_then(|value| value.as_str())
            else {
                continue;
            };
            let Some(start_line) = document
                .get_first(self.fields.start_line)
                .and_then(|value| value.as_u64())
            else {
                continue;
            };
            let Some(end_line) = document
                .get_first(self.fields.end_line)
                .and_then(|value| value.as_u64())
            else {
                continue;
            };
            candidates.push(Candidate {
                path: path.to_string(),
                start_line,
                end_line,
                expected_sha: expected_sha.to_string(),
                score: score as f64,
            });
        }
        candidates.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.path.cmp(&right.path))
                .then_with(|| left.start_line.cmp(&right.start_line))
                .then_with(|| left.end_line.cmp(&right.end_line))
        });
        let mut seen = BTreeSet::new();
        candidates.retain(|candidate| {
            seen.insert((
                candidate.path.clone(),
                candidate.start_line,
                candidate.end_line,
            ))
        });
        candidates.truncate(limit);
        Ok(candidates)
    }

    pub(crate) fn update_paths(&mut self, paths: &[PathBuf]) -> Result<UpdateSummary> {
        if paths.is_empty() {
            bail!("at least one path is required");
        }
        let mut normalized = paths
            .iter()
            .map(|path| normalize_relative(path))
            .collect::<Result<Vec<_>>>()?;
        normalized.sort();
        normalized.dedup();

        let mut next_manifest = self.manifest.clone();
        let mut writer = self.index.writer(50_000_000)?;
        let mut paths_updated = 0;
        let mut paths_deleted = 0;

        for path in normalized {
            writer.delete_term(Term::from_field_text(self.fields.path, &path));
            next_manifest.files.remove(&path);
            let joined = self
                .source_root
                .join(path.replace('/', std::path::MAIN_SEPARATOR_STR));
            if !joined.exists() {
                paths_deleted += 1;
                continue;
            }
            if !self.policy.allows(&path) {
                paths_deleted += 1;
                continue;
            }
            let full_path = validate_source_path(&self.source_root, &path)?;
            let metadata = fs::metadata(&full_path)?;
            if metadata.len() > self.policy.max_file_bytes {
                paths_deleted += 1;
                continue;
            }
            let bytes = fs::read(&full_path)?;
            if bytes.contains(&0) {
                paths_deleted += 1;
                continue;
            }
            let content = std::str::from_utf8(&bytes)
                .with_context(|| format!("source file is not valid UTF-8: {path}"))?;
            let content_sha = blake3::hash(&bytes).to_hex().to_string();
            let chunks =
                add_file_documents(&mut writer, self.fields, &path, &content_sha, content)?;
            if chunks > 0 {
                next_manifest.files.insert(
                    path,
                    ManifestFile {
                        content_sha,
                        chunks,
                    },
                );
                paths_updated += 1;
            }
        }

        let mut prepared = writer.prepare_commit()?;
        let generation = prepared.opstamp();
        prepared.set_payload(&generation.to_string());
        prepared.commit()?;
        next_manifest.generation = generation;
        let building_manifest = self.state_root.join(MANIFEST_BUILDING_FILE);
        safe_remove_file(&self.state_root, &building_manifest)?;
        write_json_file(&building_manifest, &next_manifest)?;
        install_manifest(&self.state_root)?;
        self.reader.reload()?;
        self.manifest = next_manifest;

        Ok(UpdateSummary {
            paths_updated,
            paths_deleted,
            files_indexed: self.manifest.file_count(),
            chunks_indexed: self.manifest.chunk_count(),
        })
    }
}

pub(crate) fn status_without_open(reason: Option<String>) -> IndexStatus {
    IndexStatus {
        ready: false,
        files_indexed: 0,
        chunks_indexed: 0,
        reason,
    }
}

pub(crate) fn purge(source_root: &Path, state_root: &Path) -> Result<()> {
    let (_, state_root) = prepare_roots(source_root, state_root)?;
    for directory in [INDEX_DIR, BUILDING_DIR, PREVIOUS_DIR] {
        safe_remove_dir(&state_root, &state_root.join(directory))?;
    }
    for file in [MANIFEST_FILE, MANIFEST_BUILDING_FILE] {
        safe_remove_file(&state_root, &state_root.join(file))?;
    }
    Ok(())
}

fn create_schema() -> (Schema, Fields) {
    let mut builder = Schema::builder();
    let path = builder.add_text_field("path", STRING | STORED);
    let content_sha = builder.add_text_field("content_sha", STRING | STORED);
    let start_line = builder.add_u64_field("start_line", STORED);
    let end_line = builder.add_u64_field("end_line", STORED);
    let content = builder.add_text_field("content", TEXT);
    let schema = builder.build();
    (
        schema,
        Fields {
            path,
            content_sha,
            start_line,
            end_line,
            content,
        },
    )
}

fn add_file_documents(
    writer: &mut tantivy::IndexWriter,
    fields: Fields,
    path: &str,
    content_sha: &str,
    content: &str,
) -> Result<u64> {
    let lines = content.lines().collect::<Vec<_>>();
    let mut chunks = 0;
    for (index, window) in lines.chunks(MAX_CHUNK_LINES).enumerate() {
        if window.is_empty() {
            continue;
        }
        let start_line = (index * MAX_CHUNK_LINES + 1) as u64;
        let end_line = start_line + window.len() as u64 - 1;
        let chunk = window.join("\n");
        writer.add_document(doc!(
            fields.path => path,
            fields.content_sha => content_sha,
            fields.start_line => start_line,
            fields.end_line => end_line,
            fields.content => chunk,
        ))?;
        chunks += 1;
    }
    Ok(chunks)
}

fn prepare_roots(source_root: &Path, state_root: &Path) -> Result<(PathBuf, PathBuf)> {
    let source_root = canonical_source_root(source_root)?;
    fs::create_dir_all(state_root)
        .with_context(|| format!("failed to create state root: {}", state_root.display()))?;
    reject_link(state_root)?;
    let state_root = state_root
        .canonicalize()
        .with_context(|| format!("failed to resolve state root: {}", state_root.display()))?;
    if source_root.starts_with(&state_root) || state_root.starts_with(&source_root) {
        bail!("state root and source root must not overlap");
    }
    Ok((source_root, state_root))
}

fn validate_manifest(manifest: &Manifest, source_root: &Path, policy: &Policy) -> Result<()> {
    if manifest.format != INDEX_FORMAT {
        bail!("index format changed; rebuild the index");
    }
    if manifest.source_id != source_id(source_root) {
        bail!("index belongs to a different source root; rebuild the index");
    }
    if manifest.policy_hash != policy.hash()? {
        bail!("index policy changed; rebuild the index");
    }
    Ok(())
}

fn source_id(source_root: &Path) -> String {
    blake3::hash(source_root.to_string_lossy().as_bytes())
        .to_hex()
        .to_string()
}

fn swap_built_index(state_root: &Path) -> Result<()> {
    let current = state_root.join(INDEX_DIR);
    let building = state_root.join(BUILDING_DIR);
    let previous = state_root.join(PREVIOUS_DIR);
    if current.exists() {
        reject_link(&current)?;
        fs::rename(&current, &previous).context("failed to stage previous index")?;
    }
    if let Err(error) = fs::rename(&building, &current) {
        if previous.exists() {
            let _ = fs::rename(&previous, &current);
        }
        return Err(error).context("failed to install new index");
    }
    install_manifest(state_root)?;
    safe_remove_dir(state_root, &previous)?;
    Ok(())
}

fn install_manifest(state_root: &Path) -> Result<()> {
    let manifest = state_root.join(MANIFEST_FILE);
    let building_manifest = state_root.join(MANIFEST_BUILDING_FILE);
    safe_remove_file(state_root, &manifest)?;
    fs::rename(&building_manifest, &manifest).context("failed to install index manifest")
}

fn write_json_file(path: &Path, value: &impl Serialize) -> Result<()> {
    if path.exists() {
        reject_link(path)?;
    }
    let bytes = serde_json::to_vec_pretty(value)?;
    let mut file =
        File::create(path).with_context(|| format!("failed to write {}", path.display()))?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(())
}

fn safe_remove_dir(state_root: &Path, path: &Path) -> Result<()> {
    ensure_direct_child(state_root, path)?;
    if !path.exists() {
        return Ok(());
    }
    reject_link(path)?;
    if !path.is_dir() {
        bail!(
            "refusing to remove non-directory state artifact: {}",
            path.display()
        );
    }
    fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))
}

fn safe_remove_file(state_root: &Path, path: &Path) -> Result<()> {
    ensure_direct_child(state_root, path)?;
    if !path.exists() {
        return Ok(());
    }
    reject_link(path)?;
    if !path.is_file() {
        bail!(
            "refusing to remove non-file state artifact: {}",
            path.display()
        );
    }
    fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))
}

fn ensure_direct_child(state_root: &Path, path: &Path) -> Result<()> {
    if path.parent() != Some(state_root) {
        bail!(
            "state artifact is outside the state root: {}",
            path.display()
        );
    }
    Ok(())
}

fn reject_link(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect state path: {}", path.display()))?;
    if metadata.file_type().is_symlink() || is_windows_reparse_point(&metadata) {
        bail!("symbolic links and reparse points are not allowed in index state");
    }
    Ok(())
}

#[cfg(windows)]
fn is_windows_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_windows_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_open_search_and_update() {
        let source = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        fs::write(source.path().join("lib.rs"), "fn original_marker() {}\n").unwrap();
        let policy = Policy::default();
        let (mut store, summary) = IndexStore::build(source.path(), state.path(), &policy).unwrap();
        assert_eq!(summary.files_indexed, 1);
        assert_eq!(store.search("original_marker", 10).unwrap().len(), 1);

        fs::write(source.path().join("lib.rs"), "fn changed_marker() {}\n").unwrap();
        store.update_paths(&[PathBuf::from("lib.rs")]).unwrap();
        assert!(store.search("original_marker", 10).unwrap().is_empty());
        assert_eq!(store.search("changed_marker", 10).unwrap().len(), 1);
    }

    #[test]
    fn source_and_state_must_be_separate() {
        let source = tempfile::tempdir().unwrap();
        let state = source.path().join("state");
        let error = prepare_roots(source.path(), &state).unwrap_err();
        assert!(error.to_string().contains("must not overlap"));
    }

    #[test]
    fn generation_mismatch_is_detected() {
        let source = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        fs::write(source.path().join("lib.rs"), "fn marker() {}\n").unwrap();
        let policy = Policy::default();
        IndexStore::build(source.path(), state.path(), &policy).unwrap();

        let manifest_path = state.path().join(MANIFEST_FILE);
        let mut manifest: Manifest =
            serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
        manifest.generation += 1;
        write_json_file(&manifest_path, &manifest).unwrap();
        assert!(IndexStore::open(source.path(), state.path(), &policy).is_err());
    }
}
