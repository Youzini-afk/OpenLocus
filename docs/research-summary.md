# OpenLocus Research Summary

This document will be updated after each evidence-gated stage.

## Stage status

| Stage | Status | Summary |
|---|---|---|
| R0 Research Harness | Passed initial gate | EvidenceCore/EvidenceMeta, trace JSONL, citation validation, and smoke eval harness are implemented. |
| R1 Local Evidence Kernel | Passed initial gate | Local read, repo scan, line-based regex/text search, policy basics, path safety, and context-lite file output are implemented without remote dependencies. |
| R2 Retrieval Method Bakeoff | Passed oracle review | BM25 (Tantivy), simple symbol search, and RRF fusion added. BM25 uses line-scoring, stale-hash skip, no-overlap skip. Symbol uses boundary delimiters. RRF merges wider metadata into narrower survivors. Eval harness with file/line/span metrics + true citation validity. |
| R3 Level0 Storage Scaffold | Passed Level0 conformance | Store traits + StoreHit materialization gate + ConservativeChunkStore + TDB Level0 placeholder. Materialization rejects empty sha / stale / invalid hits, produces citation-valid Evidence from single file read (TOCTOU-safe). Conservative skips stale/empty/traversal records. TDB placeholder returns available=false without panicking. |

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
- **All ingest from scan_repo records**: stores never walk the filesystem, so policy filtering is automatically respected.
- **This is a Level0 storage scaffold**, not a full storage bakeoff or TDB comparison. No real storage-backed retrieval quality comparison is claimed. No real triviumdb adapter exists. No persistence across CLI invocations.

## Verification snapshot

```text
Rust tests: 70 passed (9 core + 16 repo + 27 retrieval + 18 store)
fmt: clean
clippy: clean with -D warnings
CLI commands: read, scan, search regex/text/bm25/symbol, retrieve, citations validate, context-lite, store status/build/purge, version
Eval: regex/bm25/symbol/rrf on fixtures/r2.jsonl; storage_level0_smoke for conservative+tdb
Structural validity: 1.0 across all methods
Citation validity: 1.0 across all methods (true file I/O verification)
Remote dependency: none
TDB dependency: none (placeholder only)
```
