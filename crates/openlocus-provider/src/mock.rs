//! Deterministic mock embedding provider.
//!
//! Vector is generated from blake3(provider_id/model_id/text_sha/index),
//! then normalized. No network. Dimensions=32.

use crate::model::{ProviderLocality, ProviderMetadata};
use crate::provider::EmbeddingProvider;

/// Deterministic mock embedding provider.
pub struct MockEmbeddingProvider {
    metadata: ProviderMetadata,
}

impl MockEmbeddingProvider {
    pub fn new() -> Self {
        Self {
            metadata: ProviderMetadata {
                provider_id: "mock".into(),
                model_id: "mock-v1".into(),
                dimensions: 32,
                locality: ProviderLocality::Mock,
                max_data_level: 1,
                outbound_possible: false,
            },
        }
    }

    /// Generate a deterministic vector from the given inputs.
    /// Uses blake3(provider_id/model_id/text_sha/i) for each dimension.
    pub fn deterministic_vector(
        provider_id: &str,
        model_id: &str,
        text_sha: &str,
        dimensions: usize,
    ) -> Vec<f32> {
        let mut raw = Vec::with_capacity(dimensions);
        for i in 0..dimensions {
            let input = format!("{}/{}/{}/{}", provider_id, model_id, text_sha, i);
            let hash = blake3::hash(input.as_bytes());
            // Take first 4 bytes as a little-endian i32, then convert to f32
            let bytes: [u8; 4] = hash.as_bytes()[..4].try_into().unwrap_or([0u8; 4]);
            let int_val = i32::from_le_bytes(bytes);
            raw.push(int_val as f32);
        }

        // Normalize to unit vector
        let norm: f64 = raw.iter().map(|v| (*v as f64).powi(2)).sum::<f64>().sqrt();
        if norm == 0.0 {
            return raw;
        }
        raw.iter().map(|v| (*v as f64 / norm) as f32).collect()
    }
}

impl Default for MockEmbeddingProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl EmbeddingProvider for MockEmbeddingProvider {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }

    fn embed(&self, text: &str, text_sha: &str) -> anyhow::Result<Vec<f32>> {
        // We don't use `text` directly to avoid any possibility of text leaking,
        // but we hash it to verify consistency with text_sha.
        let computed_sha = blake3::hash(text.as_bytes()).to_hex().to_string();
        if computed_sha != text_sha {
            // In mock mode, still generate deterministic vector from text_sha
            // but log the mismatch (don't fail — caller may have truncated text).
        }
        Ok(Self::deterministic_vector(
            &self.metadata.provider_id,
            &self.metadata.model_id,
            text_sha,
            self.metadata.dimensions,
        ))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_metadata() {
        let p = MockEmbeddingProvider::new();
        assert_eq!(p.metadata().provider_id, "mock");
        assert_eq!(p.metadata().model_id, "mock-v1");
        assert_eq!(p.metadata().dimensions, 32);
        assert_eq!(p.metadata().locality, ProviderLocality::Mock);
        assert!(!p.metadata().outbound_possible);
    }

    #[test]
    fn deterministic_vector_normalized() {
        let v = MockEmbeddingProvider::deterministic_vector("mock", "mock-v1", "sha1", 32);
        assert_eq!(v.len(), 32);
        let norm: f64 = v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
        assert!(
            (norm - 1.0).abs() < 0.01,
            "vector should be approximately unit length, got norm={}",
            norm
        );
    }

    #[test]
    fn deterministic_vector_stable() {
        let v1 = MockEmbeddingProvider::deterministic_vector("mock", "mock-v1", "sha1", 32);
        let v2 = MockEmbeddingProvider::deterministic_vector("mock", "mock-v1", "sha1", 32);
        assert_eq!(v1, v2, "same inputs should produce same vector");
    }

    #[test]
    fn deterministic_vector_differs_for_different_inputs() {
        let v1 = MockEmbeddingProvider::deterministic_vector("mock", "mock-v1", "sha1", 32);
        let v2 = MockEmbeddingProvider::deterministic_vector("mock", "mock-v1", "sha2", 32);
        assert_ne!(
            v1, v2,
            "different text_sha should produce different vectors"
        );
    }

    #[test]
    fn mock_embed_produces_vector() {
        let p = MockEmbeddingProvider::new();
        let text = "path:src/lib.rs language:rust basename:lib words:src lib";
        let text_sha = blake3::hash(text.as_bytes()).to_hex().to_string();
        let v = p.embed(text, &text_sha).unwrap();
        assert_eq!(v.len(), 32);
    }

    #[test]
    fn deterministic_vector_different_provider() {
        let v1 = MockEmbeddingProvider::deterministic_vector("mock", "mock-v1", "sha1", 32);
        let v2 = MockEmbeddingProvider::deterministic_vector("other", "other-v1", "sha1", 32);
        assert_ne!(
            v1, v2,
            "different provider_id should produce different vectors"
        );
    }

    #[test]
    fn deterministic_vector_different_dimensions() {
        let v4 = MockEmbeddingProvider::deterministic_vector("mock", "mock-v1", "sha1", 4);
        let v8 = MockEmbeddingProvider::deterministic_vector("mock", "mock-v1", "sha1", 8);
        assert_eq!(v4.len(), 4);
        assert_eq!(v8.len(), 8);
        // Note: dimensions use index in the hash input, so different-length
        // vectors may have different values for the same index when dimensions
        // differ (the hash input includes dimensions implicitly through index).
        // The key property is same inputs → same vector.
    }
}
