# OpenLocus Research Log

## 2026-06-11 — R0/R1 bootstrap

### Objective

Start an evidence-gated implementation that can validate the core contract before adding dense, graph, TDB, or LLM indexing experiments.

### Current hypothesis

The first useful milestone is not semantic retrieval. It is a trustworthy local evidence kernel with traceability, citation validation, and file-backed context-lite.

### Stage gates

- R0 passes when every returned result is traceable as `EvidenceCore` and citation validation can verify path/range/hash.
- R1 passes when read, repo scan, regex search, and context-lite work without remote dependencies.

### Implementation notes

- Initialized a Rust workspace with separate crates for core contracts, repo IO, retrieval, and CLI.
- Implemented `EvidenceCore` as the stable output contract: `path`, `start_line`, `end_line`, `content_sha`, `score`, `why`, `channels`.
- Kept `EvidenceMeta` optional so policy/freshness/excerpt/language metadata can evolve without breaking core evidence.
- Implemented local-only policy defaults and basic `.openlocus/policy.toml` loading.
- Implemented trace JSONL output under `.openlocus/traces/` for command-level observability.
- Implemented file reads with range parsing and symlink escape protection.
- Implemented repo scanning with default ignores, policy include/exclude, and gitignore control.
- Implemented line-based regex/text search. A review caught an early over-wide span design; it was corrected so each matched line returns a narrow Evidence span.
- Implemented citation validation for single Evidence, Evidence arrays, and `{ evidence: [...] }` packs, including range/hash/excerpt checks.
- Implemented context-lite file creation as an initial file-backed session fact surface.

### Review findings resolved

- **Search span precision**: fixed broad first-match-to-last-match spans; regex/text search now returns per-line evidence.
- **Citation validation**: now validates path, range, current file hash, and excerpt consistency.
- **Path safety**: existing targets are canonicalized to prevent symlink escapes outside the repo.
- **Freshness correctness**: search evidence hashes the bytes actually read for the match, avoiding stale scan-record hashes.
- **Policy basics**: scan include/exclude and `include_gitignored` now affect scan behavior.

### Verification

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
openlocus read README.md:1-3 --json
openlocus citations validate /tmp/openlocus-evidence.json --json
openlocus search regex OpenLocus --json
openlocus citations validate /tmp/openlocus-search.json --json
openlocus context-lite --write-files --json
python3 eval/run_retrieval.py --dataset fixtures/smoke.jsonl --out runs/smoke-rg.jsonl --openlocus target/debug/openlocus
python3 eval/score.py --pred runs/smoke-rg.jsonl
```

All Rust checks passed. CLI smoke eval returned success rate `1.0`.

### Caveats carried forward

- Regex search is intentionally line-based in R1; multiline regex spanning line boundaries is not supported yet.
- Context-lite currently writes minimal placeholder files; deeper dirty/test/diagnostics ingestion belongs in a later hardening pass.
- Policy globbing is intentionally simple and should be replaced with a mature matcher before broad user-facing use.
- Trace events are command-level and not yet full replayable retrieval trajectories.

---

## 2026-06-11 — R2 Retrieval Method Bakeoff

### Objective

Add local-only, evaluable, EvidenceCore-compatible BM25/symbol/RRF retrieval methods. Do not implement dense, TDB, LLM, or graph indexing yet.

### Hypothesis

BM25 over bounded line chunks with line tightening will outperform plain regex for file-level recall while maintaining narrow evidence spans. Symbol search will provide high-precision single-line hits for definition queries. RRF fusion of regex + BM25 + symbol will achieve the best combined file recall and MRR.

### Implementation notes

- **BM25 search** (`bm25_search.rs`): Builds an in-temp Tantivy index over bounded line chunks (max 30 lines). Schema: path, language, content_sha (STRING|STORED), start_line, end_line (u64 STORED), content (TEXT|STORED). Returns Evidence tightened to the best-matching line ±2 context, capped at 7 lines. content_sha recomputed from current file bytes. Channel::Bm25, meta.score_parts.bm25, meta.freshness = verified_current. Query parser safe fallback on parse errors.
- **BM25 span selection**: Each chunk hit is line-scored by query token overlap (not chunk center). If no line has any query token overlap, the hit is skipped (precision-biased). The best-scoring line becomes the center, ±2 context, capped at 7 lines.
- **BM25 stale check**: Index-time content_sha from Tantivy stored fields is compared to current file hash. Mismatch → skip hit. Line range strictly guarded: 1 ≤ start ≤ end ≤ total_lines.
- **Symbol search** (`symbol_search.rs`): Simple heuristic regex-based symbol extraction for Rust/Python/TS/JS/Go. Returns narrow single-line Evidence around the signature/head. Uses Channel::Regex with why="simple_symbol: {name}". Fills meta.symbol with name + kind. Language-aware pattern filtering prevents cross-language false matches. Word-boundary delimiters prevent partial matches (e.g. "User" won't match "UserProfile").
- **RRF fusion** (`rrf.rs`): Combines evidence from multiple channels using Reciprocal Rank Fusion (k=60). Exact same (path, start_line, end_line) → merge why/score/channels. Overlapping spans on same path → keep narrower, discard wider. The wider's why, channels, and RRF score are merged into the narrower survivor (no span widening). Tie-break: score desc, path asc, start_line asc, end_line asc. RRF only changes ranking/score, never widens spans.
- **CLI**: Added `search bm25 <query>`, `search symbol <query>`, `retrieve <query> --channels regex,bm25,symbol --max-results N`. All new commands append trace events.
- **Eval**: Updated `run_retrieval.py` to accept `--method regex|text|bm25|symbol|rrf`, `--channels` for RRF. Updated `score.py` with structural_validity + citation_validity (true file I/O validation), FileRecall@1/5/10, FilePrecision@5/10, MRR, LinePrecision@10, LineRecall@10, SpanF0.5@10, token_waste_ratio@10, wrong_span_rate@10 (gold-file-only), zero_overlap_evidence_rate@10 (all evidence). Added `fixtures/r2.jsonl` with 28 tasks, gold spans refreshed to current codebase.

### R2 eval results (after oracle review fixes)

| Metric | regex | bm25 | symbol | rrf |
|---|---|---|---|---|
| file_recall@1 | 0.21 | **0.46** | 0.39 | **0.46** |
| file_recall@5 | 0.50 | **0.86** | 0.39 | **0.86** |
| file_recall@10 | 0.57 | **0.93** | 0.39 | **0.93** |
| mrr | 0.33 | 0.64 | 0.39 | **0.64** |
| line_precision@10 | 0.07 | 0.06 | **0.39** | 0.08 |
| line_recall@10 | 0.02 | 0.06 | 0.01 | 0.05 |
| span_f0.5@10 | 0.04 | 0.06 | 0.06 | **0.07** |
| structural_validity | **1.0** | **1.0** | **1.0** | **1.0** |
| citation_validity | **1.0** | **1.0** | **1.0** | **1.0** |
| wrong_span_rate@10 | 0.79 | 0.77 | **0.0** | 0.78 |
| zero_overlap@10 | 0.90 | 0.92 | **0.0** | 0.92 |
| avg_latency_ms† | 12 | 131 | 168 | 274 |

† CLI end-to-end latency including index build (BM25/RRF) or file scan (symbol). Not a warm-index benchmark.

### Key findings

1. BM25 substantially improves file-level recall on the current self-referential fixture (0.46 vs 0.21 at k=1, 0.86 vs 0.50 at k=5). This is an early local-only bakeoff result, not a general benchmark claim.
2. Symbol search has the highest line precision (0.39) and zero wrong spans when it fires, but only activates for definition-style queries on known languages.
3. RRF fusion recovers BM25-level file recall (0.86 at k=5) while incorporating symbol precision, achieving the best SpanF0.5@10 (0.07) in this fixture.
4. All methods produce citation-valid evidence (1.0 structural + true citation validity).
5. Token waste ratio remains high (~0.92) — evidence spans are narrow but often miss gold lines. Query-token line scoring fixed chunk-center anchoring, but chunking/query expansion/gold-span calibration need further refinement.
6. BM25 stale check and no-overlap skip are effective: no stale or center-only hits appear in the output.
7. Symbol boundary delimiters prevent partial matches (verified: "User" does not match "UserProfile").

### Oracle review fixes applied

- **BM25 span selection**: Replaced chunk-center anchoring with line-level query token overlap scoring. Hits with no token overlap are skipped (precision-biased).
- **BM25 stale check**: Index-time content_sha compared to current file hash. Mismatch → skip. Strict line range guard added.
- **BM25 query fallback**: Removed `*` all-doc fallback; parse failures now degrade to sanitized query or empty results.
- **Symbol boundary**: Added `(?:[^a-zA-Z0-9_]|$)` delimiter after symbol name in all patterns. Prevents partial matches.
- **RRF overlap dedup**: Wider span's why/channels/score are now merged into the narrower survivor. Deterministic tie-break: score desc, path asc, start_line asc, end_line asc.
- **Eval citation validity**: Split into `structural_validity` (no I/O) and `citation_validity` (verifies path exists and range/hash match current file). Added `--repo-root`.
- **Eval metrics**: Added `zero_overlap_evidence_rate@10` (all evidence, not just gold-file). `wrong_span_rate@10` now only counts evidence on gold files. Added `--channels` to `run_retrieval.py`.
- **Fixtures**: Refreshed gold spans to current codebase actual line numbers.

### Caveats carried forward

- BM25 builds a fresh Tantivy index per query; not suitable for large repos without persistent indexing.
- Symbol search is heuristic-only; real TreeSitter or SCIP would improve recall.
- Line-level metrics are low because gold spans target specific code regions; the eval is more meaningful for file-level metrics at this stage.
- BM25 CLI end-to-end latency includes index build; a persistent index would reduce this significantly.
- The Rust `regex` crate does not support lookahead; symbol boundary uses consuming match `(?:[^a-zA-Z0-9_]|$)` instead.

---

## 2026-06-11 — R3 Level0 Storage Scaffold/Conformance

### Objective

Add Store traits + StoreHit materialization gate + conservative storage surface + TDB Level 0 test surface. Do not implement dense/LLM/graph quality or make TDB a default dependency. This is a Level0 storage scaffold, not a full storage bakeoff or TDB comparison.

### Hypothesis

A storage abstraction layer with materialization gating (StoreHit → filesystem Evidence) will enable safe backend substitution. The conservative in-memory chunk store should pass conformance tests. TDB can be represented as a placeholder that correctly reports unavailability rather than being silently missing.

### Implementation notes

- **Store crate** (`openlocus-store`): New workspace crate with types (SnapshotId, ChunkKind, ChunkKey, ChunkRecord, StoreSource, StoreDebug, StoreHit, StoreCapabilities, StoreHealth, StoreError) and traits (StoreBackend, ChunkStore, LexicalStore, VectorStore, GraphStore).
- **Materialization gate** (`materialize_evidence`): Critical gate that converts StoreHit → Evidence by: (0) reject empty content_sha, (1) validate_path (symlink protection), (2) read file bytes once; compute content_sha from same bytes (TOCTOU-safe), (3) reject stale hits (sha mismatch), (4) decode same bytes for line content; validate range (1 ≤ start ≤ end ≤ total_lines), (5) build excerpt from decoded content, (6) return Evidence with freshness=VerifiedCurrent. Store backends never directly output authoritative Evidence.
- **ConservativeChunkStore**: In-memory ephemeral implementation (mode=ephemeral_in_memory, persistent=false) with metadata=true, chunks=true, lexical=false, vector=false, graph=false. Build validates paths via validate_path, skips stale records (content_sha mismatch), skips empty files (no start=1,end=0 invalid chunks), computes content_sha from same bytes used for line splitting. Does not walk filesystem itself.
- **TdbPlaceholderStore**: Level 0 test surface implementing StoreBackend with available=false, success=false, mode=placeholder, and all capabilities false. Returns clear error messages ("feature 'tdb' is not enabled"). Does not add triviumdb as a dependency.
- **CLI**: Added `openlocus store status/build/purge conservative|tdb --json`. JSON output includes mode, persistent, and success fields. All commands append trace events.
- **Eval**: Added `eval/storage_bakeoff.py` with report_kind="storage_level0_smoke". Uses positional backend syntax.

### R3 Level0 conformance results

| Check | Result |
|---|---|
| Conservative build from scan_repo | ✅ chunks, files, valid ranges |
| Conservative capabilities explicit | ✅ metadata+chunks only |
| Conservative skips stale records | ✅ test passes |
| Conservative skips empty files (no invalid chunks) | ✅ test passes |
| Conservative skips traversal records | ✅ test passes |
| Conservative purge | ✅ clears all data |
| TDB status returns available=false, success=false | ✅ |
| TDB build returns error, not panic | ✅ |
| TDB purge returns error, not panic | ✅ |
| Materialize rejects empty content_sha | ✅ test passes |
| Materialize rejects stale hit | ✅ test passes |
| Materialize rejects invalid range | ✅ test passes |
| Materialize produces citation-valid Evidence | ✅ test passes |
| Materialize TOCTOU-safe (sha + excerpt from same read) | ✅ test passes |
| Ingest only from scan_repo records | ✅ store never walks filesystem |
| Unknown backend returns error | ✅ |

### Key findings

1. The materialization gate is essential and works correctly: empty sha rejected, stale hits rejected, invalid ranges rejected, and produced Evidence is citation-valid (verified_current freshness, correct content_sha, excerpt from current file).
2. TOCTOU safety matters: reading file bytes once and deriving both sha and excerpt from that single read prevents a file modification between sha computation and excerpt extraction from producing inconsistent evidence.
3. ConservativeChunkStore validates paths and skips stale/invalid records: traversal paths are rejected, stale content_sha records are skipped, empty files produce no invalid chunks.
4. TDB placeholder provides a clean Level 0 test surface: CLI commands return JSON with available=false, success=false, and descriptive errors rather than panicking or silently failing.
5. All ingest comes from scan_repo filtered records; stores never walk the filesystem. Policy filtering is automatically respected.

### Caveats carried forward

- ConservativeChunkStore is in-memory ephemeral only; data does not persist across CLI invocations. A persistent index (e.g. sidecar file) would be needed for production use.
- TDB placeholder does not exercise any actual TriviumDB code; it only validates the API shape. When a real adapter is needed, it should be behind a feature flag with an optional dependency.
- VectorStore and GraphStore traits are defined but not implemented; they are placeholders for R4+ experimentation.
- The store does not yet integrate with BM25 search; BM25 still builds a fresh Tantivy index per query.
- This is a Level0 storage scaffold, not a full storage bakeoff or TDB comparison. No real storage-backed retrieval quality comparison is claimed.

---

## 2026-06-11 — R4 LLM Indexing Research Candidate Level0 Safety Scaffold

### Objective

Add DerivedIndexView model + deterministic rule/mock generator + policy/citation/freshness gates + JSONL store + CLI/eval. No real LLM, no quality claim. This is a Level0 safety scaffold: prove the gates work before connecting any remote service.

### Hypothesis

A derived index view system with strict safety gates (no-LLM-required generator, policy-gated kinds, data_level bounds, source validation, experimental opt-in) can be implemented and verified without any remote calls. The safety gates should block all high-risk operations by default.

### Implementation notes

- **Derived crate** (`openlocus-derived`): New workspace crate with model (DerivedIndexView, DerivedViewKind, DerivedSource, DerivedGeneratorKind, DerivedProvenance, DerivedValidation), generator (deterministic rule extractor for L1 kinds), validation (path/content_sha/range/kind/data_level checks), and JSONL store.
- **Key constraint**: DerivedIndexView is NOT Evidence. It cannot bypass StoreHit/materialize_evidence. If derived search is ever implemented, it must materialize source evidence.
- **Generator**: Deterministic rule-based extractor for chunk_summary, symbol_tags, query_aliases. No full raw code snippets at data_level ≤ 1 (only metadata + first identifier per chunk). Identifier extraction via simple heuristics; query alias splitting via camelCase/snake_case decomposition.
- **Validation**: validate_derived_view checks path safety (via validate_path), source content_sha matches current file, range valid, kind allowed, data_level allowed. No remote calls.
- **Store**: JsonlDerivedViewStore writes to `.openlocus/derived/views.jsonl` with audit log at `.openlocus/derived/audit.jsonl`. Supports upsert (replaces by view_id), list, purge.
- **View ID**: Deterministic hash of (source_path, source_sha, kind, generator, data_level, policy_mode, generator_version). Same inputs always produce the same ID. Source, policy, or generator-version changes produce different IDs.
- **High-risk kinds**: candidate_edge and bug_symptom_hint are disabled by default (is_high_risk() = true). Generator skips them entirely.
- **CLI**: `openlocus derived build/validate/inspect/purge`. Build requires `--experimental`. All commands output JSON with remote_calls=0, data_level, policy_mode.
- **Eval**: `eval/derived_safety.py` with report_kind="derived_level0_safety".

### R4 Level0 safety results (after oracle review fixes)

| Safety Check | Result |
|---|---|
| Build without --experimental blocked | ✅ clear error JSON |
| Build with --experimental succeeds | ✅ views generated |
| Build with --max-data-level 2 blocked | ✅ Level0 hard gate |
| remote_calls always 0 | ✅ |
| candidate_edge blocked | ✅ blocked_kind=1, generated=0 |
| bug_symptom_hint blocked | ✅ blocked_kind=1, generated=0 |
| data_level ≤ 1 enforced | ✅ Level0 hard gate |
| No raw full code in derived text at data_level=1 | ✅ test passes |
| Secret-like tokens filtered (SECRET_KEY, API_TOKEN, sk_, ghp_, AKIA) | ✅ test passes |
| View ID deterministic | ✅ test passes |
| View ID changes on source change | ✅ test passes |
| View ID changes on policy_mode change | ✅ test passes |
| View ID changes on generator_version change | ✅ test passes |
| Validate detects stale views | ✅ test passes |
| Validate detects blocked kind | ✅ test passes |
| Validate detects blocked data_level | ✅ test passes |
| Validate detects path_unsafe | ✅ test passes |
| Validate detects invalid range | ✅ test passes |
| Validate surfaces parse_errors | ✅ corrupt JSONL test passes |
| Stale mutation detected (modify file → validate → stale>0) | ✅ eval passes |
| Policy-excluded paths absent (.env, *.pem) | ✅ eval passes |
| Purge removes views and audit files | ✅ eval passes |

### Key findings

1. The safety scaffold works: all gates are functional and block by default. High-risk kinds are blocked, data_level is hard-gated at ≤1 for Level0, experimental opt-in is required, and no remote calls are made.
2. Deterministic view IDs include policy_mode and generator_version: the same source/kind/generator/data_level/policy_mode/generator_version always produces the same ID, and a change in any of these produces a different ID.
3. At data_level ≤ 1, derived text contains only metadata (line range, language, first identifier) — no full raw code snippets. This prevents accidental exposure of sensitive code in derived artifacts.
4. Secret-like tokens are aggressively filtered: identifiers containing SECRET/TOKEN/PASSWORD/API_KEY/PRIVATE_KEY, or with prefixes sk_/ghp_/AKIA, or long high-entropy mixed-case strings are not emitted in tags or aliases.
5. JSONL parse errors are surfaced (not silently skipped): `derived validate` reports parse_errors count; safety eval fails if parse_errors > 0.
6. Stale mutation is detected: building views, modifying a source file, then validating correctly reports stale views.
7. The rule-based generator is intentionally simple: identifier extraction, camelCase/snake_case splitting. It is not a replacement for real LLM-based indexing; it proves the pipeline shape works.

### Caveats carried forward

- This is a Level0 safety scaffold only. No quality claim about derived view relevance or usefulness.
- The rule-based generator produces coarse metadata; real LLM indexing would produce richer summaries and aliases.
- DerivedIndexView is not Evidence and cannot be used as such. Any future derived search must materialize source evidence through the store gate.
- The JSONL store does not scale for large repos; it reads/writes the full file on each operation.
- candidate_edge and bug_symptom_hint are defined but never generated; they serve as API surface for future high-risk experiments.
- No real LLM adapter exists. The MockLlm generator kind is defined but not implemented.
- Derived views are not integrated with the RRF retrieval pipeline; this is a future optimization.
- The secrets/ directory is not in the default exclude list; only .env* and **/*.pem are excluded by default.

## 2026-06-11 — R5 Semantic Graph Level0 Deterministic Scaffold

### Objective

Implement a local-only, deterministic, depth=1 semantic graph scaffold. No LSP/SCIP/LLM. Graph candidates cannot directly substitute for Evidence; must be materialized through StoreHit → `openlocus_store::materialize_evidence()`.

### Implementation

1. **openlocus-graph crate**: Types (GraphNode, GraphEdge with source_content_sha/source_language, EdgeKind, GraphBuildResult, GraphCapabilities), builder, and materializer.
2. **Edge kinds**:
   - imports: parse Rust `mod/use`, TS/JS `import ... from`, Python `import/from`, Go `import` lines. Resolution uses path_set and basename_index.
   - tests: path/name heuristic (tests/ dir, *_test.*, *.test.*, test_*) linking to source files with matching basename.
   - configures: config files (Cargo.toml, package.json, etc.) to nearby source files (bounded max 50).
3. **Safe record construction**: `build_graph` validates paths and current sha, creates safe_records, and builds import/test/config edges only from safe/current records. Stale and path-unsafe records are counted and skipped.
4. **Materialization via StoreHit**: `materialize_graph_evidence()` converts GraphEdge to StoreHit and delegates to `openlocus_store::materialize_evidence(repo_root, &hit, Channel::Graph)`. Invalid ranges are rejected (not clamped). Graph-specific why/score_parts are added post-materialization without changing span/hash.
5. **Depth gate**: depth>1 returns error, not silently expanded.
6. **CLI**: `graph build`, `graph inspect` (with artifact="graph_edges_not_evidence" marker), `impact <path> --depth N` (with skipped count), `tests select --path P` (with skipped count).

### Oracle review fixes

- GraphEdge now carries source_content_sha and source_language at build time.
- materialize_graph_evidence converts to StoreHit and calls openlocus_store::materialize_evidence (not hand-building Evidence). Invalid ranges rejected (not clamped).
- build_graph builds safe_records/path_set/sha map from validated current records only; test/config edges built from safe_records only.
- graph inspect wraps output with artifact="graph_edges_not_evidence" marker.
- tests command includes skipped count.
- eval/graph_smoke.py uses synthetic temp fixture repo, includes import/test/config examples, true citation validation via `openlocus citations validate`, policy-excluded file checks, depth-2 blocking.

### R5 Level0 results (after oracle review)

| Check | Result |
|---|---|
| Graph build succeeds | ✅ |
| Edges > 0 (imports + configures + tests) | ✅ |
| Inspect has artifact marker | ✅ |
| Impact returns citation-valid evidence | ✅ verified via citations validate |
| Depth=2 blocked | ✅ clear error JSON |
| Tests select works | ✅ with skipped count |
| Stale check active (skipped_stale field) | ✅ |
| Policy excluded (.env, .pem) absent from edges | ✅ |
| StoreHit materialization gate | ✅ edges → StoreHit → materialize_evidence |

### Key findings

1. Simple line-based import parsing is surprisingly effective for Rust and Python repos. The path_set resolution approach (look for candidate files in same directory or by basename) avoids needing a full module resolver.
2. Config edges dominate because each config file links to all nearby source files. This is noisy but safe for Level0.
3. Test heuristics work for common patterns (*_test.rs, *.test.ts, tests/ dir) but don't handle inline tests or less common naming conventions.
4. Materialization via StoreHit gate is essential: graph edges carry build-time sha, and `materialize_evidence` rejects stale/invalid hits. This means graph-derived evidence is always citation-valid.
5. Graph is rebuilt on each command (not persisted). Stale records are caught at build time.

### Caveats

- This is a Level0 deterministic scaffold only. Not a precise call graph, type graph, or dependency graph.
- Import parsing is line-based heuristic, not a full parser. It will miss multi-line imports and macro-generated code.
- Go import resolution is not implemented (requires module path mapping).
- Test heuristic only links by filename/path convention, not by analyzing test content.
- Config edges are noisy (many false positives). A future version should be more targeted.
- Graph is rebuilt on each command (not persisted). For large repos, this needs caching.
- Stale detection works at build time only (graph rebuilt per command). A persistent graph would need materialization-time stale checks, which the StoreHit gate already provides.
- Graph edges are not integrated with the RRF retrieval pipeline; this is a future optimization.
