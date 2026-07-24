use crate::index::{IndexStore, purge, status_without_open};
use crate::model::{
    BuildSummary, Citation, CitationValidation, Evidence, IndexStatus, QueryDiagnostics,
    QueryRequest, QueryResult, QueryStatus, UpdateSummary,
};
use crate::policy::Policy;
use crate::rank::fuse;
use crate::repo::{
    MaterializedCandidate, canonical_source_root, literal_search, materialize_candidate, read,
    validate_citation,
};
use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const MAX_QUERY_BYTES: usize = 4096;
const MAX_RESULTS: usize = 64;

pub struct Engine {
    source_root: PathBuf,
    state_root: PathBuf,
    policy: Policy,
    index: Option<IndexStore>,
    index_error: Option<String>,
}

impl Engine {
    pub fn open(source_root: impl AsRef<Path>, state_root: impl AsRef<Path>) -> Result<Self> {
        let source_root = canonical_source_root(source_root.as_ref())?;
        let state_root = state_root.as_ref().to_path_buf();
        let policy = Policy::load(&source_root)?;
        let (index, index_error) = match IndexStore::open(&source_root, &state_root, &policy) {
            Ok(index) => (index, None),
            Err(error) => (None, Some(error.to_string())),
        };
        Ok(Self {
            source_root,
            state_root,
            policy,
            index,
            index_error,
        })
    }

    pub fn build_index(&mut self) -> Result<BuildSummary> {
        let (index, summary) =
            IndexStore::build(&self.source_root, &self.state_root, &self.policy)?;
        self.index = Some(index);
        self.index_error = None;
        Ok(summary)
    }

    pub fn query(&self, request: QueryRequest) -> Result<QueryResult> {
        let query = request.query.trim();
        if query.is_empty() {
            bail!("query must not be empty");
        }
        if query.len() > MAX_QUERY_BYTES {
            bail!("query must not exceed {MAX_QUERY_BYTES} bytes");
        }
        if !(1..=MAX_RESULTS).contains(&request.max_results) {
            bail!("max_results must be between 1 and {MAX_RESULTS}");
        }
        let index = self.index.as_ref().with_context(|| {
            self.index_error
                .clone()
                .unwrap_or_else(|| "index is not built; run `openlocus index build`".into())
        })?;
        let candidate_limit = request.max_results.saturating_mul(4);
        let mut bm25 = Vec::new();
        let mut stale_hits_skipped = 0;
        let mut invalid_hits_skipped = 0;
        for candidate in index.search(query, candidate_limit)? {
            match materialize_candidate(&self.source_root, candidate, query) {
                MaterializedCandidate::Evidence(evidence) => bm25.push(evidence),
                MaterializedCandidate::Stale => stale_hits_skipped += 1,
                MaterializedCandidate::Invalid => invalid_hits_skipped += 1,
            }
        }
        let literal = literal_search(&self.source_root, &self.policy, query, candidate_limit)?;
        let mut evidence = fuse(vec![bm25, literal]);
        evidence.truncate(request.max_results);
        let channels_used = evidence
            .iter()
            .flat_map(|item| item.channels.iter().copied())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let status = if stale_hits_skipped > 0 || invalid_hits_skipped > 0 {
            QueryStatus::Partial
        } else {
            QueryStatus::Complete
        };
        Ok(QueryResult {
            status,
            evidence,
            diagnostics: QueryDiagnostics {
                stale_hits_skipped,
                invalid_hits_skipped,
                channels_used,
            },
        })
    }

    pub fn read(&self, path_spec: &str) -> Result<Evidence> {
        read(&self.source_root, &self.policy, path_spec)
    }

    pub fn update_paths(&mut self, paths: &[PathBuf]) -> Result<UpdateSummary> {
        self.index
            .as_mut()
            .context("index is not ready; run `openlocus index build`")?
            .update_paths(paths)
    }

    pub fn index_status(&self) -> IndexStatus {
        self.index.as_ref().map_or_else(
            || status_without_open(self.index_error.clone()),
            IndexStore::status,
        )
    }

    pub fn purge_index(&mut self) -> Result<()> {
        self.index = None;
        purge(&self.source_root, &self.state_root)?;
        self.index_error = Some("index is not built".into());
        Ok(())
    }

    pub fn validate_citations(&self, citations: Vec<Citation>) -> Vec<CitationValidation> {
        citations
            .into_iter()
            .map(|citation| validate_citation(&self.source_root, &self.policy, citation))
            .collect()
    }

    pub fn source_root(&self) -> &Path {
        &self.source_root
    }

    pub fn state_root(&self) -> &Path {
        &self.state_root
    }
}

pub fn default_state_root(source_root: impl AsRef<Path>) -> Result<PathBuf> {
    let source_root = canonical_source_root(source_root.as_ref())?;
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_CACHE_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    let source_id = blake3::hash(source_root.to_string_lossy().as_bytes())
        .to_hex()
        .to_string();
    Ok(base.join("openlocus").join(source_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Channel;
    use std::fs;

    #[test]
    fn query_never_emits_stale_index_evidence() {
        let source = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        fs::write(source.path().join("lib.rs"), "fn original_marker() {}\n").unwrap();
        let mut engine = Engine::open(source.path(), state.path()).unwrap();
        engine.build_index().unwrap();
        let current = engine.query(QueryRequest::new("original_marker")).unwrap();
        assert_eq!(current.status, QueryStatus::Complete);
        assert_eq!(current.evidence.len(), 1);

        fs::write(source.path().join("lib.rs"), "fn changed_marker() {}\n").unwrap();
        let stale = engine.query(QueryRequest::new("original_marker")).unwrap();
        assert_eq!(stale.status, QueryStatus::Partial);
        assert!(stale.evidence.is_empty());

        engine.update_paths(&[PathBuf::from("lib.rs")]).unwrap();
        let updated = engine.query(QueryRequest::new("changed_marker")).unwrap();
        assert_eq!(updated.status, QueryStatus::Complete);
        assert_eq!(updated.evidence.len(), 1);
        assert_eq!(
            updated.diagnostics.channels_used,
            vec![Channel::Literal, Channel::Bm25]
        );
    }
}
