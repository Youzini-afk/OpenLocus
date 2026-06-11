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
| R9 AST vs Line Quality Bakeoff | Safety checks passed (16/16); quality gate false (FileRecall@5 regression) | eval/ast_quality_bakeoff.py compares persistent BM25 line vs ast on R2 fixture. Latest run: AST improves SpanF0.5@10 (+0.025), FileRecall@1 (+0.143), token_waste (−0.022), wrong_span_rate (−0.087), but regresses FileRecall@5 (−0.071). Citation_validity and structural_validity 1.0 for both. Latency is comparable/noisy in this tiny CLI benchmark. AST remains experimental/opt-in; line remains default. Negative result on gate is valid; fixture is small and self-referential. |
| R10 Incremental Index + Dirty Summary + Synthetic SLO | Passed Level0 smoke (37→48 incremental checks + synthetic SLO) | Dirty summary (dirty_index) computes manifest-vs-current scan: clean, requires_update, requires_rebuild, added/modified/deleted files with counts. Added detection uses ALL manifest paths (indexed+skipped); skipped→nonempty is modified not added. File-level update (update_index) via --dirty or --path: delete-by-term + re-add, commit once, manifest file write uses tmp+rename (not single transaction with Tantivy commit). Safety gates: missing manifest, policy/schema/strategy mismatch → refuse update (load failures also caught). Context-lite writes dirty-summary.json file. eval/incremental_index_smoke.py 48 safety checks. eval/synthetic_slo_bench.py: 1000-file synthetic repo, build_ms, dirty p50, persistent_cli_search p95, one-file update p50 (true modification each iteration), 0 invalid citations. Level0 synthetic only; not a general performance claim. TDB deferred to R11. |
| R11 TDB Level0 Adapter Probe | Passed Level0 smoke (11/11 adapter checks; 29/29 total store tests with --features tdb) | Feature-gated TriviumDB 0.7.0 adapter behind `tdb` Cargo feature. TdbChunkStore opens Database<f32> with dim=1, stores chunk metadata as JSON payloads (schema `tdb_chunk_v1`). Build discipline copies ConservativeChunkStore: validate_path, TOCTOU-safe sha, skip stale/traversal/empty. Capabilities honest: metadata+chunks only, no lexical/vector/graph. Marker-based purge safety. Materialization via StoreHit → materialize_evidence(). Default build unchanged; TDB is NOT a default dependency. Placeholder preserved. Level0 probe only; no retrieval quality claim. |
| R12 Real-Repo Incremental Robustness Bench | Passed hard safety checks (149/149); latency and catastrophic growth guard are report-only | eval/real_repo_incremental_bench.py tests R10 incremental update on temp copy of OpenLocus repo. Per-run unique markers avoid self-contamination. Positive gates require path+marker conjunction in cited excerpt (not disjunction). Branch delete/rename-old markers are proven indexed before removal. Latency compare uses twin repo copies with same mutation. Growth catastrophic guard (max(3×rebuild, rebuild+64MiB)); observed 20-cycle growth ~1.10×; does not prove long-term bounded growth. sys.exit(1) on safety failure only; latency/growth gates report-only. |
| R13 Remote Embedding / LLM-Derived Indexing Safety Scaffold | Passed Level0 safety (45/45 checks) | New crate `openlocus-provider` with EmbeddingProvider trait, MockEmbeddingProvider (deterministic blake3-based vectors, dimensions=32), DisabledEmbeddingProvider. Policy gate: remote denied by default, data_level ≤1 AND ≤metadata.max_data_level, secret scanning blocks SECRET/TOKEN/PASSWORD/API_KEY/sk_/ghp_/AKIA. Dense JSONL store at .openlocus/embeddings/vectors.jsonl stores EmbeddingRecord (vectors present, no raw text). Audit JSONL at .openlocus/audit/embeddings.jsonl (no raw text/vector/query). CLI uses query_sha/query_len (no raw query). Search produces StoreHits → materialize_evidence(Channel::Dense). Short file ranges: end_line=min(total_lines,8). Audit events: query_embed/allow/block/provider_unavailable (not cache_hit). CLI: provider status/audit, dense build/search/purge. 45/45 safety checks. Integration/safety only; not real semantic retrieval. |

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
- **AST improves SpanF0.5@10 (+0.025, latest run ~63% relative)** and FileRecall@1 (+0.143, 36% relative): AST-bounded chunks align better with logical code structures, producing more targeted evidence spans and better top-1 file retrieval.
- **AST regresses FileRecall@5 (−0.071 in the latest run)**: More granular AST chunks can dilute BM25 scores across multiple chunks per file, reducing the chance that any single chunk ranks a file into top-5. This is the quality gate failure.
- **AST reduces token waste (−0.022) and wrong_span_rate (−0.087 in the latest run)**: Narrower evidence spans waste fewer tokens and overlap gold spans more often.
- **Quality gate is false** (FileRecall@5 regression). **Safety checks all pass** (16/16). Citation_validity and structural_validity are 1.0 for both strategies.
- **Latency is comparable** (ratio ~1.0). Both strategies have similar per-query latency on this fixture.
- **AST remains experimental/opt-in; line remains default.** The fixture is too small and self-referential to generalise. A larger, diverse codebase eval would be needed for a definitive quality comparison.
- **Negative result is valid**: the bakeoff correctly captures a real trade-off between span precision (AST better) and broad file recall at k>1 (line more conservative).

## R10 findings

- **Incremental update works correctly**: dirty_index detects added/modified/deleted files; update_index applies batch changes (delete-by-term + re-add + commit + manifest file write via tmp+rename). Post-update status shows clean. 48/48 incremental smoke checks passed.
- **Dirty summary is accurate and safe**: distinguishes requires_update (file changes) from requires_rebuild (policy/schema/strategy mismatch or corrupt manifest). Policy-excluded added files do not dirty. Skipped entries (empty files, read errors) with unchanged sha are clean; skipped→nonempty is reported as modified (not added). Status never says clean if validate would fail.
- **Safety gates enforced on update**: missing manifest, policy hash mismatch, schema mismatch, and unrecognized chunk strategy all refuse update with clear error messages requiring rebuild. Manifest load failures are also caught gracefully.
- **Tantivy delete-by-term prevents duplicate docs**: `Term::from_field_text(path_field, path)` correctly removes all chunks for a path before re-adding. Deletes are tombstones until merge (documented, not a bug).
- **Context-lite dirty summary written to file**: R10 writes actual dirty index status to `.openlocus/context/dirty-summary.json`. The `ContextLitePack.dirty_summary` struct field remains `None` (the file is the surface, not the struct field).
- **Synthetic SLO benchmark (1000 files)**: latest run build_ms=147, dirty_status p50≈44ms/p95≈48ms, persistent_cli_search p95≈15ms, bench_warm open-once query p95=0ms, one-file update p50≈115ms/p95≈117ms (true modification each iteration), 0 invalid citations. Level0 synthetic only; not a general performance claim. `persistent_cli_search` is CLI-measured; `bench_warm` is the Rust CLI's internal open-once query timing over a synthetic dataset.
- **TDB deferred to R11**: R10 focused on incremental index; TDB moves to R11.
- **Not a single transaction**: Tantivy commit and manifest file write are separate; a crash between may leave a safe but inconsistent state requiring rebuild or re-update.

## R11 findings

- **TriviumDB 0.7.0 compiles and works as an optional dependency**: Feature-gated behind `tdb = ["dep:triviumdb"]`. Default build does not compile TDB. `cargo test --workspace` passes without TDB. `cargo test -p openlocus-store --features tdb` passes with 29/29 tests.
- **TdbChunkStore is a Level0 adapter probe**: Opens `Database<f32>` with `dim=1`, stores chunk metadata as JSON payloads (schema `tdb_chunk_v1`). The `[0.0]` vector is a smoke probe, NOT vector quality. Capabilities honestly report metadata+chunks only, no lexical/vector/graph.
- **Build discipline preserved**: validate_path, TOCTOU-safe sha, skip stale/traversal/empty — same as ConservativeChunkStore.
- **Marker-based purge safety**: Adapter writes an `.openlocus_marker` file; purge verifies marker before deletion and refuses without it.
- **Materialization conformance enforced**: TDB chunk records → StoreHit → materialize_evidence(). Stale, empty-sha, and invalid-range hits correctly rejected.
- **No default dependency on TDB**: TDB is NOT a default backend. It does not replace Tantivy persistent BM25 or the conservative store. Placeholder preserved.
- **No retrieval quality claim**: This is a Level0 wiring/persistence probe. No comparison against Tantivy BM25 or conservative store quality.

## R12 findings

- **Real-repo incremental update passes this Level0 sample**: On a temp copy of the OpenLocus repository (mixed Rust, Python, TypeScript, Markdown), incremental update correctly handles sampled modify, add, delete, rename, policy-excluded, and batch workloads. No stale VerifiedCurrent evidence produced.
- **All hard safety checks pass**: 149/149 hard safety checks pass (dirty detection, update success, clean after update, validate valid, collected marker-search citations invalid_count=0, no stale VerifiedCurrent for deleted/old paths).
- **Per-run unique markers avoid self-contamination**: Fixed markers appeared in copied docs/scripts causing false positives. Per-run suffixes (8-hex chars) and pre-build assert prevent this.
- **Positive gates use path+marker conjunction**: `evidence_has_path_and_marker` requires both path fragment AND marker in the cited excerpt of the same evidence item. Previous disjunction (path OR marker) could pass from unrelated evidence.
- **Citation validity maintained for collected marker-search evidence**: total_invalid_citations=0 across all workloads. Collected evidence validated through `openlocus citations validate` with validator returncode==0.
- **Latency comparison uses twin repo copies**: Both update and rebuild start from same state with same mutation applied. Incremental update ~42% faster on this sample. Gate is report-only; does not cause exit failure.
- **Growth is a catastrophic guard, not bounded proof**: 20 cycles observed growth ~1.11×; catastrophic guard passed (max(3×rebuild, rebuild+64MiB)). Does not prove long-term bounded growth.
- **Level0 one real-repo sample only**: OpenLocus temp copy is one data point. Not a general performance or robustness claim.

## Verification snapshot

```text
Rust tests: 243 passed (193 existing + 50 new in openlocus-provider); 29 passed (store with --features tdb)
fmt: clean
clippy: clean with -D warnings (default and --features tdb)
CLI commands: read, scan, search regex/text/bm25/symbol, retrieve, fast-context, citations validate, context-lite, store status/build/purge, derived build/validate/inspect/purge, graph build/inspect, impact, tests, index build/status/dirty/validate/update/purge, search bm25 --index persistent (policy gate enforced), search symbol --mode regex|ast|auto, index build --chunk-strategy line|ast, bench warm (honest: open-once + real citation validation), provider status/audit, dense build/search/purge, version
Eval: regex/bm25/symbol/rrf on fixtures/r2.jsonl; storage_level0_smoke; derived_level0_safety (13/13 checks passed); graph_level0_smoke (11/11 checks passed); fast_context_level0_smoke (14/14 checks passed); persistent_index_smoke (32/32 checks passed, incl. policy/manifest gates + strict validation + honest bench); ast_chunking_smoke (40/40 checks passed); ast_quality_bakeoff (16/16 safety checks passed, quality_gate_passed=false due to FileRecall@5 regression); incremental_index_smoke (48/48 checks passed, incl. dirty summary + skipped empty file + file-level update + policy/schema/strategy gates + citation validation); synthetic_slo_bench (1000 files, build_ms, dirty p50/p95, persistent_cli_search p95, bench_warm open-once query p95, one-file update p50/p95, 0 invalid citations, Level0 synthetic only); real_repo_incremental_bench (modify/add/delete/rename/policy_exclude/batch/latency_compare/growth_cycles on OpenLocus temp copy, total_invalid_citations=0, no stale VerifiedCurrent violations, Level0 one real-repo sample only); provider_dense_safety (45/45 checks passed, incl. remote/outbound defaults, experimental gate, vector/audit no raw text, secret blocking, stale rejection, disabled/unknown provider audit events, query_sha not raw query, short file range, citation validity)
Structural validity: 1.0 across all methods
Citation validity: Python scorer reports 1.0 across methods (`path_range_only` unless Python blake3 is installed); Rust CLI citation validator confirmed current aggregated R2 evidence has `0` invalid citations with hash/range/excerpt checks
Remote dependency: none
TDB dependency: optional only (behind `tdb` feature; not in default build)
LLM dependency: none (rule extractor only)
Graph: deterministic, local-only, depth=1 only
Fast-context: 4-turn deterministic loop, EvidencePack output, ActionRecord replay, token budget, unknown channel gate, final citation validation, no LLM, remote_calls=0
Persistent index: r8-bm25-v2, mandatory manifest + policy gate enforced, validate_path per hit, empty sha skip, strict range no clamp, chunk_strategy line|ast, ast_stats in manifest, warm open=1ms p50=1ms, 32/32 R7 safety checks + 40/40 R8 AST safety checks + 48/48 R10 incremental safety checks
Incremental update: dirty summary (added/modified/deleted), skipped entries tracked (not falsely added), file-level update (--dirty, --path), manifest file write via tmp+rename (not single transaction with Tantivy commit), Tantivy delete-by-term, policy/schema/strategy mismatch + load failure refusal
TDB adapter: Level0 probe, feature-gated, dim=1 smoke, metadata+chunks only, marker-based purge, materialization conformance, no default dependency, no retrieval quality claim
Real-repo bench: Level0 one real-repo sample (OpenLocus temp copy), per-run unique markers avoid self-contamination, cited-excerpt path+marker conjunction gates, branch old/delete markers proven indexed before removal, sampled modify/add/delete/rename/policy_exclude/batch workloads pass, latency_compare uses twin repos (report-only gate), growth_cycles catastrophic guard (observed 20-cycle ~1.10×, does not prove long-term bounded), total_invalid_citations=0, citations_validator_ok=true, no stale VerifiedCurrent violations, sys.exit(1) on safety failure only
Provider/dense scaffold: MockEmbeddingProvider deterministic blake3 vectors dim=32, gate enforces data_level≤1 AND data_level≤metadata.max_data_level, secret scanning blocks SECRET/TOKEN/PASSWORD/API_KEY/sk_/ghp_/AKIA, audit uses query_embed/allow/block/provider_unavailable (not cache_hit), CLI uses query_sha/query_len (no raw query), short file end_line=min(total_lines,8), vector store has vectors but no raw text, audit has no raw text/vector/query, 45/45 safety checks, integration/safety only — not real semantic retrieval
```

## R13 findings

- **Safe scaffold works**: All 45 safety checks pass. Remote is denied by default. Experimental opt-in is required. Secret scanning blocks token-like inputs. Audit contains no raw text or vectors. Vector store contains embedding vectors but no raw text/code snippet.
- **Mock provider is deterministic and normalized**: Same inputs always produce the same unit-length vector via blake3 hash. Different inputs produce different vectors. No network dependency.
- **Materialization gate is essential**: Dense search produces StoreHits which must be materialized through `materialize_evidence()`. Stale hits (content_sha mismatch) are correctly rejected.
- **Metadata-only views prevent code leakage**: Dense store builds views from path/language/basename/path-tokens only. No code snippets at data_level=0. Vector store and audit log do not contain raw code text.
- **Short file ranges are valid**: end_line=min(total_lines, 8) ensures materialize_evidence can verify ranges. Short files produce valid evidence.
- **Query text never leaks**: CLI JSON uses query_sha/query_len. Trace events use query_sha. Audit never stores raw query text. Blocked secret queries do not appear in traces.
- **Audit events use accurate names**: `query_embed` for query embedding, `allow`/`block`/`provider_unavailable` for decisions. Not `cache_hit` (no real cache behavior in R13). Cache key builder/stability only; no cache-hit behavior yet.
- **This is a safety scaffold only. No real semantic quality claim.** Mock vectors are deterministic blake3-based and do not capture semantic similarity. Dense mock search is integration/safety only.
