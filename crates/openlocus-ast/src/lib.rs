//! AST-bounded chunking and symbol extraction using Tree-sitter.
//!
//! This crate provides experimental AST-aware chunking and symbol extraction
//! for Rust, Python, JavaScript, and TypeScript source files.
//!
//! **This is an R8 experimental feature.** Line-based chunking remains the
//! default. AST mode is opt-in only. Quality lift is not proven.

mod chunk;
mod symbol;

pub use chunk::*;
pub use symbol::*;
