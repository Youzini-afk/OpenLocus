# OpenLocus

OpenLocus is the product codebase for local code evidence retrieval. It turns a query into source spans that have been re-read and hash-verified against the current working tree before they are returned.

This repository is intentionally small and unreleased. OpenLocus-Lab remains the research system; this repository contains only the product path supported by that work: persistent BM25 plus case-sensitive literal search. Dense retrieval, graph retrieval, symbol search, remote providers, daemons, compatibility adapters, and binary distribution are not part of the current product.

## Use from source

Build the index once:

```console
cargo run -p openlocus-cli -- --root . index build
```

Query, read, and update it:

```console
cargo run -p openlocus-cli -- --root . query "current-source verification"
cargo run -p openlocus-cli -- --root . read crates/openlocus/src/engine.rs:20-60
cargo run -p openlocus-cli -- --root . index update crates/openlocus/src/engine.rs
cargo run -p openlocus-cli -- --root . index status
```

All successful commands emit JSON. Errors are written to stderr and return a non-zero exit code. When `--root` is omitted, the CLI requires a parent containing `.git`; it does not silently treat an arbitrary directory as a repository.

Index data is stored outside the source tree in the operating-system cache directory. Pass `--state-root PATH` to choose another non-overlapping directory. OpenLocus never creates traces, caches, or index files inside the source tree.

## Evidence contract

Every returned evidence item contains:

- a normalized repository-relative path and line range;
- the current BLAKE3 content hash and exact excerpt;
- retrieval reasons and contributing channels;
- `freshness: "verified_current"`.

Indexed candidates are not evidence. OpenLocus reopens the source file, checks its hash and range, and only then creates evidence. Known stale or invalid candidates are dropped and make the query status `partial`; stale content is never returned.

BM25 and literal results are combined with deterministic reciprocal-rank fusion. Ties and final ordering have stable path/line fallbacks, so identical source and state produce identical output.

## Index lifecycle

```console
openlocus index build
openlocus index update PATH [PATH ...]
openlocus index status
openlocus index purge
```

`build` replaces the entire derived index. `update` changes only the named paths and also handles deletions. Index format, source-root, policy, or generation mismatches require a rebuild; development formats are deliberately not migrated or read through compatibility layers.

## Policy

An optional `openlocus.toml` at the source root controls scanning:

```toml
include = ["**/*"]
exclude = [".git/**", ".openlocus/**", "target/**", "node_modules/**", "dist/**", ".env*", "**/*.pem"]
include_gitignored = false
max_file_bytes = 2097152
```

Missing policy uses those defaults. Malformed policy, symbolic-link policy files, and unsafe size limits fail closed. The minimal matcher accepts exact paths plus `dir/**`, `**/*.ext`, `*.ext`, `prefix*`, and `**/*` forms.

Direct reads and citation validation obey the same policy. Source symlink escapes, path traversal, binary files, and oversized files are rejected or skipped.

## Citation validation

Provide a JSON array containing only citation identity:

```json
[
  {
    "path": "crates/openlocus/src/lib.rs",
    "start_line": 1,
    "end_line": 5,
    "content_sha": "..."
  }
]
```

```console
cargo run -p openlocus-cli -- --root . citations validate citations.json
```

The result validates the path, current content hash, policy, and line range. `Evidence` itself is output-only: external callers cannot construct an object marked as verified.

## Library

The CLI is a thin wrapper over the `openlocus` crate:

```rust
use openlocus::{Engine, QueryRequest, default_state_root};

let source = std::env::current_dir()?;
let state = default_state_root(&source)?;
let mut engine = Engine::open(&source, state)?;
engine.build_index()?;
let result = engine.query(QueryRequest::new("needle"))?;
# Ok::<(), anyhow::Error>(())
```

## Development

```console
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
```

The two workspace crates are the engine and its CLI. Version `0.0.0` means schemas and APIs may be replaced directly until the project is explicitly declared released; no deprecated aliases or migration shims are maintained.

Licensed under AGPL-3.0-only.
