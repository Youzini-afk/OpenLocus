# OpenLocus R0-R12 Research Report

Date: 2026-06-11  
Repository: `https://github.com/Youzini-afk/OpenLocus.git`  
Scope: continuous evidence-gated research implementation from the initial design into a working local retrieval kernel prototype, now including the R12 real-repo incremental robustness benchmark milestone.

## Executive summary

OpenLocus now has a working Rust prototype that validates the core design direction: **all agent-facing code facts must be evidence-backed, citation-checkable, and freshness-aware**.

The implementation completed twelve evidence-gated checkpoints:

| Commit | Stage | Result |
|---|---|---|
| `9779e9f` | R0/R1 local evidence kernel | `EvidenceCore`, local read/search/scan, trace JSONL, citation validation, context-lite. |
| `6d8a274` | R2 retrieval bakeoff | Regex/text, Tantivy BM25, heuristic symbol search, RRF fusion, metrics harness. |
| `f488d08` | R3 storage scaffold | Store traits, StoreHit materialization gate, ConservativeChunkStore, TDB placeholder. |
| `925ed38` | R4 derived indexing safety | `DerivedIndexView`, deterministic rule generator, policy/freshness gates, JSONL store. |
| `83ae02e` | R5 graph scaffold | Deterministic depth=1 graph, StoreHit materialization, impact/test-selection smoke. |
| `fb7104e` | R6 fast context prototype | 4-turn deterministic context orchestration with EvidencePack-compatible output. |
| `43135ed` | R7 persistent BM25 index | Persistent Tantivy index with mandatory manifest/policy gates, safe hit validation, and warm benchmark. |
| R8 checkpoint | R8 AST chunking/symbols | Tree-sitter AST-bounded chunking and AST symbol search as explicit experimental modes. |
| R9 checkpoint | R9 AST quality bakeoff | Persistent BM25 quality comparison: line vs ast on R2 fixture. AST improves span precision but regresses FileRecall@5. |
| R10 checkpoint | R10 incremental index + dirty summary + synthetic SLO | Dirty summary (manifest-vs-current scan), file-level incremental update, context-lite dirty integration, synthetic 1000-file SLO benchmark. 48/48 incremental smoke checks passed. |
| R11 checkpoint | R11 TDB Level0 adapter probe | Feature-gated TriviumDB 0.7.0 adapter behind `tdb` Cargo feature. TdbChunkStore with dim=1 smoke probe, metadata+chunks only, marker-based purge, materialization conformance. 11/11 adapter checks passed. No default dependency. No retrieval quality claim. |
| R12 checkpoint | R12 real-repo incremental robustness benchmark | eval/real_repo_incremental_bench.py on temp copy of OpenLocus repo. modify/add/delete/rename/policy_exclude/batch workloads pass 149/149 hard safety checks. total_invalid_citations=0. No stale VerifiedCurrent violations. Growth catastrophic guard passed (not bounded proof). Latency measured as report-only. Level0 one real-repo sample only. |

Final verification snapshot:

```text
Rust tests: 193 passed
fmt: clean
clippy: clean with -D warnings
Remote dependency: none
LLM dependency: none
TDB dependency: optional only (behind `tdb` feature; not in default build)
Safety evals: storage, derived, graph, fast-context, persistent-index, AST-chunking all passing their Level0 gates; AST quality bakeoff safety checks 16/16 passed; incremental index smoke 48/48 checks passed; synthetic SLO bench 0 invalid citations; TDB adapter probe 11/11 checks passed; real-repo incremental bench 149/149 hard safety checks passed
```

The most important research outcome is not that retrieval quality is solved. It is that the project now has a **safe experimental harness** where BM25, graph, TDB, LLM-derived views, dense embeddings, and future planners can be tested without weakening the EvidenceCore contract.

## Research methodology

We followed the agreed continuous research loop:

```text
Hypothesis -> Prototype -> Bakeoff / Safety Eval -> Review -> Fix blockers -> Push -> Next stage
```

No stage was promoted just because it ran. Each stage was reviewed against the design constraints:

- do not mutate `EvidenceCore` for research features;
- keep all authoritative facts tied to `path + line_range + content_sha`;
- prefer narrow spans over broad summaries;
- validate current file state before declaring evidence `VerifiedCurrent`;
- keep remote/LLM/TDB features outside the core until gates prove safety and value;
- record caveats rather than overclaim quality.

Several early implementations were deliberately corrected before commit: regex span widening, BM25 `*` fallback, graph hand-built evidence, R6 weak final citation validation, and R4 data-level/policy gaps. These corrections are strong evidence that the gate discipline is useful.

## Implemented architecture

Current workspace structure:

```text
crates/openlocus-core       EvidenceCore, EvidenceMeta, EvidencePack, Policy, trace types
crates/openlocus-repo       path safety, read, scan, content hashing
crates/openlocus-retrieval  regex/text search, BM25, symbol search, RRF fusion
crates/openlocus-ast        Tree-sitter AST chunk and symbol extraction scaffold
crates/openlocus-index      persistent Tantivy BM25 index, manifest, warm benchmark handle
crates/openlocus-store      Store traits, StoreHit materialization, conservative store, TDB placeholder
crates/openlocus-derived    DerivedIndexView safety scaffold
crates/openlocus-graph      deterministic graph scaffold + graph materialization
crates/openlocus-context    Fast Context Level0 rule loop
crates/openlocus-cli        user-facing CLI
eval/                       retrieval/storage/derived/graph/fast-context/persistent-index/AST smoke and scoring scripts
docs/                       research log, summary, agent guide, final report
```

Representative commands:

```bash
openlocus read <path[:start-end]> --json
openlocus scan --json
openlocus search regex <pattern> --json
openlocus search bm25 <query> --json
openlocus search bm25 <query> --index persistent --json
openlocus search symbol <name> --mode regex|ast|auto --json
openlocus retrieve <query> --channels regex,bm25,symbol --json
openlocus citations validate <file.json> --json
openlocus store status conservative --json
openlocus derived build all --experimental --json
openlocus graph build --json
openlocus impact <path> --json
openlocus tests --path <path> --json
openlocus fast-context <query> --json
openlocus index build --chunk-strategy line|ast --json
openlocus index status --json
openlocus index dirty --json
openlocus index validate --json
openlocus index update --dirty --json
openlocus index update --path <path> --json
openlocus index purge --json
openlocus bench warm --dataset fixtures/r2.jsonl --iterations 3 --json
```

## Stage results

### R0/R1 — local evidence kernel

Goal: establish a trustworthy local evidence contract before adding semantic retrieval.

Implemented:

- `EvidenceCore`: stable minimal contract (`path`, `start_line`, `end_line`, `content_sha`, `score`, `why`, `channels`).
- Optional `EvidenceMeta`: freshness, language, excerpt, score parts, symbol metadata.
- Path validation with absolute path rejection, `..` rejection, and symlink escape protection.
- `read`, `scan`, regex/text search, command trace JSONL, context-lite files.
- Citation validation for single evidence, arrays, and `{ evidence: [...] }` objects.

Key finding:

- Evidence precision problems appear immediately. The first regex implementation returned over-wide spans for multiple distant matches; this would have harmed token waste and Span F0.5. It was fixed to return one narrow evidence span per matching line.

Status: passed initial local-only gate.

### R2 — retrieval method bakeoff

Goal: compare local retrieval channels without dense, graph, TDB, or LLM indexing.

Implemented:

- Tantivy BM25 over bounded line chunks.
- Line-level query-token scoring and no-overlap skip.
- Stale hash checks for BM25 hits.
- Heuristic symbol search for Rust/Python/TS/JS/Go-style definitions.
- RRF fusion that merges same spans and keeps narrower overlapping spans.
- Evaluation metrics: FileRecall, MRR, LinePrecision/Recall, SpanF0.5, token waste, wrong-span rate, structural validity, and citation validity. The Python scorer verifies path/range and verifies BLAKE3 hashes when optional Python `blake3` is installed; the Rust CLI validator performs hash/range/excerpt-backed citation validation.

Current self-referential fixture results:

| Metric | regex | bm25 | symbol | rrf |
|---|---:|---:|---:|---:|
| FileRecall@1 | 0.21 | 0.39 | 0.39 | 0.39 |
| FileRecall@5 | 0.36 | 0.86 | 0.39 | 0.82 |
| FileRecall@10 | 0.50 | 0.86 | 0.39 | 0.82 |
| MRR | 0.29 | 0.58 | 0.39 | 0.56 |
| SpanF0.5@10 | 0.035 | 0.043 | 0.064 | 0.057 |
| Python scorer citation validity | 1.0 | 1.0 | 1.0 | 1.0 |

The current aggregated R2 evidence output was additionally validated with the Rust CLI citation validator with `0` invalid citations, using hash/range/excerpt checks.

Key finding:

- BM25 improves file-level recall on this fixture, but line/span precision remains weak. The next quality bottleneck is chunking/query rewriting/span targeting, not just adding more recall channels.

Status: passed oracle review as a local retrieval baseline, not a general benchmark claim.

### R3 — storage scaffold and TDB test surface

Goal: introduce backend abstraction without allowing storage to become an evidence authority.

Implemented:

- Store traits and capability reporting.
- `StoreHit -> materialize_evidence()` gate.
- Conservative in-memory chunk store.
- TDB placeholder backend: explicit unavailable/unsupported status, no hidden dependency.
- Storage smoke eval.

Key safety property:

```text
Store backends never directly produce authoritative Evidence.
They produce StoreHit candidates, which must be materialized against the current filesystem.
```

Materialization gate rejects:

- empty `content_sha`;
- path traversal / symlink escape;
- stale hashes;
- invalid ranges;
- inconsistent TOCTOU reads.

TDB conclusion:

- TDB is in the test surface, but only as `Level0 placeholder` for now. A real adapter should be optional, feature-gated, and judged by the same conformance tests before it can become an optional backend.

Status: passed Level0 conformance.

### R4 — LLM-derived indexing safety scaffold

Goal: explore LLM indexing shape without connecting any real LLM or remote provider.

Implemented:

- `DerivedIndexView` model.
- Deterministic rule generator for low-risk view kinds: `chunk_summary`, `symbol_tags`, `query_aliases`.
- Policy/data-level gate: Level0 hard-gates `data_level <= 1`.
- High-risk kinds (`candidate_edge`, `bug_symptom_hint`) blocked by default.
- Source citation/freshness validation.
- JSONL store with parse error reporting and purge.
- Secret-like token filtering for obvious patterns.
- Derived safety eval with stale mutation, policy-excluded files, corrupt JSONL, and no-remote checks.

Key boundary:

```text
DerivedIndexView is not Evidence.
It may become a future query hint, but it cannot prove a code fact.
```

Status: passed oracle review as Level0 safety scaffold. No real LLM integration and no retrieval quality claim.

### R5 — deterministic graph scaffold

Goal: add graph/test/impact plumbing without pretending to have a precise call graph.

Implemented:

- Deterministic graph edge kinds: imports, tests, configures.
- Depth=1 only; depth>1 blocked.
- GraphEdge carries build-time `source_content_sha` and `source_language`.
- Graph materialization uses `StoreHit -> openlocus_store::materialize_evidence()`.
- Invalid ranges rejected, not clamped.
- `graph inspect` labels raw output as `graph_edges_not_evidence`.
- `impact` and `tests` commands return materialized evidence and skipped counts.
- Synthetic graph smoke eval with true citation validation.

Key finding:

- Graph plumbing is useful, but config edges are noisy and import resolution is heuristic. This is safe graph infrastructure, not a semantic graph quality win yet.

Status: passed oracle review as Level0 deterministic graph scaffold.

### R6 — Fast Context Level0 rule prototype

Goal: prototype agent-facing context assembly without an LLM planner.

Implemented:

- 4-turn deterministic loop:
  1. lexical regex/text/BM25;
  2. symbol search if identifier-like query;
  3. graph expansion from top file candidates;
  4. RRF fusion, validation, budget trimming.
- EvidencePack-compatible output with `trace_id`.
- Per-turn/per-channel `ActionRecord` replay trace.
- Trace file written to `.openlocus/traces/fast-context-<trace_id>.json`.
- Token budget using chars/4 approximation plus `--max-evidence` count cap.
- Unknown channel gate.
- Final in-process citation validation: path safety, current file hash, range bounds, excerpt match, and `VerifiedCurrent` freshness.
- Fast-context smoke eval with pack shape, replayable actions, unknown-channel block, token budget, no remote, true citation validation.

Status: passed oracle review as Level0 rule orchestration scaffold. It is not a learned agent and makes no general quality claim.

### R7 — persistent BM25 index and warm benchmark

Goal: separate persistent local index performance from per-query cold build cost while preserving evidence safety.

Implemented:

- `openlocus-index` crate with persistent Tantivy index under `.openlocus/index/tantivy/`.
- Mandatory manifest at `.openlocus/index/manifest.json` with schema version `r7-bm25-v1`, policy hash, file/chunk counts, and per-file content hashes.
- `openlocus index build/status/validate/purge` CLI commands.
- `openlocus search bm25 <query> --index persistent --json` as explicit opt-in; default BM25 remains temporary index behavior.
- `PersistentBm25Index::open(repo_root, policy)` reusable handle used by `openlocus bench warm`.
- Warm benchmark reports `index_open_ms`, `index_build_ms`, p50/p95/max query latency, stale hits skipped, and invalid citations.
- `eval/persistent_index_smoke.py` with 32 Level0 safety checks.

Critical gates added after oracle review:

- persistent search/open refuse if manifest is missing;
- persistent search/open refuse if manifest policy hash or schema version mismatches current policy/schema;
- every Tantivy hit path is validated with `validate_path` before file read;
- empty stored `content_sha` is skipped;
- stored chunk ranges are strictly validated and never clamped;
- `bench warm` opens the Index/searcher once and reuses the same handle for every query;
- benchmark `invalid_citations` checks hash/range/excerpt/freshness, not only line range.

Current small-repo measurement:

```text
Repo scale: small self-referential workspace snapshot
index_open_ms: 1
warm_query_p50_ms: 1
warm_query_p95_ms: 2
warm_query_max_ms: 2
invalid_citations: 0
stale_hits_skipped: 0
persistent_index_smoke: 32/32 checks passed
```

Status: passed Level0 smoke and oracle safety gates. This is a small self-referential benchmark, not a general performance claim.

### R8 — AST-bounded chunking and AST symbol extraction

Goal: test whether Tree-sitter chunk boundaries can improve future span precision without changing EvidenceCore or weakening R7 persistent-index safety.

Implemented:

- `openlocus-ast` crate using Tree-sitter 0.25.x grammars for Rust, Python, JavaScript, and TypeScript.
- `extract_ast_chunks(path, language, source, max_lines)` returns AST-bounded chunks plus fallback line windows; unsupported languages and parse errors fall back to line windows.
- `extract_ast_symbols(path, language, source)` returns narrow header/signature spans; parse errors and unsupported languages return empty AST symbols so callers can use regex fallback.
- Persistent index manifest schema advanced to `r8-bm25-v2`, with `chunk_strategy` and optional `ast_stats`.
- `openlocus index build --chunk-strategy line|ast --json`; line remains default, AST is explicit/experimental.
- `openlocus search symbol <name> --mode regex|ast|auto --json`; regex mode preserves old behavior, auto tries AST then regex fallback.
- `eval/ast_chunking_smoke.py` with 40 Level0 safety checks.

Critical gates:

- AST chunks/symbols are candidates only; final Evidence still comes from current file hash/range/excerpt/freshness validation.
- AST symbol output is narrow header/signature evidence, not full function bodies by default.
- Parser-error files fall back instead of producing AST-bounded evidence from error trees.
- Policy-excluded files are not parsed/indexed/searched.
- Default line-window build/search remains available and R7 persistent smoke still passes.

Status: passed Level0 smoke (`40/40`) and full Rust tests (`176`). AST quality lift is **not proven** yet; this is an experimental scaffold.

### R9 — AST vs line persistent BM25 quality bakeoff

Goal: measure whether AST-bounded chunking improves persistent BM25 retrieval quality compared to default line-window chunking.

Implemented:

- `eval/ast_quality_bakeoff.py`: Reproducible bakeoff script that runs both strategies (purge/build/search/score) on the R2 fixture and produces a combined JSON report with metrics, delta, quality gate, and safety checks.
- Directly reuses `eval/score.py` functions (file_recall_at_k, mrr, span_f_beta_at_k, token_waste_ratio_at_k, citation_validity, etc.) instead of duplicating scoring logic.
- Predictions written as JSONL compatible with `eval/score.py` format.
- Quality gate: citation_validity==1.0 and success_rate==1.0 for both; AST FileRecall@5 ≥ line; AST SpanF0.5@10 ≥ line; token_waste not worse; latency ratio ≤ 1.25.
- Safety checks: build succeeds, validate valid, status strategy matches, emitted evidence nonempty, citation validator invalid_count=0, search stats counters present/nonnegative, strategy explicit.
- Gate and safety are reported separately. Negative results are valid; script does not exit failure on quality gate false.

R2 fixture results (28 tasks, persistent BM25):

| Metric | line | ast | delta |
|---|---:|---:|---:|
| FileRecall@1 | 0.393 | 0.536 | +0.143 |
| FileRecall@5 | 0.821 | 0.750 | −0.071 |
| FileRecall@10 | 0.821 | 0.821 | 0.000 |
| MRR | 0.548 | 0.624 | +0.076 |
| SpanF0.5@10 | 0.039 | 0.064 | +0.025 |
| token_waste_ratio@10 | 0.961 | 0.940 | −0.022 |
| wrong_span_rate@10 | 0.776 | 0.689 | −0.087 |
| citation_validity | 1.0 | 1.0 | 0.0 |
| avg_latency_ms | ~10.4 | ~9.1 | noisy/comparable |
| latency_ratio | — | ~1.0 | — |

Quality gate: **false** (AST FileRecall@5 < line).  
Safety checks: **16/16 passed**.

Key finding:

- AST improves span precision (SpanF0.5@10 ~+63% relative, token_waste −2.2pp, wrong_span_rate −8.7pp) and top-1 recall (+36% relative), but regresses FileRecall@5 (−7.1pp in the latest run) due to chunk-score dilution: more granular AST chunks scatter a file's BM25 signal, reducing the chance that any single chunk ranks the file into top-5.
- This is a real trade-off, not a bug. The fixture is too small and self-referential to generalise.
- **AST remains experimental/opt-in; line remains default.** A larger, diverse codebase eval would be needed for a definitive quality comparison.

Status: safety checks all pass; quality gate is false due to FileRecall@5 regression. This is a valid negative result on a small self-referential fixture.

### R10 — incremental index, dirty summary, and synthetic SLO

Goal: add persistent index dirty summary, file-level incremental update, context-lite dirty integration, and synthetic SLO benchmark without implementing TDB/daemon/watcher or LLM/dense.

Implemented:

 - `dirty_index(repo_root, policy, current_records)`: Manifest-vs-current scan returning `DirtyResult` with clean, requires_update, requires_rebuild, added/modified/deleted files and counts, policy_hash_matches, schema_matches, chunk_strategy. Added detection uses ALL manifest paths (indexed+skipped). Skipped entries with unchanged sha remain clean; skipped→nonempty is reported as modified (not added).
 - `update_index(repo_root, policy, current_records, dirty, path)`: File-level incremental update via `--dirty` (batch) or `--path` (single file). Safety gates: missing manifest, policy/schema/strategy mismatch, manifest load failure → refuse update. Tantivy delete-by-term + re-add, commit once, manifest file write via tmp+rename (not a single transaction with Tantivy commit; crash between may require rebuild or re-update).
 - CLI: `openlocus index dirty --json`, `openlocus index update --dirty --json`, `openlocus index update --path <path> --json`.
 - Context-lite writes dirty-summary.json file with actual dirty index status; `ContextLitePack.dirty_summary` struct field remains `None` (file is the surface).
 - `eval/incremental_index_smoke.py`: 48 safety checks covering build/clean, modify/update/search/clean, add/update/search/clean, delete/update/search/clean, rename simulation, policy-excluded no dirty, policy mismatch refuses update, missing manifest refuses update, skipped empty file clean/promotion, schema/strategy mismatch refuses update, citations invalid_count=0.
 - `eval/synthetic_slo_bench.py`: Deterministic 1000-file synthetic repo, measures build_ms, dirty status latency, persistent_cli_search p95, bench-warm open-once query p95, and one-file update latency (true modification each iteration). 0 invalid citations.

Critical gates:

- Missing manifest → error (requires rebuild); no silent fallback.
- Policy hash/schema/strategy mismatch → refuse update, require rebuild.
- Manifest load failure (corrupt JSON, unknown schema/strategy) → refuse update gracefully, require rebuild.
- Tantivy delete-by-term before add prevents duplicate old+new docs.
- Manifest file write uses tmp+rename to prevent partial writes; but this is not a single transaction with Tantivy commit (crash between may require rebuild or re-update).
- Chunk strategy from manifest is respected; no strategy mixing.
- Tantivy deletes are tombstones until merge (documented, not a bug).
- Added detection uses ALL manifest paths (indexed+skipped); skipped entries with unchanged sha do not appear as added.

Incremental smoke: 48/48 checks passed.

Synthetic SLO (1000 files, seed=42, dev profile):

```text
build_ms: 147
dirty_status_latency p50: 44ms, p95: 48ms
persistent_cli_search_latency p50: 13ms, p95: 14ms
bench_warm open-once query latency p50: 0ms, p95: 0ms, max: 1ms
one_file_update_latency p50: 115ms, p95: 117ms (true modification each iteration)
total_invalid_citations: 0
```

Note: Level0 synthetic-only measurements. Not a general performance claim. `persistent_cli_search_latency_ms` measures CLI search (each call opens index fresh). `bench_warm` reports the Rust CLI's internal open-once query latency over a synthetic dataset.

Key finding:

- Incremental update works correctly and safely: dirty summary accurately identifies changes (skipped entries with unchanged sha remain clean; skipped→nonempty reported as modified, not added), update applies batch changes (Tantivy commit + manifest file write via tmp+rename, not a single transaction), post-update status is clean, and persistent search returns only current content.
- TDB is deferred to R11; R10 focuses on incremental index infrastructure.

Status: passed Level0 smoke (48/48 incremental checks + synthetic SLO).

### R11 — TriviumDB/TDB feature-gated Level0 adapter probe

Goal: implement a real TriviumDB adapter behind a Cargo feature flag, proving the StoreBackend / ChunkStore trait hierarchy can be wired to a live TDB instance. If TDB was not compilable, fall back to a negative feasibility report.

Implemented:

- **Cargo feature**: `tdb = ["dep:triviumdb"]` in `openlocus-store/Cargo.toml`. `triviumdb = { version = "=0.7.0", optional = true }`. Default build does NOT enable the feature.
- **TdbChunkStore** (`crates/openlocus-store/src/tdb_adapter.rs`): Behind `#[cfg(feature = "tdb")]`.
  - Opens `Database<f32>` with `dim=1` and stores chunk metadata as JSON payloads with schema `openlocus_schema=tdb_chunk_v1`.
  - The `[0.0]` vector is a smoke probe only — NOT vector quality. Capabilities honestly report vector=false.
  - Build discipline copies ConservativeChunkStore: `validate_path`, read current bytes once, sha from same bytes, skip stale/traversal/empty.
  - Marker file (`.openlocus_marker`) written alongside `.tdb` for adapter ownership identification.
  - Purge only deletes the adapter-owned TDB artifact set (`.tdb` plus known sidecars) after verifying the marker. Refuses without valid marker.
  - Ingest only from `scan_repo` filtered records; never walks filesystem.
  - TDB hits must go through `StoreHit → materialize_evidence()`.
- **Placeholder preserved**: `TdbPlaceholderStore` unchanged.

R11 Level0 adapter probe results:

| Check | Result |
|---|---|
| `cargo test --workspace` (default, no tdb) | ✅ 193 passed |
| `cargo test -p openlocus-store --features tdb` | ✅ 28 passed |
| `cargo fmt --all -- --check` | ✅ clean |
| `cargo clippy --workspace -- -D warnings` | ✅ clean |
| `cargo clippy -p openlocus-store --features tdb -- -D warnings` | ✅ clean |
| TDB adapter health available when open | ✅ |
| TDB adapter build from records | ✅ |
| TDB adapter capabilities conservative | ✅ metadata+chunks only |
| TDB adapter skips stale records | ✅ |
| TDB adapter skips empty files | ✅ |
| TDB adapter skips traversal records | ✅ |
| TDB adapter purge marker-owned only | ✅ deletes `.tdb` + known sidecars + marker |
| TDB adapter purge refuses without marker | ✅ |
| TDB adapter materialization conformance | ✅ VerifiedCurrent |
| TDB adapter materialization rejects stale | ✅ StaleHit |
| TDB adapter materialization rejects empty sha | ✅ |
| TDB adapter materialization rejects invalid range | ✅ |
| Default build unchanged | ✅ no TDB dependency |
| Placeholder unchanged | ✅ |
| EvidenceCore unchanged | ✅ |

Key finding:

- TriviumDB 0.7.0 compiles from crates.io and works as an optional dependency. The feature-gated adapter correctly wires TDB into the StoreBackend/ChunkStore trait hierarchy with honest capability reporting. Materialization conformance is enforced.
- This is a Level0 adapter probe only. No retrieval quality comparison against Tantivy BM25 or conservative store. The dim=1 vector is a smoke probe. TDB is NOT a default dependency.

Status: passed Level0 smoke (11/11 adapter checks; 29/29 total store tests with --features tdb).

### R12 — real-repo incremental robustness benchmark

Goal: test R10 incremental index on a real repository copy (OpenLocus) with modify/add/delete/rename/policy-exclude/batch workloads, latency comparison, and growth cycling. Do not change Rust core, default CLI/search/retrieve, or introduce watcher/daemon/TDB changes.

Implemented:

- `eval/real_repo_incremental_bench.py` with `report_kind=real_repo_incremental_bench`.
- Source repo defaults to current working directory; copies to temp repo (excluding `target`, `.git`, `.openlocus`, `runs`, `node_modules`, `dist`, `__pycache__`, etc.), creates empty `.git/` and `.openlocus/policy.toml`.
- All workload file mutations occur only in temp repo; original source files are never modified. `--out` writes report to caller workspace.
- **Per-run unique markers** (8-hex-char suffix) avoid self-contamination from copied docs/scripts. Pre-build assert confirms markers absent.
- All search uses `openlocus search bm25 <query> --index persistent --json`. Search returncode must be 0 for positive gates.
- Collected marker-search evidence is validated via `openlocus citations validate` with `invalid_count=0` and validator returncode==0.
- **Positive gates use path+marker conjunction**: `evidence_has_path_and_marker` requires both path fragment AND marker in the cited excerpt. Previous disjunction could pass from unrelated evidence.
- **Empty evidence is not a pass**: Where a marker must be found, `len(evidence) > 0` required.
- Eight workloads: modify_one, add_one, delete_one, rename_one, policy_exclude, branch_like_batch, latency_compare, growth_cycles.
- **Latency compare uses twin repo copies**: Both update and rebuild start from same state with same mutation. Gate is report-only; false does not cause exit failure.
- **Growth catastrophic guard**: `final_after_updates_size ≤ max(3 × post_full_rebuild_size, post_full_rebuild_size + 64MiB)`. Observed 20-cycle growth ratio reported. Does not prove long-term bounded growth.
- `sys.exit(1)` on safety failure only; latency/growth gate failures are report-only.

R12 benchmark results (OpenLocus temp copy):

| Check | Result |
|---|---|
| Baseline build succeeds, file/chunk count >0 | ✅ |
| Baseline dirty clean after build | ✅ |
| Baseline validate valid | ✅ |
| modify_one: dirty detects modified | ✅ |
| modify_one: update succeeds, new marker found at path+marker, old gone | ✅ |
| modify_one: clean + valid after update | ✅ |
| add_one: dirty detects added, search finds marker at path+marker | ✅ |
| add_one: clean + valid after update | ✅ |
| delete_one: dirty detects deleted, no VerifiedCurrent for deleted | ✅ |
| delete_one: clean + valid after update | ✅ |
| rename_one: dirty detects added+deleted, old gone, new found at path+marker | ✅ |
| rename_one: clean + valid after update | ✅ |
| policy_exclude: dirty stays clean, no evidence from excluded path | ✅ |
| branch_batch: add target found at path+marker | ✅ |
| branch_batch: rename-new target found at path+marker | ✅ |
| branch_batch: delete/rename-old markers were indexed before removal | ✅ |
| branch_batch: deleted path no VerifiedCurrent | ✅ |
| branch_batch: rename-old path no VerifiedCurrent | ✅ |
| branch_batch: clean + valid after update | ✅ |
| growth_cycles: all cycles dirty→update→clean→valid | ✅ |
| growth_cycles: catastrophic guard passed (observed ~1.11×) | ✅ |
| total_invalid_citations | 0 |
| stale_verified_current_violations | [] |

Key finding:

- Real-repo incremental update passes this Level0 temp-copy workload: no stale VerifiedCurrent evidence, correct dirty detection for sampled workload types, valid collected marker-search citations, catastrophic growth guard passed.
- Per-run unique markers avoid self-contamination from copied docs/scripts. Pre-build assert confirms markers absent.
- Positive gates require path+marker conjunction; empty evidence is not a pass for positive assertions.
- Latency comparison uses twin repo copies (same mutation, same starting state); incremental update ~42% faster on this sample. Gate is report-only.
- Growth catastrophic guard passed (20 cycles observed growth ~1.11×). Does not prove long-term bounded growth.
- This is one real-repo sample (OpenLocus temp copy). Not a general performance or robustness claim. Different repos, hardware, and workloads may produce different results.
- Collected marker-search evidence validated; not "all evidence in repo".

Status: 149/149 hard safety checks passed; latency/growth gates measured honestly (report-only). Level0 one real-repo sample only.

## Cross-stage findings

### 1. EvidenceCore stayed stable

R0-R11 did not require changing the core evidence contract. R12 also did not change it. Research features were added around it:

- storage uses StoreHit candidates;
- derived indexing uses DerivedIndexView;
- graph uses GraphEdge candidates and materialization;
- fast-context outputs EvidencePack-compatible wrappers.
- persistent BM25 uses Tantivy hits plus mandatory manifest/policy gates, then materializes from the current filesystem before output.
- AST chunking/symbol extraction only changes candidate boundaries; it still materializes final Evidence from current filesystem validation.
- Incremental update uses Tantivy delete-by-term + re-add with the same materialization path; no new evidence bypass.
- TDB adapter uses chunk metadata in JSON payloads with dim=1 smoke probe; materialization still goes through StoreHit.
- Real-repo benchmark uses temp copy of OpenLocus; all evidence still goes through the same materialization and citation validation path.

This validates the original “small and hard” contract design.

### 2. Materialization is the central safety boundary

The largest issues found during review were attempts to produce evidence without going through the materialization gate. R3 fixed this for store hits, R5 fixed it for graph edges, and R6 now performs final validation before output.

The rule should remain permanent:

```text
Candidates are not facts.
Only current-source materialized Evidence can be a fact.
```

### 3. Evaluation caught real mistakes early

The eval/review loop caught:

- over-wide regex spans;
- BM25 parse fallback returning all docs;
- stale BM25 hits;
- graph evidence hand-built without hash binding;
- R4 derived views with insufficient data-level/policy coverage;
- R6 weak final citation validation.

This supports continuing with evidence-gated development rather than feature-first implementation.

### 4. Retrieval quality is still mostly unsolved at span level

BM25 and RRF improved file recall on the current fixture, but SpanF0.5 is still low and token waste is high. More recall channels alone will not solve this. The next quality work should focus on:

- Tree-sitter chunking;
- function/class-level span boundaries;
- query expansion and intent classification;
- better gold datasets;
- persistent warm indexes for fair latency measurement.

### 5. LLM indexing is worth testing, but only through derived views

The R4 scaffold proves a safe route for future LLM-derived indexing:

```text
source span + content_sha + provider/model/prompt/policy provenance
-> DerivedIndexView
-> optional query hint / rerank feature
-> source materialization before final evidence
```

The next LLM step should be a quality bakeoff on low-risk view kinds (`chunk_summary`, `symbol_tags`, `query_aliases`) with provider policy and audit logging, not free-form repository summaries.

### 6. TDB entered as an adapter, not as the data model

The storage scaffold kept TDB in scope while avoiding premature commitment. R11 implemented the first real TDB adapter behind a feature flag, running the same conformance/materialization tests. It did not redefine EvidenceCore or become a default dependency. The adapter is a Level0 probe with honest capability reporting (metadata+chunks only, no lexical/vector/graph quality).

## Current caveats

This prototype is intentionally not production-ready.

- Default BM25 still builds a temporary index per query unless `--index persistent` is explicitly selected.
- Persistent Tantivy is implemented at Level0 with incremental update (R10); updates are file-level via --dirty or --path. No daemon/watch mode yet.
- Incremental update chunk count is approximate after update; full rebuild produces exact counts.
- Tantivy deletes are tombstones until merge; periodic full rebuild recommended for frequently-updated repos.
- Tantivy commit and manifest file write are not a single transaction; crash between may leave a safe but inconsistent state requiring rebuild or re-update.
- Dirty summary re-scans all indexed files (O(n)); filesystem watchers or mtime caching needed for very large repos.
- Skipped entries (empty files, read errors, path_unsafe) are tracked in manifest; they do not appear as "added" on subsequent dirty scans if sha is unchanged.
- ConservativeChunkStore is in-memory and ephemeral.
- TDB is a feature-gated Level0 adapter probe; not a default dependency. Real TriviumDB code is linked only when `--features tdb` is enabled.
- LLM indexing is a deterministic safety scaffold only; no real model/provider is used.
- AST chunking/symbol extraction is experimental; R9 bakeoff shows span precision improvement but FileRecall@5 regression on the small fixture. AST remains opt-in.
- Graph parsing is heuristic and line-based; no LSP/SCIP graph yet.
- Config graph edges are noisy.
- Fast Context is fixed-rule orchestration, not adaptive planning.
- Token budget uses chars/4 approximation, not a tokenizer.
- Policy globbing is simple and needs a mature matcher before broad use.
- Warm-index SLO now has Level0 synthetic measurement and real-repo measurement; larger and more diverse repo behavior is still unknown.
- R12 real-repo benchmark uses one repo (OpenLocus temp copy) with per-run unique alphanumeric markers; not a general performance claim. Growth catastrophic guard passed (observed 20-cycle ~1.11×); does not prove long-term bounded growth.

## Recommended next research stages

### R11 — real TDB adapter behind feature flag ✅ DONE

Priority: medium-high. **Completed in R11.**

Implemented TDB adapter behind `tdb` feature flag. TdbChunkStore with dim=1 smoke probe, metadata+chunks only, marker-based purge, materialization conformance. 11/11 adapter checks passed. No default dependency. No retrieval quality claim.

Gate:

- conformance tests pass ✅;
- corruption/purge/rebuild behavior understood ✅ (marker-based purge, refuses without marker);
- quality/latency/resource comparison against conservative track — deferred to future R13+ bakeoff.

### R12 — real-repo incremental robustness benchmark ✅ DONE

Priority: high. **Completed in R12.**

R10 proved a Level0 synthetic incremental loop. R12 tested one real repository sample with modify/add/delete/rename/policy-exclude/batch workloads:

- `eval/real_repo_incremental_bench.py` on temp copy of OpenLocus;
- all sampled workloads pass hard safety gates (no stale VerifiedCurrent, correct dirty detection, valid collected marker-search citations);
- per-run unique markers avoid self-contamination;
- positive gates require path+marker conjunction (not disjunction);
- latency compare uses twin repo copies (same mutation, report-only gate);
- growth catastrophic guard (observed 20-cycle ~1.11×; does not prove long-term bounded growth);
- sys.exit(1) on safety failure only; latency/growth gates report-only;
- total_invalid_citations=0;
- Level0 one real-repo sample only; not general performance claim.

Gate:

- no stale VerifiedCurrent evidence ✅;
- update path remains faster than full rebuild for small edits — measured honestly via twin repos (report-only gate);
- dirty status remains honest under adds/deletes/renames/excludes ✅;
- collected marker-search evidence validated; not "all evidence in repo".

### R13 — remote embedding and LLM-derived indexing bakeoffs

Priority: medium-high.

Add provider policy, audit logging, secret gate, and cache keys before any outbound call. Test:

- dense semantic retrieval;
- LLM `chunk_summary` / `query_aliases` as retrieval hints;
- no-snippet/signature-only modes.

Gate:

- quality gain measured in eval;
- no policy regression;
- graceful degradation when provider unavailable.

### R14 — graph precision upgrade

Priority: medium.

Add Tree-sitter/LSP/SCIP-like graph adapters behind the same graph model. Keep heuristic graph as baseline.

Gate:

- impact/test-selection fixture improvement;
- depth>1 remains opt-in;
- graph results still materialize through StoreHit.

### R15 — Fast Context quality bakeoff

Priority: medium.

Compare `openlocus fast-context` against `retrieve` over larger task sets. Add ablations: no graph, no symbol, BM25 only, derived hints, dense hints.

Gate:

- no citation regressions;
- budget violations = 0;
- FileRecall/MRR/SpanF0.5 improve or stay within allowed regression;
- trace replay coverage = 100%.

## Final conclusion

The current implementation successfully converts the research design into a working, reviewable prototype. The project now has:

- a stable evidence contract;
- local read/search/retrieve primitives;
- file-backed citation validation;
- retrieval method bakeoff harness;
- storage, derived-index, graph, persistent-index, and fast-context safety scaffolds;
- AST vs line quality bakeoff with measurable span precision improvement;
- incremental index with dirty summary and file-level update;
- feature-gated TriviumDB Level0 adapter probe;
- real-repo incremental robustness benchmark (modify/add/delete/rename/policy-exclude/batch/latency/growth);
- pushed checkpoints for each stage.

The next phase should not rush into a full LLM/dense/TDB system. The safest path is to continue testing incremental robustness on more real repositories (R12 completed one OpenLocus temp-copy sample), then extend TDB to meaningful search quality (R11 adapter probe complete), plug in dense vectors and LLM-derived views into the same evidence-gated harness, and run bakeoffs against the conservative baseline.
