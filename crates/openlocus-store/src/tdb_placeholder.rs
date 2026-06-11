//! TDB (TriviumDB) placeholder store.
//!
//! This implements `StoreBackend` but returns `available=false` for all
//! operations. It is a Level 0 default-path test surface: the API shape is
//! defined, but no actual TDB dependency is used. The real adapter lives in a
//! separate `tdb_adapter` module behind the optional `tdb` Cargo feature; this
//! placeholder remains the default behavior when that feature is disabled.
//!
//! Important: TDB/TriviumDB is NOT a default dependency. This placeholder
//! exists solely to validate the store trait API against an "unavailable"
//! backend and to serve as a test surface for CLI commands.

use crate::{StoreBackend, StoreCapabilities, StoreDebug, StoreError, StoreHealth, StoreResult};
use openlocus_repo::scan::FileRecord;
use std::path::Path;

/// TDB placeholder store — always unavailable, all capabilities false.
pub struct TdbPlaceholderStore;

impl Default for TdbPlaceholderStore {
    fn default() -> Self {
        Self
    }
}

impl TdbPlaceholderStore {
    pub fn new() -> Self {
        Self
    }
}

impl StoreBackend for TdbPlaceholderStore {
    fn name(&self) -> &str {
        "tdb"
    }

    fn health(&self) -> StoreHealth {
        StoreHealth {
            available: false,
            backend: self.name().to_string(),
            capabilities: StoreCapabilities {
                metadata: false,
                chunks: false,
                lexical: false,
                vector: false,
                graph: false,
            },
            snapshot_id: None,
            error: Some(
                "TDB backend not available: feature 'tdb' is not enabled or dependency not present"
                    .into(),
            ),
        }
    }

    fn build(&mut self, _repo_root: &Path, _records: &[FileRecord]) -> StoreResult<StoreDebug> {
        Err(StoreError::NotAvailable(
            "TDB backend not available: feature 'tdb' is not enabled".into(),
        ))
    }

    fn purge(&mut self) -> StoreResult<()> {
        Err(StoreError::NotAvailable(
            "TDB backend not available: feature 'tdb' is not enabled".into(),
        ))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StoreBackend;

    #[test]
    fn tdb_placeholder_health_unavailable() {
        let store = TdbPlaceholderStore::new();
        let health = store.health();
        assert!(!health.available);
        assert_eq!(health.backend, "tdb");
        assert!(health.error.is_some());
    }

    #[test]
    fn tdb_placeholder_build_unsupported() {
        let mut store = TdbPlaceholderStore::new();
        let result = store.build(Path::new("."), &[]);
        assert!(result.is_err());
        match result.unwrap_err() {
            StoreError::NotAvailable(msg) => assert!(msg.contains("not available")),
            e => panic!("expected NotAvailable, got {:?}", e),
        }
    }

    #[test]
    fn tdb_placeholder_purge_unsupported() {
        let mut store = TdbPlaceholderStore::new();
        let result = store.purge();
        assert!(result.is_err());
    }

    #[test]
    fn tdb_placeholder_all_caps_false() {
        let store = TdbPlaceholderStore::new();
        let health = store.health();
        let caps = &health.capabilities;
        assert!(!caps.metadata);
        assert!(!caps.chunks);
        assert!(!caps.lexical);
        assert!(!caps.vector);
        assert!(!caps.graph);
    }
}
