//! Embedding audit writer.
//!
//! Appends JSONL to `.openlocus/audit/embeddings.jsonl`.
//! No raw text or vector in audit events.

use crate::model::EmbeddingAuditEvent;
use anyhow::Result;
use std::fs;
use std::path::Path;

/// Path to the embedding audit file relative to repo root.
pub const AUDIT_RELATIVE_PATH: &str = ".openlocus/audit/embeddings.jsonl";

/// Append an audit event to the JSONL file.
pub fn append_audit_event(repo_root: &Path, event: &EmbeddingAuditEvent) -> Result<()> {
    let audit_path = repo_root.join(AUDIT_RELATIVE_PATH);
    if let Some(parent) = audit_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let line = serde_json::to_string(event)? + "\n";
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)?
        .write_all(line.as_bytes())?;
    Ok(())
}

/// Read all audit events from the JSONL file.
pub fn read_audit_events(repo_root: &Path) -> Result<Vec<EmbeddingAuditEvent>> {
    let audit_path = repo_root.join(AUDIT_RELATIVE_PATH);
    if !audit_path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(&audit_path)?;
    let mut events = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<EmbeddingAuditEvent>(line) {
            Ok(event) => events.push(event),
            Err(_) => continue, // skip malformed lines
        }
    }
    Ok(events)
}

use std::io::Write;

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProviderLocality;

    #[test]
    fn append_and_read_audit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

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

        append_audit_event(root, &event).unwrap();
        let events = read_audit_events(root).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "allow");
        assert_eq!(events[0].provider_id, "mock");
    }

    #[test]
    fn audit_no_raw_text_or_vector() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let event = EmbeddingAuditEvent {
            timestamp: "2026-06-11T00:00:00Z".into(),
            event: "block".into(),
            request_id: "req2".into(),
            provider_id: "mock".into(),
            model_id: "mock-v1".into(),
            locality: ProviderLocality::Mock,
            purpose: "query".into(),
            path: None,
            line_range: None,
            data_level: 0,
            view_kind: "query".into(),
            bytes_selected: 0,
            text_sha: "qsha".into(),
            secret_scan: "blocked:contains_secret".into(),
            policy_decision: "block".into(),
            cache_key: "key2".into(),
            outbound_attempted: false,
            reason: Some("secret_detected".into()),
        };

        append_audit_event(root, &event).unwrap();

        // Read the raw file and verify no text/vector fields
        let raw = fs::read_to_string(root.join(AUDIT_RELATIVE_PATH)).unwrap();
        assert!(!raw.contains("\"text\":"));
        assert!(!raw.contains("\"vector\":"));
        assert!(raw.contains("text_sha"));
    }

    #[test]
    fn read_empty_audit() {
        let dir = tempfile::tempdir().unwrap();
        let events = read_audit_events(dir.path()).unwrap();
        assert!(events.is_empty());
    }
}
