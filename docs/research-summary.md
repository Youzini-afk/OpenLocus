# OpenLocus Research Summary

This document will be updated after each evidence-gated stage.

## Stage status

| Stage | Status | Summary |
|---|---|---|
| R0 Research Harness | Passed initial gate | EvidenceCore/EvidenceMeta, trace JSONL, citation validation, and smoke eval harness are implemented. |
| R1 Local Evidence Kernel | Passed initial gate | Local read, repo scan, line-based regex/text search, policy basics, path safety, and context-lite file output are implemented without remote dependencies. |
| R2 Retrieval Method Bakeoff | Passed oracle review | BM25 (Tantivy), simple symbol search, and RRF fusion added. BM25 uses line-scoring, stale-hash skip, no-overlap skip. Symbol uses boundary delimiters. RRF merges wider metadata into narrower survivors. Eval harness reports file/line/span metrics and citation validity; Rust CLI validator provides hash/excerpt-backed citation validation. |
| R3 Level0 Storage Scaffold | Passed Level0 conformance | Store traits + StoreHit materialization gate + ConservativeChunkStore + TDB Level0 placeholder. Materialization rejects empty sha / stale / invalid hits, produces citation-valid Evidence from single file read (TOCTOU-safe). |
| R4 Level0 Derived Safety | Passed oracle review | DerivedIndexView model + deterministic rule generator + policy/citation/freshness gates + JSONL store. data_level hard-gated ≤1. Secret-like tokens filtered. High-risk kinds disabled. View IDs include policy_mode/generator_version. Stale mutation detected. JSONL parse errors surfaced. No quality claim. |
| R5 Level0 Graph Scaffold | Passed oracle review | GraphEdge carries source_content_sha/source_language. Materialization via StoreHit → openlocus_store::materialize_evidence (not hand-built). Invalid ranges rejected (not clamped). build_graph validates paths/sha, builds safe_records only. Depth=1 only. Channel::Graph. Citation-valid evidence. Not a precise semantic/call graph. |
| R6 Level0 Fast Context | Passed oracle review | 4-turn deterministic loop (lexical → symbol → graph → RRF fusion). EvidencePack-compatible output with trace_id. ActionRecord per-channel replay. Token budget (chars/4). Unknown channel gate. Final citation validation drops invalid. Orchestration scaffold only, not learned agent. |
| R7 Persistent BM25 Index + Warm SLO | Passed Level0 smoke (after oracle review gates) | Persistent Tantivy index at .openlocus/index/tantivy/ with mandatory manifest at .openlocus/index/manifest.json. schema_version=r7-bm25-v1. Search/open refuse if manifest is missing or policy_hash/schema mismatches. validate_path on every hit before file read. Empty content_sha → skip. Strict range (1≤start≤end≤total_lines, no clamp). build_index filters unsafe paths. PersistentBm25Index keeps the Index/searcher open and is reused by bench warm. Warm open=1ms, query p50=1ms. Bench invalid_citations uses real citation validation (hash/range/excerpt/freshness). 32/32 safety checks passed. Level0 implementation notes only; not a general performance claim. |
| R8 AST Chunking + Symbol Extraction | Passed Level0 smoke (40/40 checks) | Tree-sitter AST-bounded chunking and symbol extraction as experimental opt-in (--chunk-strategy ast). AST symbol Evidence uses Channel::TreeSitter, narrow header spans, current-file verification. Fallback to line windows for unsupported languages/parse errors. Manifest schema r8-bm25-v2 with chunk_strategy and ast_stats. R7 manifests loadable. Line remains default. |
| R9 AST vs Line Quality Bakeoff | Safety checks passed (16/16); quality gate false (FileRecall@5 regression) | eval/ast_quality_bakeoff.py compares persistent BM25 line vs ast on R2 fixture. AST improves SpanF0.5@10 (+0.025), FileRecall@1 (+0.143), token_waste (−0.022), wrong_span_rate (−0.086). AST regresses FileRecall@5 (−0.036). Citation_validity and structural_validity 1.0 for both. Latency is comparable/noisy in this tiny CLI benchmark. AST remains experimental/opt-in; line remains default. Negative result on gate is valid; fixture is small and self-referential. |

## R0/R1 initial findings

- Evidence precision matters immediately: the first regex implementation returned over-wide line ranges for distant matches in one file. This would have harmed token waste and Span F0.5. The fix moved R1 regex/text search to one narrow Evidence per matching line.
- Citation validation must validate more than hashes. Range validity and excerpt consistency are needed to catch incorrect spans.
- Path safety is part of evidence safety. Symlink escape protection is required before treating read output as verified current evidence.
- The current local baseline is intentionally boring: no dense, graph, TDB, or LLM indexing has been added yet. This keeps R0/R1 suitable as the control group for later bakeoffs.

## R2 findings

- **BM25 substantially improves file-level recall on the current self-referential fixture**: 0.39 vs 0.21 at k=1, 0.86 vs 0.36 at k=5.
- **Symbol search is high-precision but narrow**: only activates for definition-style queries, but when it fires, line precision is the highest of all methods (0.39) and wrong_span_rate is 0.0.
- **RRF fusion approaches BM25-level recall** while incorporating symbol precision, achieving 0.82 FileRecall@5 and 0.057 SpanF0.5@10 on the current fixture.
- **All methods produce structurally citation-valid evidence in the Python scorer**; aggregated current R2 output was also validated by the Rust CLI citation validator with `0` invalid citations (hash/range/excerpt checked).
- **Token waste is high** (~0.92) because evidence spans are often near-but-not-on narrow gold spans.
- **CLI end-to-end latency** (not warm-index): regex ~13ms, BM25 ~113ms, symbol ~161ms, RRF ~272ms.

## R3 findings

- **Materialization gate is essential and works**: empty sha rejected, stale hits rejected, invalid ranges rejected, TOCTOU-safe (sha + excerpt from same bytes), produced Evidence is citation-valid.
- **TOCTOU safety matters**: reading file bytes once and deriving both sha and excerpt from that single read prevents a modification between reads from producing inconsistent evidence.
- **ConservativeChunkStore validates paths and skips bad records**: traversal paths rejected, stale content_sha skipped, empty files produce no invalid chunks.
- **TDB placeholder provides clean Level0 surface**: returns available=false, success=false with descriptive errors, never panics.
- **This is a Level0 storage scaffold**, not a full storage bakeoff or TDB comparison.

## R4 findings

- **Safety scaffold works**: all gates are functional and block by default. High-risk kinds blocked, data_level hard-gated at ≤1 for Level0, experimental opt-in required, no remote calls.
- **Deterministic view IDs include policy_mode and generator_version**: same source/kind/generator/data_level/policy_mode/generator_version always produces the same ID; change in any produces different ID.
- **No raw full code at data_level ≤ 1**: derived text contains only metadata (line range, language, first identifier). Prevents accidental exposure in derived artifacts.
- **Secret-like tokens are aggressively filtered**: identifiers containing SECRET/TOKEN/PASSWORD/API_KEY/PRIVATE_KEY, or with prefixes sk_/ghp_/AKIA, or long high-entropy mixed strings are not emitted in tags or aliases.
- **JSONL parse errors are surfaced** (not silently skipped): `derived validate` reports parse_errors count.
- **Stale mutation is detected**: building views, modifying a source file, then validating correctly reports stale views.
- **DerivedIndexView is NOT Evidence**: cannot bypass StoreHit/materialize_evidence gate. Any future derived search must materialize source evidence.
- **This is a Level0 safety scaffold only. No quality claim about derived view relevance or usefulness.**

## R5 findings

- **StoreHit materialization gate is essential**: graph edges are converted to StoreHit and delegated to `openlocus_store::materialize_evidence()`. This ensures consistency with all other materialization paths and prevents hand-built Evidence from bypassing validation.
- **GraphEdge carries build-time sha and language**: source_content_sha and source_language allow the materializer to detect stale edges and reject invalid ranges (not clamp them).
- **build_graph validates paths and current sha**: safe_records with validated path and current sha are used for all edge builders (imports, tests, configures). Stale and path-unsafe records are counted and skipped, producing no edges.
- **Simple line-based import parsing works for Rust/Python repos**: mod/use, import/from lines are easy to parse and resolve against the path_set.
- **Config edges are noisy but bounded**: Cargo.toml/package.json link broadly to nearby source files in the fixture. This favors recall but can create many false positives; no general precision/recall claim is made.
- **Depth=1 only; depth>1 returns clear error**: not silently expanded.
- **graph inspect wraps output with artifact marker**: `artifact="graph_edges_not_evidence"` makes it clear these are not citation-valid Evidence.
- **This is a Level0 deterministic scaffold only. Not a precise call graph, type graph, or dependency graph.**

## R6 findings

- **4-turn deterministic loop works as orchestration scaffold**: lexical → symbol → graph → RRF fusion produces multi-channel evidence without any LLM planner.
- **Symbol turn is conditionally activated**: skipped when query lacks identifier-like tokens, avoiding wasted computation.
- **Token budget enforced**: `--budget N` uses chars/4 approximation; evidence trimmed from bottom if cumulative tokens exceed budget. `--max-evidence` is separate count cap.
- **Unknown channel gate**: channels outside regex/text/bm25/symbol/graph are rejected with clear error.
- **Final citation validation**: evidence filtered through `is_citation_valid` before output; invalid dropped and counted in `diagnostics.invalid_citations_dropped`.
- **EvidencePack-compatible output**: `pack` field with trace_id, budget_used. `evidence` field preserved for direct access.
- **ActionRecord per-channel replay**: each turn/channel recorded with query, result_count, latency_ms, optional error. Written to `.openlocus/traces/fast-context-<trace_id>.json`.
- **Confidence derived from top RRF score**: low confidence (<0.1) triggers a missing_question.
- **Orchestration scaffold only, not learned agent.** No adaptive re-querying, no feedback loops, no LLM planning.

## R7 findings

- **Persistent Tantivy BM25 index with manifest works**: build creates .openlocus/index/tantivy/ + .openlocus/index/manifest.json. status/validate/search/purge CLI commands all functional.
- **Manifest and policy gates enforced**: search_persistent_bm25 and PersistentBm25Index::open require the manifest and check manifest policy_hash/schema against current Policy/schema; refuse search if manifest is missing or mismatched. validate_index reports policy_hash_matches=false. Eval confirms: policy change after build → search refuses, validate detects mismatch; manifest deletion → search refuses.
- **Stale/deleted hits are skipped, not emitted**: search_persistent_bm25 re-reads every hit's current file, computes content_sha, and skips mismatches. No stale VerifiedCurrent evidence is ever produced.
- **Empty content_sha bypass prevented**: Hits with empty index_content_sha are skipped (invalid_hits_skipped++), cannot bypass stale check.
- **validate_path on every Tantivy hit**: Before reading a file from a Tantivy hit's path, validate_path is called. Invalid paths → skip. build_index also filters unsafe FileRecord paths.
- **Strict range validation, no clamping**: Chunk ranges must satisfy 1 ≤ start ≤ end ≤ total_lines. Invalid ranges → skip (invalid_hits_skipped++), not clamped.
- **Manifest enables fast staleness detection**: status_index quickly checks all indexed files' current sha against manifest entries. validate_index reports specific stale/deleted/path_unsafe files.
- **Policy exclusion works end-to-end**: .env and *.pem files are excluded by scan_repo, never indexed, and never appear in persistent search output.
- **Warm benchmark is honest**: PersistentBm25Index::open opens the Index/searcher once; same handle reused for all queries with no per-query Index::open. index_open_ms measures open cost only (1ms). index_build_ms reported separately if build was needed. invalid_citations uses real citation validation (hash/range/excerpt/freshness check), not just range.
- **Warm query latency**: On the current small self-referential workspace snapshot, warm queries take 1-2ms per query after index is opened. Open cost is 1ms.
- **Safety is preserved**: Every persistent search hit is re-verified against the current filesystem. The Tantivy stored body is never used as the final excerpt.
- **Purge is safe**: Only deletes known R7 artifact paths under .openlocus/index/. Canonicalizes paths and refuses to delete if index_dir escapes repo root.
- **This is a Level0 implementation only. No incremental update; build is always full rebuild. Warm SLO numbers are from a small self-referential codebase; not a general performance claim. R7 Level0 passed only after oracle review gates.**

## R8 findings

- **Tree-sitter AST-bounded chunking is functional as an experimental scaffold**: `openlocus-ast` crate parses Rust, Python, JavaScript, and TypeScript using Tree-sitter 0.25.x. AST chunk boundaries align with logical code structures (functions, classes, structs, etc.) rather than arbitrary line windows. Oversized nodes are split into line windows; gaps are covered by fallback line windows; no overlapping chunks.
- **AST symbol extraction produces narrow, citation-valid Evidence**: `extract_ast_symbols` extracts definition nodes with header/signature spans (max 10 lines, usually signature/header only rather than full bodies). Symbol names are extracted from Tree-sitter node fields. AST symbol Evidence uses Channel::TreeSitter and is verified against the current filesystem (hash/excerpt/freshness).
- **Fallback is correct**: Unsupported languages (e.g., Go) fall back to line-window chunking. Parse errors also fall back to line windows. No data loss. Fallback stats are visible in manifest ast_stats.
- **Opt-in, not default**: `--chunk-strategy ast` is experimental; line-window remains the default. No quality claim about AST chunking superiority until eval computes it.
- **Manifest schema r8-bm25-v2**: Includes chunk_strategy and ast_stats fields. R7 manifests (r7-bm25-v1) still loadable with default chunk_strategy=line_window_v1. Unrecognized schema versions refuse with rebuild instruction.
- **Schema/strategy mismatch refusal**: search/validate/status refuse if manifest chunk_strategy is unrecognized. R7 manifests without chunk_strategy are loaded as line_window_v1 for compatibility; R8-written manifests always include chunk_strategy. No silent search of unverifiable strategy.
- **CLI symbol search modes**: `openlocus search symbol <name> --mode regex|ast|auto`. Default auto: AST first for supported files, regex fallback for unsupported/no results. Regex mode preserves existing behavior.
- **R7 persistent smoke still passes**: Default line build continues to work with all 32 safety checks passing.
- **AST smoke eval passes 40/40 checks**: Including AST build/status/validate/search, parser-error visibility, stale mutation, narrow AST symbol header, symbol search modes, citation validation, schema mismatch, policy exclusion, default line build compatibility.
- **This is a Level0 experimental scaffold. AST chunking quality lift is NOT proven. Tree-sitter parser edge cases may exist. AST symbol extraction does not handle all symbol patterns (re-exports, aliased imports). No incremental update for AST index.**

## R9 findings

- **AST vs line persistent BM25 bakeoff completed on R2 fixture (28 tasks)**: `eval/ast_quality_bakeoff.py` runs both strategies through purge/build/search/score and produces a combined report with delta, quality gate, and safety checks.
- **AST improves SpanF0.5@10 (+0.025, 62% relative)** and FileRecall@1 (+0.143, 36% relative): AST-bounded chunks align better with logical code structures, producing more targeted evidence spans and better top-1 file retrieval.
- **AST regresses FileRecall@5 (−0.036)**: More granular AST chunks can dilute BM25 scores across multiple chunks per file, reducing the chance that any single chunk ranks a file into top-5. This is the quality gate failure.
- **AST reduces token waste (−0.022) and wrong_span_rate (−0.086)**: Narrower evidence spans waste fewer tokens and overlap gold spans more often.
- **Quality gate is false** (FileRecall@5 regression). **Safety checks all pass** (16/16). Citation_validity and structural_validity are 1.0 for both strategies.
- **Latency is comparable** (ratio ~1.0). Both strategies have similar per-query latency on this fixture.
- **AST remains experimental/opt-in; line remains default.** The fixture is too small and self-referential to generalise. A larger, diverse codebase eval would be needed for a definitive quality comparison.
- **Negative result is valid**: the bakeoff correctly captures a real trade-off between span precision (AST better) and broad file recall at k>1 (line more conservative).

## Verification snapshot

```text
Rust tests: 176 passed (17 ast + 9 core + 16 repo + 27 retrieval + 18 store + 27 derived + 18 graph + 11 context + 33 index)
fmt: clean
clippy: clean with -D warnings
CLI commands: read, scan, search regex/text/bm25/symbol, retrieve, fast-context, citations validate, context-lite, store status/build/purge, derived build/validate/inspect/purge, graph build/inspect, impact, tests, index build/status/validate/purge, search bm25 --index persistent (policy gate enforced), search symbol --mode regex|ast|auto, index build --chunk-strategy line|ast, bench warm (honest: open-once + real citation validation), version
Eval: regex/bm25/symbol/rrf on fixtures/r2.jsonl; storage_level0_smoke; derived_level0_safety (13/13 checks passed); graph_level0_smoke (11/11 checks passed); fast_context_level0_smoke (14/14 checks passed); persistent_index_smoke (32/32 checks passed, incl. policy/manifest gates + strict validation + honest bench); ast_chunking_smoke (40/40 checks passed, incl. AST build/status/validate/search, parser-error visibility, stale mutation, narrow AST symbol header, symbol modes, citation validation, schema mismatch, policy exclusion, default line compatibility); ast_quality_bakeoff (16/16 safety checks passed, quality_gate_passed=false due to FileRecall@5 regression)
Structural validity: 1.0 across all methods
Citation validity: Python scorer reports 1.0 across methods (`path_range_only` unless Python blake3 is installed); Rust CLI citation validator confirmed current aggregated R2 evidence has `0` invalid citations with hash/range/excerpt checks
Remote dependency: none
TDB dependency: none (placeholder only)
LLM dependency: none (rule extractor only)
Graph: deterministic, local-only, depth=1 only
Fast-context: 4-turn deterministic loop, EvidencePack output, ActionRecord replay, token budget, unknown channel gate, final citation validation, no LLM, remote_calls=0
Persistent index: r8-bm25-v2, mandatory manifest + policy gate enforced, validate_path per hit, empty sha skip, strict range no clamp, chunk_strategy line|ast, ast_stats in manifest, warm open=1ms p50=1ms, 32/32 R7 safety checks + 40/40 R8 AST safety checks
```
