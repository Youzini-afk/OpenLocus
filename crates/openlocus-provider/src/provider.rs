//! Embedding provider trait and implementations.

use crate::model::{ProviderLocality, ProviderMetadata};
use anyhow::Result;

/// Trait for embedding providers.
pub trait EmbeddingProvider: Send + Sync {
    /// Return static metadata about this provider.
    fn metadata(&self) -> &ProviderMetadata;

    /// Embed a single input text, returning a vector.
    fn embed(&self, text: &str, text_sha: &str) -> Result<Vec<f32>>;
}

/// Disabled provider: always returns unavailable.
pub struct DisabledEmbeddingProvider {
    metadata: ProviderMetadata,
}

impl DisabledEmbeddingProvider {
    pub fn new() -> Self {
        Self {
            metadata: ProviderMetadata {
                provider_id: "disabled".into(),
                model_id: "disabled-v0".into(),
                dimensions: 0,
                locality: ProviderLocality::Disabled,
                max_data_level: 0,
                outbound_possible: false,
            },
        }
    }
}

impl Default for DisabledEmbeddingProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl EmbeddingProvider for DisabledEmbeddingProvider {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }

    fn embed(&self, _text: &str, _text_sha: &str) -> Result<Vec<f32>> {
        anyhow::bail!("provider disabled: no embedding available")
    }
}

/// Create a provider by name. Only "mock" and "disabled" are supported in R13.
pub fn create_provider(name: &str) -> Result<Box<dyn EmbeddingProvider>> {
    match name {
        "mock" => Ok(Box::new(crate::mock::MockEmbeddingProvider::new())),
        "disabled" => Ok(Box::new(DisabledEmbeddingProvider::new())),
        other => anyhow::bail!(
            "unknown provider '{}'; supported providers: mock, disabled",
            other
        ),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_provider_returns_error() {
        let p = DisabledEmbeddingProvider::new();
        assert_eq!(p.metadata().provider_id, "disabled");
        assert!(!p.metadata().locality.is_available());
        let result = p.embed("test", "sha");
        assert!(result.is_err());
    }

    #[test]
    fn create_provider_mock() {
        let p = create_provider("mock").unwrap();
        assert_eq!(p.metadata().provider_id, "mock");
    }

    #[test]
    fn create_provider_disabled() {
        let p = create_provider("disabled").unwrap();
        assert_eq!(p.metadata().provider_id, "disabled");
    }

    #[test]
    fn create_provider_unknown() {
        let result = create_provider("openai");
        assert!(result.is_err());
    }
}
