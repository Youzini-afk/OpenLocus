//! Policy gate for embedding inputs.
//!
//! - Remote providers require policy.remote.allow && policy.remote.allow_embedding
//!   && provider_id in allowed_providers && data_level <= policy.remote.max_data_level.
//! - Mock/Local providers allow without remote.allow, but still require
//!   data_level <= 1 and secret gate if policy.secrets.block_on_match.
//! - Secret gate blocks tokens containing SECRET/TOKEN/PASSWORD/API_KEY/PRIVATE_KEY,
//!   sk_, ghp_, AKIA, or high-entropy-ish long mixed strings.

use crate::model::{EmbedInput, ProviderDecision, ProviderMetadata};
use openlocus_core::Policy;

/// Simple deterministic secret scanner for R13.
/// Returns "clean" or "blocked:<reason>".
pub fn scan_secrets(text: &str) -> String {
    let upper = text.to_uppercase();

    // Check for known secret markers
    let markers = ["SECRET", "TOKEN", "PASSWORD", "API_KEY", "PRIVATE_KEY"];
    for marker in markers {
        if upper.contains(marker) {
            return format!("blocked:contains_{}", marker.to_lowercase());
        }
    }

    // Check for known secret prefixes
    let prefixes = ["sk_", "ghp_", "AKIA"];
    for prefix in prefixes {
        if text.contains(prefix) {
            return format!("blocked:prefix_{}", prefix.replace('_', ""));
        }
    }

    // Simple high-entropy check: long strings (>32 chars) with mixed char classes
    // Split into tokens and check each
    for token in text.split_whitespace() {
        if token.len() >= 32 && is_high_entropy(token) {
            return "blocked:high_entropy_string".into();
        }
    }

    "clean".into()
}

/// Check if a string appears to be high-entropy (mixed character classes).
fn is_high_entropy(s: &str) -> bool {
    let has_lower = s.chars().any(|c| c.is_ascii_lowercase());
    let has_upper = s.chars().any(|c| c.is_ascii_uppercase());
    let has_digit = s.chars().any(|c| c.is_ascii_digit());
    let has_special = s.chars().any(|c| !c.is_alphanumeric());

    // At least 3 of 4 character classes suggests high entropy
    let classes = [has_lower, has_upper, has_digit, has_special]
        .iter()
        .filter(|&&x| x)
        .count();
    classes >= 3
}

/// Gate an embedding input against policy.
pub fn gate_embed_input(
    policy: &Policy,
    metadata: &ProviderMetadata,
    input: &EmbedInput,
) -> ProviderDecision {
    // 1. Check provider availability
    if !metadata.locality.is_available() {
        return ProviderDecision {
            allowed: false,
            reason: "provider_disabled".into(),
            secret_scan: "skipped".into(),
        };
    }

    // 2. Secret scan
    let secret_scan = scan_secrets(&input.text);
    if secret_scan != "clean" && policy.secrets.block_on_match {
        return ProviderDecision {
            allowed: false,
            reason: format!("secret_blocked:{}", secret_scan),
            secret_scan,
        };
    }

    // 3. Remote-specific checks
    if metadata.locality.is_remote() {
        if !policy.remote.allow {
            return ProviderDecision {
                allowed: false,
                reason: "remote_not_allowed_by_policy".into(),
                secret_scan,
            };
        }
        if !policy.remote.allow_embedding {
            return ProviderDecision {
                allowed: false,
                reason: "remote_embedding_not_allowed_by_policy".into(),
                secret_scan,
            };
        }
        if !policy
            .remote
            .allowed_providers
            .contains(&metadata.provider_id)
        {
            return ProviderDecision {
                allowed: false,
                reason: format!("provider_{}_not_in_allowed_list", metadata.provider_id),
                secret_scan,
            };
        }
        if input.data_level > policy.remote.max_data_level {
            return ProviderDecision {
                allowed: false,
                reason: format!(
                    "data_level_{}_exceeds_remote_max_{}",
                    input.data_level, policy.remote.max_data_level
                ),
                secret_scan,
            };
        }
    }

    // 4. Mock/Local: data_level must be <= 1
    if !metadata.locality.is_remote() && input.data_level > 1 {
        return ProviderDecision {
            allowed: false,
            reason: format!("data_level_{}_exceeds_local_max_1", input.data_level),
            secret_scan,
        };
    }

    // 5. Enforce data_level <= metadata.max_data_level
    if input.data_level > metadata.max_data_level {
        return ProviderDecision {
            allowed: false,
            reason: format!(
                "data_level_{}_exceeds_provider_max_{}",
                input.data_level, metadata.max_data_level
            ),
            secret_scan,
        };
    }

    // 6. Allowed
    ProviderDecision {
        allowed: true,
        reason: "allowed".into(),
        secret_scan,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProviderLocality;

    fn mock_metadata(locality: ProviderLocality) -> ProviderMetadata {
        ProviderMetadata {
            provider_id: "mock".into(),
            model_id: "mock-v1".into(),
            dimensions: 32,
            locality,
            max_data_level: 1,
            outbound_possible: false,
        }
    }

    fn mock_input(text: &str, data_level: u8) -> EmbedInput {
        EmbedInput {
            input_id: "test:1-1".into(),
            path: "test.rs".into(),
            start_line: 1,
            end_line: 1,
            source_content_sha: "sha".into(),
            language: "rust".into(),
            view_kind: "metadata".into(),
            text: text.into(),
            text_sha: "tsha".into(),
            data_level,
            policy_mode: "local_only".into(),
            purpose: "index".into(),
        }
    }

    #[test]
    fn scan_secrets_clean() {
        assert_eq!(scan_secrets("hello world"), "clean");
        assert_eq!(scan_secrets("fn main() {}"), "clean");
    }

    #[test]
    fn scan_secrets_markers() {
        assert!(scan_secrets("my SECRET value").contains("blocked"));
        assert!(scan_secrets("TOKEN=abc").contains("blocked"));
        assert!(scan_secrets("PASSWORD=xyz").contains("blocked"));
        assert!(scan_secrets("API_KEY=123").contains("blocked"));
        assert!(scan_secrets("PRIVATE_KEY=pk").contains("blocked"));
    }

    #[test]
    fn scan_secrets_prefixes() {
        assert!(scan_secrets("sk_abc123").contains("blocked"));
        assert!(scan_secrets("ghp_def456").contains("blocked"));
        assert!(scan_secrets("AKIA789012").contains("blocked"));
    }

    #[test]
    fn scan_secrets_high_entropy() {
        // Long mixed-class string (>= 32 chars)
        assert!(scan_secrets("aB3!xY7$kL9#mN2@pQ5&rT8*wU4^vZ6!").contains("blocked"));
    }

    #[test]
    fn gate_allows_mock_data_level_0() {
        let policy = Policy::default();
        let meta = mock_metadata(ProviderLocality::Mock);
        let input = mock_input("hello world", 0);
        let decision = gate_embed_input(&policy, &meta, &input);
        assert!(decision.allowed);
    }

    #[test]
    fn gate_blocks_disabled_provider() {
        let policy = Policy::default();
        let meta = ProviderMetadata {
            provider_id: "disabled".into(),
            model_id: "disabled-v0".into(),
            dimensions: 0,
            locality: ProviderLocality::Disabled,
            max_data_level: 0,
            outbound_possible: false,
        };
        let input = mock_input("hello", 0);
        let decision = gate_embed_input(&policy, &meta, &input);
        assert!(!decision.allowed);
        assert!(decision.reason.contains("provider_disabled"));
    }

    #[test]
    fn gate_blocks_remote_by_default() {
        let policy = Policy::default();
        let meta = ProviderMetadata {
            provider_id: "openai".into(),
            model_id: "text-embedding-3-small".into(),
            dimensions: 1536,
            locality: ProviderLocality::Remote,
            max_data_level: 1,
            outbound_possible: true,
        };
        let input = mock_input("hello", 0);
        let decision = gate_embed_input(&policy, &meta, &input);
        assert!(!decision.allowed);
        assert!(decision.reason.contains("remote_not_allowed"));
    }

    #[test]
    fn gate_blocks_data_level_above_1_local() {
        let policy = Policy::default();
        let meta = mock_metadata(ProviderLocality::Mock);
        let input = mock_input("hello", 2);
        let decision = gate_embed_input(&policy, &meta, &input);
        assert!(!decision.allowed);
        assert!(decision.reason.contains("data_level"));
    }

    #[test]
    fn gate_blocks_secret_with_block_on_match() {
        let policy = Policy::default(); // block_on_match = true by default
        let meta = mock_metadata(ProviderLocality::Mock);
        let input = mock_input("my SECRET value is sk_abc123def", 0);
        let decision = gate_embed_input(&policy, &meta, &input);
        assert!(!decision.allowed);
        assert!(decision.reason.contains("secret_blocked"));
    }

    #[test]
    fn gate_allows_clean_text_with_block_on_match() {
        let policy = Policy::default();
        let meta = mock_metadata(ProviderLocality::Mock);
        let input = mock_input("path:src/lib.rs language:rust", 0);
        let decision = gate_embed_input(&policy, &meta, &input);
        assert!(decision.allowed);
    }

    #[test]
    fn gate_blocks_data_level_above_metadata_max() {
        let policy = Policy::default();
        let meta = ProviderMetadata {
            provider_id: "mock".into(),
            model_id: "mock-v1".into(),
            dimensions: 32,
            locality: ProviderLocality::Mock,
            max_data_level: 0, // provider only allows data_level=0
            outbound_possible: false,
        };
        let input = mock_input("hello", 1); // input has data_level=1 > max_data_level=0
        let decision = gate_embed_input(&policy, &meta, &input);
        assert!(!decision.allowed);
        assert!(
            decision.reason.contains("data_level"),
            "should block data_level > metadata.max_data_level: {}",
            decision.reason
        );
    }
}
