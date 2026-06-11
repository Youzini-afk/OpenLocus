//! OpenLocus Semantic Graph — Level0 deterministic scaffold.
//!
//! Builds a simple dependency/test/config graph from scan_repo records.
//! Graph edges are deterministic and explainable. Graph results must be
//! materialized into Evidence via current filesystem validation; graph
//! candidates cannot directly substitute for Evidence.
//!
//! Default depth=1; depth>1 is not implemented and returns an error.
//! No LSP, SCIP, or LLM is used. All parsing is simple line-based heuristics.

pub mod graph;
pub mod materialize;

pub use graph::{EdgeKind, GraphBuildResult, GraphCapabilities, GraphEdge, GraphNode};
pub use materialize::materialize_graph_evidence;
