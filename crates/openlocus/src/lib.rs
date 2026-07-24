//! Local code evidence with current-source verification.

mod engine;
mod index;
mod model;
mod policy;
mod rank;
mod repo;

pub use engine::{Engine, default_state_root};
pub use model::{
    BuildSummary, Channel, Citation, CitationValidation, Evidence, Freshness, IndexStatus,
    QueryDiagnostics, QueryRequest, QueryResult, QueryStatus, UpdateSummary,
};
pub use policy::Policy;
