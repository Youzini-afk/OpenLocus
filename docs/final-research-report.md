# OpenLocus R0-R9 Research Report

Date: 2026-06-11  
Repository: `https://github.com/Youzini-afk/OpenLocus.git`  
Scope: continuous evidence-gated research implementation from the initial design into a working local retrieval kernel prototype, now including the R9 AST quality bakeoff milestone.

## Executive summary

OpenLocus now has a working Rust prototype that validates the core design direction: **all agent-facing code facts must be evidence-backed, citation-checkable, and freshness-aware**.

The implementation completed eight evidence-gated milestones:

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

Final verification snapshot:

```text
Rust tests: 176 passed
fmt: clean
clippy: clean with -D warnings
Remote dependency: none
LLM dependency: none
TDB dependency: none (placeholder only)
Safety evals: storage, derived, graph, fast-context, persistent-index, AST-chunking all passing their Level0 gates; AST quality bakeoff safety checks 16/16 passed
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
openlocus index validate --json
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
| FileRecall@5 | 0.821 | 0.786 | −0.036 |
| FileRecall@10 | 0.821 | 0.821 | 0.000 |
| MRR | 0.556 | 0.631 | +0.075 |
| SpanF0.5@10 | 0.040 | 0.065 | +0.025 |
| token_waste_ratio@10 | 0.960 | 0.938 | −0.022 |
| wrong_span_rate@10 | 0.780 | 0.693 | −0.086 |
| citation_validity | 1.0 | 1.0 | 0.0 |
| avg_latency_ms | ~10.9 | ~10.3 | noisy/comparable |
| latency_ratio | — | ~1.0 | — |

Quality gate: **false** (AST FileRecall@5 < line).  
Safety checks: **16/16 passed**.

Key finding:

- AST improves span precision (SpanF0.5@10 +62% relative, token_waste −2.2pp, wrong_span_rate −8.6pp) and top-1 recall (+36% relative), but regresses FileRecall@5 (−3.6pp) due to chunk-score dilution: more granular AST chunks scatter a file's BM25 signal, reducing the chance that any single chunk ranks the file into top-5.
- This is a real trade-off, not a bug. The fixture is too small and self-referential to generalise.
- **AST remains experimental/opt-in; line remains default.** A larger, diverse codebase eval would be needed for a definitive quality comparison.

Status: safety checks all pass; quality gate is false due to FileRecall@5 regression. This is a valid negative result on a small self-referential fixture.

## Cross-stage findings

### 1. EvidenceCore stayed stable

R0-R9 did not require changing the core evidence contract. Research features were added around it:

- storage uses StoreHit candidates;
- derived indexing uses DerivedIndexView;
- graph uses GraphEdge candidates and materialization;
- fast-context outputs EvidencePack-compatible wrappers.
- persistent BM25 uses Tantivy hits plus mandatory manifest/policy gates, then materializes from the current filesystem before output.
- AST chunking/symbol extraction only changes candidate boundaries; it still materializes final Evidence from current filesystem validation.

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

### 6. TDB should enter as an adapter, not as the data model

The storage scaffold keeps TDB in scope while avoiding premature commitment. The first real TDB experiment should implement Store traits behind a feature flag and run the same conformance/materialization tests against it. It should not redefine EvidenceCore or become a default dependency before bakeoff proof.

## Current caveats

This prototype is intentionally not production-ready.

- Default BM25 still builds a temporary index per query unless `--index persistent` is explicitly selected.
- Persistent Tantivy is implemented at Level0, but updates are full rebuild only; there is no incremental/watch mode yet.
- ConservativeChunkStore is in-memory and ephemeral.
- TDB is a placeholder only; no real TriviumDB code is linked.
- LLM indexing is a deterministic safety scaffold only; no real model/provider is used.
- AST chunking/symbol extraction is experimental; R9 bakeoff shows span precision improvement but FileRecall@5 regression on the small fixture. AST remains opt-in.
- Graph parsing is heuristic and line-based; no LSP/SCIP graph yet.
- Config graph edges are noisy.
- Fast Context is fixed-rule orchestration, not adaptive planning.
- Token budget uses chars/4 approximation, not a tokenizer.
- Policy globbing is simple and needs a mature matcher before broad use.
- Warm-index SLO has only been measured on a small self-referential repo; larger-repo results are unknown.

## Recommended next research stages

### R9 — AST quality bakeoff and persistent index incrementality

Priority: high.

**R9 partial completion**: AST vs line quality bakeoff completed (see R9 stage results above). AST improves SpanF0.5@10 and FileRecall@1 but regresses FileRecall@5 on the small fixture. AST remains experimental/opt-in; line remains default. Remaining R9 work: extend persistent indexes from full rebuild to incremental updates and run warm/cold SLO measurements on larger repositories.

Gate (incrementality part, still pending):

- dirty overlay/update p95 near P0 target;
- no stale verified evidence after edit/delete/rename;
- branch switch does not mix stale manifests;
- warm persistent search remains inside target on medium repos.

### R10 — real TDB adapter behind feature flag

Priority: medium-high.

Implement actual TDB adapter for vector/graph/chunk experiments, but keep SQLite/Tantivy/conservative track as baseline.

Gate:

- conformance tests pass;
- corruption/purge/rebuild behavior understood;
- quality/latency/resource comparison against conservative track.

### R11 — remote embedding and LLM-derived indexing bakeoffs

Priority: medium-high.

Add provider policy, audit logging, secret gate, and cache keys before any outbound call. Test:

- dense semantic retrieval;
- LLM `chunk_summary` / `query_aliases` as retrieval hints;
- no-snippet/signature-only modes.

Gate:

- quality gain measured in eval;
- no policy regression;
- graceful degradation when provider unavailable.

### R12 — graph precision upgrade

Priority: medium.

Add Tree-sitter/LSP/SCIP-like graph adapters behind the same graph model. Keep heuristic graph as baseline.

Gate:

- impact/test-selection fixture improvement;
- depth>1 remains opt-in;
- graph results still materialize through StoreHit.

### R13 — Fast Context quality bakeoff

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
- pushed checkpoints for each stage.

The next phase should not rush into a full LLM/dense/TDB system. The safest path is to first make the local baseline incrementally maintained (R9 remaining work), then plug TDB, dense vectors, and LLM-derived views into the same evidence-gated harness.
