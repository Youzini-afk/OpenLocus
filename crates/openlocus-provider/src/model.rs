//! Core model types for the provider/embedding subsystem.

use serde::{Deserialize, Serialize};

/// Provider locality: where the embedding computation happens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderLocality {
    /// No embedding available; all calls return unavailable.
    Disabled,
    /// Deterministic local mock; no network.
    Mock,
    /// Local model (future: on-device model).
    Local,
    /// Remote API (future: OpenAI, etc.).
    Remote,
}

impl ProviderLocality {
    pub fn is_remote(&self) -> bool {
        matches!(self, Self::Remote)
    }

    pub fn is_available(&self) -> bool {
        !matches!(self, Self::Disabled)
    }
}

/// Static metadata about an embedding provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderMetadata {
    pub provider_id: String,
    pub model_id: String,
    pub dimensions: usize,
    pub locality: ProviderLocality,
    /// Maximum data level this provider is allowed to process.
    pub max_data_level: u8,
    /// Whether this provider can make outbound network calls.
    pub outbound_possible: bool,
}

/// Input to an embedding request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedInput {
    /// Unique identifier for this input (e.g. "path:start-end").
    pub input_id: String,
    /// File path (relative to repo root).
    pub path: String,
    /// Start line (1-indexed).
    pub start_line: u64,
    /// End line (1-indexed, inclusive).
    pub end_line: u64,
    /// BLAKE3 hash of the source file content.
    pub source_content_sha: String,
    /// Guessed language.
    pub language: String,
    /// Kind of view (e.g. "metadata", "snippet", "query").
    pub view_kind: String,
    /// Text to be embedded (NOT stored in audit/vector store).
    #[serde(skip_serializing)]
    pub text: String,
    /// BLAKE3 hash of the text to be embedded.
    pub text_sha: String,
    /// Data level of this input (0=metadata only, 1=limited snippet).
    pub data_level: u8,
    /// Policy mode in effect when this input was created.
    pub policy_mode: String,
    /// Purpose of this embedding (e.g. "index", "query").
    pub purpose: String,
}

/// A stored embedding record (in vector JSONL store).
/// No raw text is stored; only path/range/hashes/metadata/vector.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingRecord {
    pub cache_key: String,
    pub provider_id: String,
    pub model_id: String,
    pub dimensions: usize,
    pub path: String,
    pub start_line: u64,
    pub end_line: u64,
    pub source_content_sha: String,
    pub language: String,
    pub view_kind: String,
    /// Hash of the text that was embedded (not the raw text).
    pub text_sha: String,
    pub policy_mode: String,
    pub data_level: u8,
    pub vector: Vec<f32>,
}

/// Audit event for embedding operations. Written as JSONL.
/// No raw `text` or `vector` fields are included.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingAuditEvent {
    pub timestamp: String,
    pub event: String,
    pub request_id: String,
    pub provider_id: String,
    pub model_id: String,
    pub locality: ProviderLocality,
    pub purpose: String,
    /// Optional: file path (absent for query inputs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Line range as "start-end" string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_range: Option<String>,
    pub data_level: u8,
    pub view_kind: String,
    pub bytes_selected: usize,
    pub text_sha: String,
    pub secret_scan: String,
    pub policy_decision: String,
    pub cache_key: String,
    pub outbound_attempted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Decision from the policy gate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderDecision {
    pub allowed: bool,
    pub reason: String,
    /// Result of secret scanning on the input text.
    pub secret_scan: String,
}

/// Schema version for cache key computation.
pub const CACHE_KEY_SCHEMA_VERSION: &str = "emb1";

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_locality_checks() {
        assert!(ProviderLocality::Mock.is_available());
        assert!(!ProviderLocality::Mock.is_remote());
        assert!(!ProviderLocality::Disabled.is_available());
        assert!(ProviderLocality::Remote.is_remote());
        assert!(!ProviderLocality::Local.is_remote());
    }

    #[test]
    fn embed_input_skips_text_in_serialize() {
        let input = EmbedInput {
            input_id: "test:1-1".into(),
            path: "test.rs".into(),
            start_line: 1,
            end_line: 1,
            source_content_sha: "sha123".into(),
            language: "rust".into(),
            view_kind: "metadata".into(),
            text: "secret code here".into(),
            text_sha: "textsha".into(),
            data_level: 0,
            policy_mode: "local_only".into(),
            purpose: "index".into(),
        };
        let json = serde_json::to_value(&input).unwrap();
        // text field should be skipped
        assert!(json.get("text").is_none());
        // text_sha should be present
        assert_eq!(json["text_sha"], "textsha");
    }

    #[test]
    fn embedding_record_has_no_raw_text() {
        let record = EmbeddingRecord {
            cache_key: "key1".into(),
            provider_id: "mock".into(),
            model_id: "mock-v1".into(),
            dimensions: 32,
            path: "lib.rs".into(),
            start_line: 1,
            end_line: 8,
            source_content_sha: "sha".into(),
            language: "rust".into(),
            view_kind: "metadata".into(),
            text_sha: "tsha".into(),
            policy_mode: "local_only".into(),
            data_level: 0,
            vector: vec![0.1; 32],
        };
        let json = serde_json::to_value(&record).unwrap();
        // No raw text field
        assert!(json.get("text").is_none());
        // text_sha is present
        assert!(json.get("text_sha").is_some());
        // vector is present (store needs it for search)
        assert!(json.get("vector").is_some());
    }

    #[test]
    fn audit_event_has_no_raw_text_or_vector() {
        let event = EmbeddingAuditEvent {
            timestamp: "2026-06-11T00:00:00Z".into(),
            event: "allow".into(),
            request_id: "req1".into(),
            provider_id: "mock".into(),
            model_id: "mock-v1".into(),
            locality: ProviderLocality::Mock,
            purpose: "index".into(),
            path: Some("lib.rs".into()),
            line_range: Some("1-8".into()),
            data_level: 0,
            view_kind: "metadata".into(),
            bytes_selected: 42,
            text_sha: "tsha".into(),
            secret_scan: "clean".into(),
            policy_decision: "allow".into(),
            cache_key: "key1".into(),
            outbound_attempted: false,
            reason: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert!(json.get("text").is_none());
        assert!(json.get("vector").is_none());
        assert!(json.get("text_sha").is_some());
    }
}
