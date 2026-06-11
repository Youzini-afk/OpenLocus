# OpenLocus Research Summary

This document will be updated after each evidence-gated stage.

## Stage status

| Stage | Status | Summary |
|---|---|---|
| R0 Research Harness | Passed initial gate | EvidenceCore/EvidenceMeta, trace JSONL, citation validation, and smoke eval harness are implemented. |
| R1 Local Evidence Kernel | Passed initial gate | Local read, repo scan, line-based regex/text search, policy basics, path safety, and context-lite file output are implemented without remote dependencies. |
| R2 Retrieval Method Bakeoff | Passed oracle review | BM25 (Tantivy), simple symbol search, and RRF fusion added. BM25 uses line-scoring, stale-hash skip, no-overlap skip. Symbol uses boundary delimiters. RRF merges wider metadata into narrower survivors. Eval harness with file/line/span metrics + true citation validity. |
| R3 Level0 Storage Scaffold | Passed Level0 conformance | Store traits + StoreHit materialization gate + ConservativeChunkStore + TDB Level0 placeholder. Materialization rejects empty sha / stale / invalid hits, produces citation-valid Evidence from single file read (TOCTOU-safe). |
| R4 Level0 Derived Safety | Passed oracle review | DerivedIndexView model + deterministic rule generator + policy/citation/freshness gates + JSONL store. data_level hard-gated ≤1. Secret-like tokens filtered. High-risk kinds disabled. View IDs include policy_mode/generator_version. Stale mutation detected. JSONL parse errors surfaced. No quality claim. |
| R5 Level0 Graph Scaffold | Passed oracle review | GraphEdge carries source_content_sha/source_language. Materialization via StoreHit → openlocus_store::materialize_evidence (not hand-built). Invalid ranges rejected (not clamped). build_graph validates paths/sha, builds safe_records only. Depth=1 only. Channel::Graph. Citation-valid evidence. Not a precise semantic/call graph. |

## R0/R1 initial findings

- Evidence precision matters immediately: the first regex implementation returned over-wide line ranges for distant matches in one file. This would have harmed token waste and Span F0.5. The fix moved R1 regex/text search to one narrow Evidence per matching line.
- Citation validation must validate more than hashes. Range validity and excerpt consistency are needed to catch incorrect spans.
- Path safety is part of evidence safety. Symlink escape protection is required before treating read output as verified current evidence.
- The current local baseline is intentionally boring: no dense, graph, TDB, or LLM indexing has been added yet. This keeps R0/R1 suitable as the control group for later bakeoffs.

## R2 findings

- **BM25 substantially improves file-level recall on the current self-referential fixture**: 0.57 vs 0.21 at k=1, 0.89 vs 0.50 at k=5.
- **Symbol search is high-precision but narrow**: only activates for definition-style queries, but when it fires, line precision is the highest of all methods (0.39) and wrong_span_rate is 0.0.
- **RRF fusion recovers BM25-level recall** while incorporating symbol precision, achieving the best SpanF0.5@10 (0.07).
- **All methods produce citation-valid evidence** (1.0 structural + true citation validity).
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

## Verification snapshot

```text
Rust tests: 115 passed (9 core + 16 repo + 27 retrieval + 18 store + 27 derived + 18 graph)
fmt: clean
clippy: clean with -D warnings
CLI commands: read, scan, search regex/text/bm25/symbol, retrieve, citations validate, context-lite, store status/build/purge, derived build/validate/inspect/purge, graph build/inspect, impact, tests, version
Eval: regex/bm25/symbol/rrf on fixtures/r2.jsonl; storage_level0_smoke; derived_level0_safety (13/13 checks passed); graph_level0_smoke (11/11 checks passed)
Structural validity: 1.0 across all methods
Citation validity: 1.0 across all methods (true file I/O verification)
Remote dependency: none
TDB dependency: none (placeholder only)
LLM dependency: none (rule extractor only)
Graph: deterministic, local-only, depth=1 only
```
