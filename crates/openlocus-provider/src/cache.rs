//! Embedding cache key computation.
//!
//! Cache key: canonical stable string domain-separated with
//! provider_id + model_id + dimensions + view_kind + text_sha +
//! source_content_sha + policy_mode + data_level + schema_version.
//! Prefix `emb1:` + blake3 hex.

use crate::model::CACHE_KEY_SCHEMA_VERSION;

/// Compute a cache key for an embedding.
#[allow(clippy::too_many_arguments)]
pub fn compute_cache_key(
    provider_id: &str,
    model_id: &str,
    dimensions: usize,
    view_kind: &str,
    text_sha: &str,
    source_content_sha: &str,
    policy_mode: &str,
    data_level: u8,
) -> String {
    let canonical = format!(
        "{}:{}:{}:{}:{}:{}:{}:{}:{}",
        CACHE_KEY_SCHEMA_VERSION,
        provider_id,
        model_id,
        dimensions,
        view_kind,
        text_sha,
        source_content_sha,
        policy_mode,
        data_level,
    );
    let hash = blake3::hash(canonical.as_bytes());
    format!("emb1:{}", hash.to_hex())
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_stable() {
        let k1 = compute_cache_key(
            "mock",
            "mock-v1",
            32,
            "metadata",
            "sha1",
            "ssha1",
            "local_only",
            0,
        );
        let k2 = compute_cache_key(
            "mock",
            "mock-v1",
            32,
            "metadata",
            "sha1",
            "ssha1",
            "local_only",
            0,
        );
        assert_eq!(k1, k2);
        assert!(k1.starts_with("emb1:"));
    }

    #[test]
    fn cache_key_differs_for_different_text_sha() {
        let k1 = compute_cache_key(
            "mock",
            "mock-v1",
            32,
            "metadata",
            "sha1",
            "ssha1",
            "local_only",
            0,
        );
        let k2 = compute_cache_key(
            "mock",
            "mock-v1",
            32,
            "metadata",
            "sha2",
            "ssha1",
            "local_only",
            0,
        );
        assert_ne!(k1, k2);
    }

    #[test]
    fn cache_key_differs_for_different_model() {
        let k1 = compute_cache_key(
            "mock",
            "mock-v1",
            32,
            "metadata",
            "sha1",
            "ssha1",
            "local_only",
            0,
        );
        let k2 = compute_cache_key(
            "mock",
            "mock-v2",
            32,
            "metadata",
            "sha1",
            "ssha1",
            "local_only",
            0,
        );
        assert_ne!(k1, k2);
    }

    #[test]
    fn cache_key_differs_for_different_view_kind() {
        let k1 = compute_cache_key(
            "mock",
            "mock-v1",
            32,
            "metadata",
            "sha1",
            "ssha1",
            "local_only",
            0,
        );
        let k2 = compute_cache_key(
            "mock",
            "mock-v1",
            32,
            "snippet",
            "sha1",
            "ssha1",
            "local_only",
            0,
        );
        assert_ne!(k1, k2);
    }

    #[test]
    fn cache_key_differs_for_different_source_sha() {
        let k1 = compute_cache_key(
            "mock",
            "mock-v1",
            32,
            "metadata",
            "sha1",
            "ssha1",
            "local_only",
            0,
        );
        let k2 = compute_cache_key(
            "mock",
            "mock-v1",
            32,
            "metadata",
            "sha1",
            "ssha2",
            "local_only",
            0,
        );
        assert_ne!(k1, k2);
    }

    #[test]
    fn cache_key_differs_for_different_data_level() {
        let k1 = compute_cache_key(
            "mock",
            "mock-v1",
            32,
            "metadata",
            "sha1",
            "ssha1",
            "local_only",
            0,
        );
        let k2 = compute_cache_key(
            "mock",
            "mock-v1",
            32,
            "metadata",
            "sha1",
            "ssha1",
            "local_only",
            1,
        );
        assert_ne!(k1, k2);
    }
}
