//! OpenLocus Provider — safe embedding / LLM-derived indexing bakeoff scaffold.
//!
//! R13 design constraints:
//! - No real remote calls, no API keys, no model downloads.
//! - Default build is fully local; EvidenceCore is unchanged.
//! - Dense/mock/derived hints produce candidate StoreHits only;
//!   final Evidence must go through `openlocus_store::materialize_evidence`.
//! - Audit/cache/vector store never store raw snippet text in audit;
//!   vector store stores path/range/source_content_sha/language/vector only.
//! - Quality claims limited to mock integration; no real semantic gain claimed.

pub mod audit;
pub mod cache;
pub mod dense_store;
pub mod gate;
pub mod mock;
pub mod model;
pub mod provider;
