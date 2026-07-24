use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Path,
    Literal,
    Bm25,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    VerifiedCurrent,
}

#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub path: String,
    pub start_line: u64,
    pub end_line: u64,
    pub expected_sha: String,
    pub score: f64,
}

/// A source span re-read and verified against the current working tree.
///
/// Fields are intentionally not public: callers can inspect evidence but
/// cannot manufacture verified evidence without going through `Engine`.
#[derive(Debug, Clone, Serialize)]
pub struct Evidence {
    pub(crate) path: String,
    pub(crate) start_line: u64,
    pub(crate) end_line: u64,
    pub(crate) content_sha: String,
    pub(crate) excerpt: String,
    pub(crate) score: f64,
    pub(crate) why: Vec<String>,
    pub(crate) channels: Vec<Channel>,
    freshness: Freshness,
}

impl Evidence {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn verified(
        path: String,
        start_line: u64,
        end_line: u64,
        content_sha: String,
        excerpt: String,
        score: f64,
        why: Vec<String>,
        channels: Vec<Channel>,
    ) -> Self {
        Self {
            path,
            start_line,
            end_line,
            content_sha,
            excerpt,
            score,
            why,
            channels,
            freshness: Freshness::VerifiedCurrent,
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn start_line(&self) -> u64 {
        self.start_line
    }

    pub fn end_line(&self) -> u64 {
        self.end_line
    }

    pub fn content_sha(&self) -> &str {
        &self.content_sha
    }

    pub fn excerpt(&self) -> &str {
        &self.excerpt
    }

    pub fn score(&self) -> f64 {
        self.score
    }

    pub fn channels(&self) -> &[Channel] {
        &self.channels
    }

    pub fn why(&self) -> &[String] {
        &self.why
    }

    pub fn freshness(&self) -> Freshness {
        self.freshness
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryRequest {
    pub query: String,
    #[serde(default = "default_max_results")]
    pub max_results: usize,
}

impl QueryRequest {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            max_results: default_max_results(),
        }
    }
}

fn default_max_results() -> usize {
    20
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryStatus {
    Complete,
    Partial,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct QueryDiagnostics {
    pub stale_hits_skipped: u64,
    pub invalid_hits_skipped: u64,
    pub channels_used: Vec<Channel>,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueryResult {
    pub status: QueryStatus,
    pub evidence: Vec<Evidence>,
    pub diagnostics: QueryDiagnostics,
}

#[derive(Debug, Clone, Serialize)]
pub struct BuildSummary {
    pub files_indexed: u64,
    pub chunks_indexed: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateSummary {
    pub paths_updated: u64,
    pub paths_deleted: u64,
    pub files_indexed: u64,
    pub chunks_indexed: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexStatus {
    pub ready: bool,
    pub files_indexed: u64,
    pub chunks_indexed: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Citation {
    pub path: String,
    pub start_line: u64,
    pub end_line: u64,
    pub content_sha: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CitationValidation {
    pub citation: Citation,
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
