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
- **Eval**: Updated `run_retrieval.py` to accept `--method regex|text|bm25|symbol|rrf`, `--channels` for RRF. Updated `score.py` with structural_validity + citation_validity, FileRecall@1/5/10, FilePrecision@5/10, MRR, LinePrecision@10, LineRecall@10, SpanF0.5@10, token_waste_ratio@10, wrong_span_rate@10 (gold-file-only), zero_overlap_evidence_rate@10 (all evidence). Python scorer verifies path/range and verifies BLAKE3 hashes when optional Python `blake3` is installed; Rust CLI validator is used for hash/excerpt-backed citation checks. Added `fixtures/r2.jsonl` with 28 tasks, gold spans refreshed to current codebase.

### R2 eval results (after oracle review fixes)

| Metric | regex | bm25 | symbol | rrf |
|---|---|---|---|---|
| file_recall@1 | 0.21 | **0.39** | **0.39** | **0.39** |
| file_recall@5 | 0.36 | **0.86** | 0.39 | 0.82 |
| file_recall@10 | 0.50 | **0.86** | 0.39 | 0.82 |
| mrr | 0.29 | **0.58** | 0.39 | 0.56 |
| line_precision@10 | 0.06 | 0.04 | **0.39** | 0.06 |
| line_recall@10 | 0.01 | **0.04** | 0.01 | 0.04 |
| span_f0.5@10 | 0.04 | 0.04 | **0.06** | 0.06 |
| structural_validity | **1.0** | **1.0** | **1.0** | **1.0** |
| citation_validity | **1.0** | **1.0** | **1.0** | **1.0** |
| wrong_span_rate@10 | 0.77 | 0.80 | **0.0** | 0.76 |
| zero_overlap@10 | 0.92 | 0.95 | **0.0** | 0.93 |
| avg_latency_ms† | 18 | 145 | 272 | 421 |

† CLI end-to-end latency including index build (BM25/RRF) or file scan (symbol). Not a warm-index benchmark.

### Key findings

1. BM25 substantially improves file-level recall on the current self-referential fixture (0.39 vs 0.21 at k=1, 0.86 vs 0.36 at k=5). This is an early local-only bakeoff result, not a general benchmark claim.
2. Symbol search has the highest line precision (0.39) and zero wrong spans when it fires, but only activates for definition-style queries on known languages.
3. RRF fusion approaches BM25-level file recall (0.82 at k=5) while incorporating symbol precision, achieving 0.057 SpanF0.5@10 in this fixture.
4. All methods produce structurally citation-valid evidence in the Python scorer. Current aggregated R2 output was also validated by `openlocus citations validate` with `0` invalid citations (hash/range/excerpt checked).
5. Token waste ratio remains high (~0.92) — evidence spans are narrow but often miss gold lines. Query-token line scoring fixed chunk-center anchoring, but chunking/query expansion/gold-span calibration need further refinement.
6. BM25 stale check and no-overlap skip are effective: no stale or center-only hits appear in the output.
7. Symbol boundary delimiters prevent partial matches (verified: "User" does not match "UserProfile").

### Oracle review fixes applied

- **BM25 span selection**: Replaced chunk-center anchoring with line-level query token overlap scoring. Hits with no token overlap are skipped (precision-biased).
- **BM25 stale check**: Index-time content_sha compared to current file hash. Mismatch → skip. Strict line range guard added.
- **BM25 query fallback**: Removed `*` all-doc fallback; parse failures now degrade to sanitized query or empty results.
- **Symbol boundary**: Added `(?:[^a-zA-Z0-9_]|$)` delimiter after symbol name in all patterns. Prevents partial matches.
- **RRF overlap dedup**: Wider span's why/channels/score are now merged into the narrower survivor. Deterministic tie-break: score desc, path asc, start_line asc, end_line asc.
- **Eval citation validity**: Split into `structural_validity` (no I/O) and `citation_validity` (path/range by default; hash check when optional Python `blake3` is installed). Added `--repo-root`. For hash/excerpt-backed validation, use `openlocus citations validate`.
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

## 2026-06-11 — R6 Fast Context Level0 Rule Prototype

### Objective

Implement a 4-turn deterministic fast-context loop using existing regex/bm25/symbol/graph channels, returning citation-valid EvidencePack. No LLM planner, no remote calls. Budget cap enforced. Graph depth=1 only.

### Implementation

1. **openlocus-context crate**: FastContextPlan, TurnResult, TurnKind, FastContextResult, ActionRecord, FastContextDiagnostics types.
2. **4-turn loop**:
   - Turn 1 (Lexical): regex/text search, optionally BM25. Broad discovery.
   - Turn 2 (Symbol): if query has identifier-like tokens (camelCase, snake_case), run symbol search.
   - Turn 3 (Graph): get top file candidates from turns 1-2, build graph, get impact edges, materialize as Evidence via StoreHit.
   - Turn 4 (Fusion): RRF combine all channel evidence, dedup/narrow, apply token budget cap, compute confidence, generate missing_questions.
3. **Token budget**: `--budget N` means approximate max tokens (chars/4). Evidence trimmed from bottom if cumulative tokens exceed budget. `--max-evidence` still supported as count cap.
4. **ActionRecord**: per-channel, per-turn replayable actions with turn, channel, query, result_count, skipped, latency_ms, error. Written to `.openlocus/traces/fast-context-<trace_id>.json`.
5. **Unknown channel gate**: channels outside `regex,text,bm25,symbol,graph` are rejected with success=false/error.
6. **Final validation**: before output, evidence is filtered through `is_citation_valid` using safe path validation, current file hash comparison, line-range bounds, excerpt matching, and `VerifiedCurrent` freshness. Invalid citations are dropped and counted in `diagnostics.invalid_citations_dropped`.
7. **EvidencePack-compatible output**: `pack` field in result is a full EvidencePack with trace_id, budget_used. `evidence` field preserved for direct access.
8. **Disabled channels tracked**: if a channel fails or is not applicable, it's recorded in disabled_channels.

### Oracle review fixes

- Output now includes `pack` (EvidencePack-compatible) and `trace_id`.
- ActionRecord per turn/channel for replay. Trace file written to `.openlocus/traces/fast-context-<trace_id>.json`.
- `--budget` is now token budget (chars/4), not evidence count. `--max-evidence` is count cap. `tokens_estimated` is meaningful >0 when evidence exists.
- Unknown channels rejected with success=false/error JSON.
- Final validation drops invalid citations, counted in `diagnostics.invalid_citations_dropped`.
- Graph turn benefits are fixture-observed only, not a general quality claim.

### R6 Level0 results (after oracle review)

| Check | Result |
|---|---|
| Fast-context succeeds | ✅ |
| EvidencePack-compatible output | ✅ pack with trace_id |
| trace_id non-empty and matching | ✅ |
| Actions replayable | ✅ 4 actions with channel/query |
| Unknown channel blocked | ✅ error with valid channels list |
| Token budget respected | ✅ budget=50 → 3 evidence, 38 tokens |
| tokens_estimated meaningful | ✅ >0 when evidence exists |
| Citation validation (all evidence) | ✅ 100% valid |
| remote_calls = 0 | ✅ |
| Turns ≤ 4 | ✅ exactly 4 |
| Diagnostics present | ✅ invalid_citations_dropped=0 |
| No raw derived/graph edges in output | ✅ |

### Key findings

1. The 4-turn deterministic loop is a simple orchestration scaffold. It runs each channel in sequence and fuses results at the end.
2. Symbol search (Turn 2) is skipped when the query lacks identifier-like tokens, avoiding wasted computation and noise.
3. Token budget (chars/4) is a reasonable approximation. Budget=50 tokens trimmed from 20 to 3 evidence in testing.
4. All evidence is citation-valid by construction: regex/bm25/symbol search produces VerifiedCurrent evidence; graph evidence goes through StoreHit materialization. Final validation catches any that slip through.
5. The loop is deterministic: same query + same repo always produces same results.
6. Confidence is derived from the top evidence's RRF score. Low confidence (<0.1) triggers a missing_question.
7. ActionRecord enables replay/debugging of each step in the loop.

### Caveats

- This is a Level0 orchestration scaffold only, not a learned agent. No adaptive re-querying, no feedback loops, no LLM planning.
- Turn ordering is fixed (lexical → symbol → graph → fusion). A smarter planner could reorder or skip turns.
- Token estimation is chars/4, not a real tokenizer. Actual token counts may differ by 20-50%.
- Graph is rebuilt every fast-context invocation (not persisted).
- The "missing_questions" are rule-generated, not LLM-generated.
- No channel-specific error recovery beyond marking disabled_channels.
- Graph turn adds contextual evidence in fixtures; no general quality claim about graph benefit.

---

## 2026-06-11 — R7 Persistent Local Tantivy BM25 Index + Warm SLO Benchmark

### Objective

Add persistent Tantivy BM25 index with manifest tracking, CLI commands, warm SLO benchmark, and eval safety smoke. Do not change EvidenceCore contract. Do not connect TDB/LLM/dense/daemon. Persistent index requires explicit flag/command; temp BM25 path remains default.

### Hypothesis

A persistent Tantivy BM25 index with per-file content_sha tracking in a manifest will enable warm-query SLO measurement and will correctly skip stale/deleted hits without producing invalid evidence. The manifest enables fast staleness detection without re-reading all indexed content.

### Implementation notes

- **openlocus-index crate** (`crates/openlocus-index`): New workspace crate with manifest module and persistent index module.
- **Manifest** (`manifest.rs`): Schema version `r7-bm25-v1`, stored at `.openlocus/index/manifest.json`. Contains file_count, chunk_count, policy_hash (blake3 of canonical TOML serialization), and per-file entries (path, content_sha, size_bytes, language, status=indexed|skipped, skipped_reason). Manifest load/save/exists.
- **Persistent index** (`persistent.rs`): Tantivy index at `.openlocus/index/tantivy/`.
  - `build_index(repo_root, records, policy)`: Full rebuild, writes Tantivy index + manifest. Removes existing index before rebuild. Uses same chunking logic as temp BM25 (max 30 lines per chunk, same schema fields). **Filters unsafe FileRecord paths via validate_path before indexing** (path_unsafe records are skipped with reason).
  - `status_index(repo_root, policy)`: Returns exists, schema_version, file_count, chunk_count, policy_hash_matches, requires_rebuild, stale_files_fast (quick sha comparison of all indexed files).
  - `validate_index(repo_root, policy)`: Full validation of manifest entries against filesystem. Returns valid, stale_files, deleted_files, policy_hash_matches, path_unsafe_files. **Policy hash mismatch → valid=false.**
  - `purge_index(repo_root)`: Safe deletion of R7 artifacts only (manifest + tantivy dir). Canonicalizes paths and refuses to delete if index_dir escapes repo root.
  - `search_persistent_bm25(repo_root, query, max_results, policy)`: **Manifest/policy gate**: requires `.openlocus/index/manifest.json`, checks manifest policy_hash matches current Policy, and checks schema version; refuses search (bail) if missing/mismatched. **validate_path** on every Tantivy hit path before reading file. **Empty index_content_sha** → skip (cannot bypass stale check). **Strict chunk range validation**: 1 ≤ start ≤ end ≤ total_lines; invalid range → skip, no clamping. Opens existing Tantivy index, searches, then re-reads current file for every hit. Computes content_sha from current bytes, compares to index-time sha (stale → skip). Performs line-level query token scoring. Returns Evidence + SearchStats (query_ms, materialize_ms, stale_hits_skipped, invalid_hits_skipped).
  - `PersistentBm25Index::open(repo_root, policy)`: Requires manifest, opens the Tantivy Index/searcher once, validates policy hash + schema version. Returns a reusable handle with a `search()` method that does not reopen the Index per query. Used by bench warm to open once and loop queries.
- **CLI**:
  - `openlocus index build --json`: Build persistent index from scanned files.
  - `openlocus index status --json`: Quick status check.
  - `openlocus index validate --json`: Full validation against filesystem.
  - `openlocus index purge --json`: Safe deletion of R7 artifacts.
  - `openlocus search bm25 <query> --index temp|persistent --json`: Default is temp (existing per-query behavior). `--index persistent` uses pre-built index. **Policy gate enforced**: if manifest policy hash ≠ current policy, search returns an error.
  - `openlocus bench warm --dataset fixtures/r2.jsonl --iterations 3 --json`: Opens persistent index once via PersistentBm25Index handle, loops queries using the same reader/searcher. Reports index_build_ms (if built), index_open_ms (open cost only), queries, iterations, warm_query_p50_ms, warm_query_p95_ms, warm_query_max_ms, invalid_citations (real citation validation: hash/range/excerpt/freshness), stale_hits_skipped, notes.
- **Eval**: `eval/persistent_index_smoke.py` with 32 safety checks covering build/status/validate/search/stale_mutation/deleted_file/policy_excluded/policy_change_detection/manifest_missing_refusal/bench_warm/purge.
- **Safety gates (oracle review)**:
  1. **Manifest + policy gate**: search_persistent_bm25 and PersistentBm25Index::open require the manifest, check manifest policy_hash against current Policy, and refuse search if missing/mismatched.
  2. **Path validation**: validate_path on every Tantivy hit path; build_index filters unsafe FileRecord paths.
  3. **Empty content_sha**: skipped (cannot verify stale check).
  4. **Strict range**: 1 ≤ start ≤ end ≤ total_lines; no clamping.
  5. **Warm benchmark honesty**: PersistentBm25Index opens the Index/searcher once and reuses the same handle for all queries; no per-query Index::open; index_open_ms measures open cost only; index_build_ms reported separately if build was needed; invalid_citations uses real citation validation (hash/range/excerpt/freshness).

### R7 Level0 results

| Check | Result |
|---|---|
| Build succeeds and writes manifest | ✅ |
| Build filters unsafe paths via validate_path | ✅ |
| Status returns exists=true, schema matches, no rebuild needed | ✅ |
| Validate returns valid=true on fresh index | ✅ |
| Search persistent returns VerifiedCurrent evidence | ✅ |
| Search stale_hits_skipped=0, invalid_hits_skipped=0 on fresh index | ✅ |
| Policy gate: search refuses with policy hash mismatch | ✅ |
| Policy gate: validate detects policy hash mismatch | ✅ |
| Empty content_sha → skip (cannot bypass stale check) | ✅ |
| Strict range validation: 1 ≤ start ≤ end ≤ total_lines, no clamping | ✅ |
| validate_path on every Tantivy hit path before reading file | ✅ |
| Stale mutation: modified file → no VerifiedCurrent evidence for stale hit | ✅ |
| Stale mutation: stale_hits_skipped > 0 or zero results | ✅ |
| Validate detects stale files after mutation | ✅ |
| Deleted file: search produces no evidence for deleted file | ✅ |
| Policy excluded files (.env, .pem) absent from persistent output | ✅ |
| Policy change after build: search refuses | ✅ |
| Policy change after build: validate detects mismatch | ✅ |
| Manifest missing: search refuses instead of trusting Tantivy dir alone | ✅ |
| Bench warm succeeds with p50/p95/max latency | ✅ |
| Bench warm invalid_citations=0 (real citation validation) | ✅ |
| Purge removes manifest and tantivy dir | ✅ |
| Temp BM25 still works as default | ✅ |

### Warm SLO benchmark results (on current codebase)

```json
{
  "index_build_ms": null,
  "index_open_ms": 1,
  "queries": 28,
  "iterations": 3,
  "warm_query_p50_ms": 1,
  "warm_query_p95_ms": 2,
  "warm_query_max_ms": 2,
  "invalid_citations": 0,
  "stale_hits_skipped": 0
}
```

Note: These are implementation notes and initial SLO measurements on a small self-referential fixture. They are not a general performance claim. The `index_open_ms` measures the cost of opening an already-built Tantivy index and validating the manifest policy/schema gates — this is the actual warm-start cost. The `index_build_ms` is reported separately if the index needed to be built; it is null here because the index already existed. The warm_query_p50_ms reflects search-only cost after the index handle is opened once. The `invalid_citations` count uses real citation validation (hash/range/excerpt/freshness checks), not just range checks.

### Key findings

1. **Manifest-based staleness detection works**: After building an index, modifying a file, and running `search persistent`, the stale hit is correctly skipped (stale_hits_skipped > 0). No stale VerifiedCurrent evidence is emitted.
2. **Deleted file safety**: After deleting a file, searching the persistent index produces no evidence for the deleted file. The read failure is counted as invalid_hits_skipped.
3. **Policy exclusion works**: Files matching policy exclude patterns (.env, *.pem) are not scanned and not indexed, so they never appear in persistent search output.
4. **Validate detects staleness**: After modifying a file, `index validate` correctly reports the file as stale and marks the index as invalid.
5. **Warm query latency is fast on the current fixture**: On the current small self-referential workspace snapshot, warm queries take 1-2ms per query after index is opened (1ms open cost). The PersistentBm25Index handle opens once and reuses the same Index/searcher for all queries.
6. **Safety is preserved**: Every persistent search hit is re-verified against the current filesystem. The Tantivy stored body is never used as the final excerpt; the excerpt is always built from the current file content.
7. **Manifest and policy gates work**: Changing the policy after building the index causes search to refuse (error) and validate to report policy_hash_matches=false. Deleting the manifest also causes persistent search to refuse instead of trusting the Tantivy directory alone. This prevents querying unverifiable or stale-policy indexes.
8. **Strict validation prevents bypass**: Empty content_sha hits are skipped (cannot verify stale check). Chunk ranges are strictly validated (1 ≤ start ≤ end ≤ total_lines) with no clamping — invalid ranges are skipped.

### Caveats carried forward

- This is a Level0 persistent index implementation. Incremental update added in R10; the index still becomes stale when files change until `index update --dirty` is run.
- The manifest stores per-file content_sha at build time; it does not track per-chunk sha. If a file changes, all chunks from that file are considered stale (conservative).
- Warm SLO numbers are from a small self-referential codebase snapshot. Performance on larger repos may differ significantly.
- Persistent index does not integrate with RRF or fast-context pipelines yet; it is accessible only via `search bm25 --index persistent`.
- No daemon/watch mode; index becomes stale when files change and must be manually rebuilt.
- The index is stored under `.openlocus/index/` which may grow large for big repos. No size limit or rotation is implemented.
- Tantivy index is not encrypted; content is stored in the Tantivy segment files under `.openlocus/index/tantivy/`.
- R7 Level0 passed only after oracle review gates (policy hash, validate_path, empty sha, strict range, honest warm benchmark).

## 2026-06-11 — R8 AST-bounded chunking + AST symbol extraction (experimental)

### Objective

Add Tree-sitter-based AST chunking and symbol extraction as an experimental, opt-in alternative to line-window chunking. AST mode changes candidate chunk/symbol boundaries; final Evidence still requires current-filesystem path/range/hash/excerpt/freshness verification. Line-window chunking remains the default.

### Current hypothesis

AST-bounded chunks should improve retrieval precision by aligning chunk boundaries with logical code structures (functions, classes, structs, etc.) rather than arbitrary line windows. However, the quality lift is not yet proven by eval; this is an experimental scaffold.

### Stage gates

- R8 passes when AST build/search/validate/status work correctly, AST symbol search returns tree_sitter-channel Evidence, stale mutation is detected, fallback (unsupported language, parse error) is correct, and R7 persistent smoke still passes.
- AST mode is opt-in (`--chunk-strategy ast`); line remains default.
- No quality claim unless eval computes it.

### Implementation notes

- New crate `openlocus-ast` with Tree-sitter 0.25.x + language grammars (Rust, Python, JavaScript, TypeScript).
- `extract_ast_chunks(path, language, source, max_lines)`: Parses source with Tree-sitter for supported languages, extracts definition nodes (fn/struct/enum/trait/impl/mod/type/macro for Rust; function_definition/class_definition/decorated_definition for Python; function/class/method/lexical/variable/export for JS/TS plus interface/type/enum for TS). Oversized nodes split into line windows. Gaps covered by fallback line windows. No overlapping chunks. Strict 1-based line ranges. Unsupported language or parse error → fallback line windows.
- `extract_ast_symbols(path, language, source)`: Extracts narrow header/signature spans (max 10 lines, usually signature/header only rather than full bodies) for definitions. Uses Tree-sitter node field "name" for symbol extraction, with special handling for decorated Python definitions. Unsupported/parse error → empty symbols + fallback status; callers use regex fallback.
- Manifest schema version changed to `r8-bm25-v2`. R7 manifests (`r7-bm25-v1`) still loadable with default chunk_strategy=line_window_v1. Unrecognized schema versions refuse with rebuild instruction.
- `ChunkStrategy` enum: `LineWindowV1` (default), `AstV1` (experimental). Stored in manifest.
- `AstManifestStats` in manifest: supported_files, fallback_files, parser_error_files, ast_chunks, fallback_chunks.
- CLI: `openlocus index build --chunk-strategy line|ast` (default line). `openlocus search symbol <name> --mode regex|ast|auto` (default auto: AST first for supported files, regex fallback).
- Search/validate/status refuse if manifest chunk_strategy is unrecognized. R7 manifests without chunk_strategy are loaded as line_window_v1 for compatibility; R8-written manifests always include chunk_strategy. No silent search of unverifiable strategy.
- AST symbol Evidence uses Channel::TreeSitter, narrow span, current-file hash/excerpt/freshness validation. Regex symbol Evidence uses Channel::Regex (unchanged).
- `eval/ast_chunking_smoke.py`: 40 safety checks covering AST build/status/validate/search, parser-error visibility, stale mutation, narrow AST symbol header, symbol search modes, citation validation, schema mismatch, policy exclusion, default line build compatibility.

### R8 AST chunking safety checks

| Check | Result |
|---|---|
| Line build succeeds (default strategy) | ✅ |
| Line build has chunk_strategy=line | ✅ |
| Line build has no ast_stats | ✅ |
| AST build succeeds | ✅ |
| AST build has chunk_strategy=ast | ✅ |
| AST build has ast_stats | ✅ |
| AST stats: supported_files > 0 | ✅ |
| AST stats: ast_chunks > 0 | ✅ |
| AST stats: parser_error_files visible | ✅ |
| AST status: chunk_strategy=ast | ✅ |
| AST status: has ast_stats | ✅ |
| AST status: no rebuild needed | ✅ |
| AST validate: valid | ✅ |
| AST validate: chunk_strategy=ast | ✅ |
| AST search persistent: returns evidence | ✅ |
| AST search: all freshness verified_current | ✅ |
| Stale mutation: no verified_current evidence for stale file | ✅ |
| Stale mutation: stale_hits_skipped > 0 | ✅ |
| Validate detects stale after mutation | ✅ |
| AST symbol search: returns results | ✅ |
| AST symbol: uses tree_sitter channel | ✅ |
| AST symbol: header/signature span, not full body | ✅ |
| Regex symbol search: returns results | ✅ |
| Auto symbol search: returns results | ✅ |
| Policy excluded files absent | ✅ |
| Citation validation: valid | ✅ |
| Citation validation: invalid_count=0 | ✅ |
| Bad schema version: search refuses | ✅ |
| Line build after AST: succeeds | ✅ |
| Line build after AST: strategy=line | ✅ |

### Key findings

1. **AST chunking is functional as an experimental scaffold**: Tree-sitter parsing works for Rust, Python, JavaScript, and TypeScript. Chunk boundaries align with logical code structures.
2. **Fallback is correct**: Unsupported languages fall back to line-window chunking. Parse errors also fall back. No data loss.
3. **Oversized node splitting works**: Functions exceeding max_lines are split into line windows with kind=fallback_line_window.
4. **Gap filling ensures complete coverage**: No gaps or overlaps between chunks.
5. **AST symbol extraction produces narrow, citation-valid Evidence**: Symbols use Channel::TreeSitter, narrow header/signature spans (max 10 lines, usually signature/header only rather than full bodies), and are verified against the current filesystem.
6. **R7 persistent smoke still passes**: Default line build continues to work. Schema version is backward-compatible (R7 manifests loadable).
7. **Quality lift is NOT proven**: This is an experimental scaffold. No comparative eval has been run to measure retrieval quality improvement from AST chunking vs line-window chunking.

### Caveats

- AST mode is experimental and opt-in. Line-window chunking remains the default and recommended strategy.
- Tree-sitter parsers may have edge cases or version-specific behavior. The quality of chunking depends on the grammar quality.
- AST symbol extraction does not handle all possible symbol patterns (e.g., re-exports, aliased imports, nested definitions). Regex fallback covers many of these.
- The `auto` mode for symbol search tries AST first for supported files and falls back to regex if no results. This may miss cases where regex finds results that AST does not.
- No incremental update for AST index in this R8 iteration; R10 adds incremental update for both line and ast strategies.
- Manifest schema version `r8-bm25-v2` is not compatible with `r7-bm25-v1` for search purposes (older schemas load but may require rebuild for search).
- AST chunking adds Tree-sitter as a dependency, increasing binary size and compile time.

## 2026-06-11 — R9 AST vs Line Persistent BM25 Quality Bakeoff

### Objective

Evaluate `index build --chunk-strategy line` vs `--chunk-strategy ast` persistent BM25 retrieval quality on the R2 self-referential fixture. Produce a reproducible script and research log. Do not change EvidenceCore, default behaviour, or implement incremental indexing.

### Hypothesis

AST-bounded chunks should improve span-level precision/recall (SpanF0.5@10) by aligning chunk boundaries with logical code structures, but the effect may be small or negative on this small self-referential fixture. File-level recall may not improve because AST chunking changes span boundaries, not file-level ranking.

### R9 bakeoff results (persistent BM25 on fixtures/r2.jsonl, 28 tasks)

| Metric | line | ast | delta |
|---|---:|---:|---:|
| FileRecall@1 | 0.393 | 0.536 | +0.143 |
| FileRecall@5 | 0.821 | 0.750 | −0.071 |
| FileRecall@10 | 0.821 | 0.821 | 0.000 |
| MRR | 0.548 | 0.624 | +0.076 |
| SpanF0.5@10 | 0.039 | 0.064 | +0.025 |
| token_waste_ratio@10 | 0.961 | 0.940 | −0.022 |
| wrong_span_rate@10 | 0.776 | 0.689 | −0.087 |
| zero_overlap_evidence_rate@10 | 0.950 | 0.913 | −0.038 |
| citation_validity | 1.0 | 1.0 | 0.0 |
| structural_validity | 1.0 | 1.0 | 0.0 |
| success_rate | 1.0 | 1.0 | 0.0 |
| avg_latency_ms | ~10.4 | ~9.1 | noisy/comparable |
| latency_ratio | — | ~1.0 | noisy |

### Quality gate

| Gate condition | Result |
|---|---|
| Both citation_validity == 1.0 | ✅ |
| Both success_rate == 1.0 | ✅ |
| AST FileRecall@5 ≥ line | ❌ (0.750 < 0.821) |
| AST SpanF0.5@10 ≥ line | ✅ (0.064 > 0.039) |
| AST token_waste not worse | ✅ (0.940 < 0.961) |
| Latency ratio ≤ 1.25 | ✅ (comparable; latest run ~0.88) |
| **Overall quality_gate_passed** | **false** (FileRecall@5 regression) |

### Safety checks

| Safety check | Result |
|---|---|
| Line build succeeds | ✅ |
| AST build succeeds | ✅ |
| Line validate valid | ✅ |
| AST validate valid | ✅ |
| Line status strategy=line | ✅ |
| AST status strategy=ast | ✅ |
| Evidence nonempty (both) | ✅ |
| Citation validator invalid_count=0 (both) | ✅ |
| Search stats counters present/nonnegative (both) | ✅ |
| Build strategy explicit (both) | ✅ |
| **All safety checks passed** | **true** |

### Key findings

1. **AST improves SpanF0.5@10 by +0.025** (0.039 → 0.064, ~63% relative in the latest run). AST-bounded chunks align better with logical code structures, producing evidence spans that overlap gold lines more often. This is the strongest positive signal for AST chunking.
2. **AST improves FileRecall@1 by +0.143** (0.393 → 0.536, +36% relative). The finer-grained AST chunks appear to help top-1 file retrieval, likely because more specific chunks match queries more precisely at the top rank.
3. **AST regresses FileRecall@5 by −0.071** (0.821 → 0.750 in the latest run). This is the quality gate failure. AST chunking produces more, smaller chunks (e.g., 15 chunks vs 5 for a 5-file repo), which can dilute BM25 scores for some files, causing them to rank outside top-5. This is a real trade-off: finer chunks help precision at k=1 but may hurt recall at higher k because the same file's chunks compete with each other.
4. **AST reduces token_waste_ratio@10 by −0.022** (0.961 → 0.940 in the latest run). Narrower, more targeted evidence spans waste fewer tokens on irrelevant lines. The improvement is modest on this fixture.
5. **AST reduces wrong_span_rate@10 by −0.087** (0.776 → 0.689 in the latest run). AST-bounded evidence on gold files is more likely to overlap gold spans, reducing the rate of evidence that hits the right file but the wrong location.
6. **Latency is comparable/noisy** (latest ratio ~0.88). Both strategies have similar per-query latency in this tiny CLI benchmark; this is not a general performance claim.
7. **Citation validity and structural validity are perfect for both strategies** (1.0). AST chunking does not compromise evidence safety.
8. **Quality gate is false** due to FileRecall@5 regression. This is a negative result on the gate but not on safety. The gate correctly captures the trade-off.

### Interpretation

**AST remains experimental/opt-in. Line remains default.**

The R9 bakeoff shows a mixed picture: AST improves span precision (SpanF0.5, token waste, wrong_span_rate) and top-1 recall (FileRecall@1, MRR), but regresses FileRecall@5. On this small self-referential fixture, the trade-off is real but the magnitude is small. The regression at FileRecall@5 likely reflects chunk-score dilution: more granular AST chunks can scatter a file's BM25 signal across many smaller chunks, reducing the chance that any single chunk ranks the file into the top-5.

Whether this trade-off is acceptable depends on the use case: for top-1 precision and span quality, AST is preferable; for broad file discovery at k=5+, line chunking is more conservative. The fixture is too small and self-referential to generalise these findings. A larger, diverse codebase eval would be needed to determine whether AST chunking provides a consistent quality improvement.

### Caveats

- Fixture is R2 small self-referential (28 tasks, ~20 source files). Results are not generalisable.
- FileRecall@5 regression may be an artifact of chunk granularity on small repos; larger repos may not exhibit the same trade-off.
- No incremental indexing in this R9 bakeoff; R10 adds incremental update.
- The bakeoff only tests persistent BM25; temp BM25, RRF, and fast-context are not compared here.
- token_waste_ratio remains high (~0.94) for both strategies. The bottleneck is query-evidence alignment, not chunking strategy.
- Python citation validation uses path_range_only mode (no blake3 Python package installed); Rust CLI citation validator confirmed invalid_count=0 for both strategies.
- AST mode is experimental and opt-in. Line-window chunking remains the default and recommended strategy.

## 2026-06-11 — R10 Incremental Index + Dirty Summary + Synthetic SLO

### Objective

Add persistent index dirty summary, file-level incremental update, context-lite dirty integration, and synthetic SLO benchmark. Do not implement TDB/daemon/watcher. Do not add LLM/dense. EvidenceCore unchanged.

### Current hypothesis

A manifest-vs-filesystem dirty summary enables incremental index updates that avoid full rebuilds for small changes. File-level update with Tantivy delete-by-term + re-add should correctly maintain the index without producing duplicate or stale evidence. Policy/schema/strategy mismatches must refuse update, requiring rebuild instead. Note: the Tantivy commit and manifest file write are not a single transaction; failure between them may leave a safe but inconsistent state requiring rebuild or re-update.

### Implementation notes

- **Dirty summary** (`dirty_index`): Computes manifest-vs-current scan returning `DirtyResult` with:
  - `clean`: bool (true if no changes and policy/schema match)
  - `requires_update`: bool (true if added/modified/deleted files, no rebuild needed)
  - `requires_rebuild`: bool (true if policy/schema/strategy mismatch or no index)
  - `added_files`, `modified_files`, `deleted_files`: arrays of path strings
  - `added_count`, `modified_count`, `deleted_count`: u64 counts
  - `policy_hash_matches`, `schema_matches`: bool flags
  - `chunk_strategy`: from manifest
  - Discovers policy-included added files not in manifest; policy-excluded added files do not dirty.
  - Status must not say clean if validate would fail.

- **File-level update** (`update_index`): CLI: `openlocus index update --dirty --json` and/or `openlocus index update --path <path> --json`.
  - If index/manifest missing → error (requires rebuild).
  - If policy hash/schema/strategy mismatch → refuse update, require rebuild.
  - For `--dirty`: compute added/modified/deleted from dirty summary, then:
    - Delete old Tantivy docs by path using `Term::from_field_text(path_field, path)`.
    - For added/modified policy-included files: validate_path, read current file, compute sha, chunk according to manifest chunk_strategy, add docs.
    - For deleted files: delete docs by path, remove manifest entry.
    - Commit once after batch.
    - Write manifest file using tmp+rename (not a single transaction with Tantivy commit; failure between may require rebuild/update).
  - Added detection uses ALL manifest paths (indexed + skipped), not just indexed. Skipped entries with unchanged sha remain clean; skipped→nonempty is reported as modified (not added).
  - For `--path`: update only that policy-included path. If file exists add/update; if missing delete from index. Validate path safety.
  - Returns `UpdateResult` with `added_count`, `modified_count`, `deleted_count`, `commit_ms`, `manifest_written`, `post_status_clean`.
  - Tantivy deletes are tombstones until merge; documented, not a bug.
  - Prevents duplicate old+new docs by delete_term(path) before add.
  - Chunk according to manifest chunk_strategy; does not mix strategies.

- **Context-lite integration**: R10 populates dirty-summary.json with actual dirty index status (clean, requires_update, requires_rebuild, added/modified/deleted counts and files) instead of the R1 empty placeholder. If index doesn't exist, reports requires_rebuild=true.

- **CLI commands**:
  - `openlocus index dirty --json`: Show dirty summary.
  - `openlocus index update --dirty --json`: Update all dirty files.
  - `openlocus index update --path <path> --json`: Update single file.

- **Eval scripts**:
  - `eval/incremental_index_smoke.py`: 48 safety checks covering build/clean, modify/update/search/clean, add/update/search/clean, delete/update/search/clean, rename simulation, policy-excluded no dirty, policy mismatch refuses update, missing manifest refuses update, skipped empty file clean/promotion, schema/strategy mismatch refuses update, citations invalid_count=0.
  - `eval/synthetic_slo_bench.py`: Deterministic 1000-file synthetic repo (mix .rs/.py/.ts/.md/.txt), measures build_ms, dirty status latency, persistent_cli_search p95, bench_warm open-once query p95, and one-file update latency (true modification each iteration). Validates no invalid citations. Level0 synthetic only; no broad performance claims.

### R10 incremental index smoke results (48/48 checks passed)

| Check | Result |
|---|---|
| Build succeeds, file_count > 0 | ✅ |
| Dirty clean after build | ✅ |
| Dirty: no requires_update/rebuild after build | ✅ |
| Dirty: policy_hash_matches + schema_matches | ✅ |
| Modify file → dirty detects modified | ✅ |
| Search before update: no stale VerifiedCurrent | ✅ |
| Update dirty: modifies file, manifest written | ✅ |
| Search after update: finds new content | ✅ |
| Dirty clean after update | ✅ |
| Add new file → dirty detects added | ✅ |
| Update dirty: adds file | ✅ |
| Search finds added file | ✅ |
| Dirty clean after add update | ✅ |
| Delete file → dirty detects deleted | ✅ |
| Update dirty: deletes file | ✅ |
| Search: no evidence for deleted file | ✅ |
| Dirty clean after delete update | ✅ |
| Rename: old gone, new found | ✅ |
| Policy-excluded added file does not dirty | ✅ |
| Policy hash mismatch refuses update | ✅ |
| Missing manifest refuses update | ✅ |
| Citations: invalid_count=0 | ✅ |
| Purge after smoke succeeds | ✅ |

### R10 synthetic SLO benchmark results (1000 files, seed=42)

```json
{
  "build_ms": 147,
  "build_file_count": 1000,
  "build_chunk_count": 1072,
  "dirty_status_latency_ms": {"p50": 44, "p95": 48, "max": 48},
  "dirty_clean": true,
  "persistent_cli_search_latency_ms": {"p50": 13, "p95": 14, "max": 15},
  "bench_warm": {"index_open_ms": 5, "queries": 10, "iterations": 3, "warm_query_p50_ms": 0, "warm_query_p95_ms": 0, "warm_query_max_ms": 1, "invalid_citations": 0},
  "total_invalid_citations": 0,
  "one_file_update_latency_ms": {"p50": 115, "p95": 117, "max": 126},
  "update_success": true,
  "update_modified_count_ok": true,
  "all_safety_checks_passed": true
}
```

Note: These are Level0 synthetic-only measurements on a deterministic 1000-file fixture. They are not a general performance claim. Actual performance on real-world repos will vary with file sizes, content diversity, and hardware. `persistent_cli_search_latency_ms` measures CLI search (each call opens index fresh); `bench_warm` reports the Rust CLI's internal open-once query latency over a synthetic dataset. The dirty_status_latency includes a full file rescan which is bounded by the number of indexed files; for very large repos this would need optimization (e.g., filesystem watchers, mtimes). One-file update latency reflects a true modification (different content each iteration), not a no-op.

### Key findings

1. **Incremental update works correctly**: Modified, added, and deleted files are properly detected, updated in the Tantivy index, and the manifest is written using tmp+rename. Post-update status shows clean.
2. **Dirty summary is accurate**: It correctly identifies added/modified/deleted files, distinguishes requires_update from requires_rebuild, and does not report policy-excluded files as dirty.
3. **Safety gates are enforced**: Policy hash mismatch and missing manifest refuse update with clear error messages. Schema and strategy mismatches also refuse update.
4. **Search-after-update correctness**: After updating modified files, persistent search returns only new content (verified_current freshness). After deleting files, no evidence appears. After adding files, new files are discoverable.
5. **Tantivy delete-by-term works**: Using `Term::from_field_text(path_field, path)` correctly removes all chunks for a given path before re-adding, preventing duplicate old+new docs.
6. **Manifest file write uses tmp+rename**: This prevents partial manifest writes. However, the Tantivy commit and manifest write are not a single transaction; a crash between them may leave a safe but inconsistent state (Tantivy committed but manifest stale), requiring a rebuild or re-update.
7. **One-file update latency is reasonable for synthetic small edits**: ~115ms p50 for a true single-file update in a 1000-file repo (dev profile, unoptimized). This includes scan + dirty computation + Tantivy write + manifest write.
8. **Dirty status latency scales with file count**: ~44ms p50 for 1000 files in dev build. For very large repos, filesystem watchers or mtime-based optimization would be needed.
9. **Context-lite dirty summary is now written to file**: R10 replaces the R1 empty placeholder by writing actual dirty counts and status information to `.openlocus/context/dirty-summary.json`. The `ContextLitePack.dirty_summary` struct field remains `None` (the file is the authoritative source, not the struct field).

### Caveats

- This is a Level0 incremental index implementation. Not a daemon/watcher; index becomes stale when files change and must be manually updated via `index update --dirty` or `--path`.
- Tantivy deletes are tombstones until merge. For repos with frequent updates, periodic full rebuild or explicit merge may be needed to reclaim space.
- Chunk count in manifest after update is approximate (old_count + new_chunks - estimated_deleted_chunks). A full rebuild produces exact counts.
- Dirty summary re-scans all indexed files to compare content_sha. For very large repos, this is O(n) in indexed file count. A filesystem watcher or mtime cache would be needed for sub-second dirty detection.
- The update does not handle branch switches or concurrent modifications; a rebuild is recommended after significant changes.
- AST strategy update uses the same `compute_chunks` helper as build; it respects the manifest's chunk_strategy and does not mix strategies.
- The Tantivy commit and manifest file write are not a single transaction. If the process crashes between them, the index may be in a safe but inconsistent state (Tantivy committed, manifest stale). A rebuild or re-update resolves this.
- Skipped entries (empty files, read errors, path_unsafe) are tracked in the manifest; they do not appear as "added" on subsequent dirty scans if sha is unchanged. A skipped→nonempty transition is reported as "modified" and the update promotes it to "indexed".
- R7/R8/R9 smoke tests still pass after R10 changes.
- TDB moves to a later research stage (was R10 in original roadmap, now deferred to R11).

---

## 2026-06-11 — R11 TriviumDB/TDB Feature-Gated Level0 Adapter Probe

### Objective

Implement a feature-gated TriviumDB (TDB) adapter behind a `tdb` Cargo feature, proving the StoreBackend / ChunkStore trait hierarchy can be wired to a real TDB instance. This is a Level0 adapter probe for metadata/chunk persistence only — not a retrieval quality claim. If the TDB crate was not compilable or its API was unsuitable, the fallback was a negative feasibility report.

### Current hypothesis

TriviumDB 0.7.0 (from crates.io) can be used as an optional dependency behind a Cargo feature flag to store chunk metadata as JSON payloads in TDB nodes with dim=1 dummy vectors. The adapter should pass the same build discipline as ConservativeChunkStore (validate_path, TOCTOU-safe sha, skip stale/traversal/empty) and honestly report capabilities (metadata+chunks only, no lexical/vector/graph claims). TDB hits must go through StoreHit → materialize_evidence(); they cannot directly become Evidence.

### Implementation notes

- **Cargo feature**: Added `[features] default = []` and `tdb = ["dep:triviumdb"]` to `crates/openlocus-store/Cargo.toml`. Added `triviumdb = { version = "=0.7.0", optional = true }`. Workspace/default build does NOT enable the feature; `cargo test --workspace` does not compile or require TriviumDB.
- **TdbChunkStore** (`crates/openlocus-store/src/tdb_adapter.rs`): Behind `#[cfg(feature = "tdb")]`.
  - Opens a `Database<f32>` with `dim=1` and stores chunk metadata as JSON payloads with schema `openlocus_schema=tdb_chunk_v1`.
  - The vector `[0.0]` is a smoke probe only — this is NOT vector quality. Documentation explicitly states this.
  - Payload schema: `openlocus_schema`, `path`, `start_line`, `end_line`, `content_sha`, `language`, `kind`.
  - Build discipline copies ConservativeChunkStore: `validate_path`, read current bytes once, sha from same bytes, skip stale/traversal/empty invalid chunks; line chunks OK.
  - Uses `db.insert(&[0.0f32], payload)` with auto-assigned NodeIds. In-memory `ChunkRecord` list maintained for conformance.
  - Capabilities: metadata=true, chunks=true, lexical=false, vector=false, graph=false.
  - Marker file (`.openlocus_marker`) written alongside the `.tdb` file to identify adapter-owned data.
  - Purge only deletes the adapter-owned TDB artifact set (`.tdb` plus known sidecars) after verifying the marker. Refuses purge if marker is missing or mismatched.
  - Ingest only from `scan_repo` filtered records; never walks filesystem itself.
  - If adapter exposes hits, they must be converted to `StoreHit` and go through `materialize_evidence()`.
- **Module registration**: `#[cfg(feature = "tdb")] pub mod tdb_adapter;` in `lib.rs`.
- **Placeholder preserved**: `TdbPlaceholderStore` (always available) unchanged — it still reports `available=false` when the `tdb` feature is not enabled.

### R11 Level0 adapter probe results

| Check | Result |
|---|---|
| `cargo test --workspace` (default, no tdb) | ✅ 193 passed |
| `cargo test -p openlocus-store --features tdb` | ✅ 28 passed (18 existing + 10 new adapter) |
| `cargo fmt --all -- --check` | ✅ clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ clean |
| `cargo clippy -p openlocus-store --features tdb -- -D warnings` | ✅ clean |
| TDB adapter health available when open | ✅ |
| TDB adapter build from records | ✅ chunks, files, valid ranges |
| TDB adapter capabilities conservative | ✅ metadata+chunks only, no lexical/vector/graph |
| TDB adapter skips stale records | ✅ test passes |
| TDB adapter skips empty files (no invalid chunks) | ✅ test passes |
| TDB adapter skips traversal records | ✅ test passes |
| TDB adapter purge marker-owned only | ✅ deletes .tdb + known sidecars + marker |
| TDB adapter purge refuses without marker | ✅ error with marker/refused |
| TDB adapter materialization conformance | ✅ StoreHit → materialize_evidence → VerifiedCurrent |
| TDB adapter materialization rejects stale | ✅ StaleHit |
| TDB adapter materialization rejects empty sha | ✅ error |
| TDB adapter materialization rejects invalid range | ✅ error |
| Default build unchanged | ✅ no TDB dependency in default |
| Placeholder unchanged | ✅ TdbPlaceholderStore preserved |
| EvidenceCore unchanged | ✅ |
| No default dependency on triviumdb | ✅ optional only |

### Key findings

1. **TriviumDB 0.7.0 compiles and works**: The crate from crates.io compiles with the workspace's Rust edition (2024) and toolchain. `Database::open(path, dim=1)` creates/opens a database; `db.insert(&[0.0f32], payload)` inserts nodes with JSON payloads; `db.flush()` persists to disk.
2. **Feature-gated adapter works correctly**: The `tdb` feature flag cleanly gates the adapter. Default builds do not compile or require TriviumDB. The adapter module is only included when the feature is enabled.
3. **Build discipline is preserved**: The TDB adapter follows the same ConservativeChunkStore discipline — validate_path, TOCTOU-safe sha computation, stale/traversal/empty skipping. This ensures consistent behavior across store backends.
4. **dim=1 is a smoke probe, not vector quality**: The adapter explicitly does not claim vector search capability. The `[0.0]` vector is used solely to satisfy TDB's API requirement. Capabilities honestly report vector=false.
5. **Marker-based purge is safe**: The adapter writes a marker file alongside the `.tdb` file. Purge verifies the marker before deletion and refuses to delete paths without a valid marker. This prevents accidental deletion of arbitrary files.
6. **Materialization conformance is enforced**: TDB chunk records are converted to StoreHit and must go through `materialize_evidence()`. Stale, empty-sha, and invalid-range hits are correctly rejected.
7. **TDB is NOT a default backend**: The adapter is feature-gated and opt-in. TDB does not replace Tantivy persistent BM25 or the conservative store. The placeholder store continues to report unavailability when the feature is not enabled.
8. **This is a Level0 adapter probe**: No retrieval quality claim is made. The adapter proves wiring and persistence; it does not provide meaningful lexical, vector, or graph search through TDB.

### Caveats

- This is a Level0 adapter probe only. No retrieval quality comparison against Tantivy BM25 or conservative store.
- The TDB adapter uses auto-assigned NodeIds (via `db.insert()`). The `insert_with_id` API exists but requires predetermined IDs; this is a possible future enhancement for deterministic chunk IDs.
- The TDB adapter does not implement `LexicalStore`, `VectorStore`, or `GraphStore`. It only provides `StoreBackend` + `ChunkStore`.
- TDB file locking prevents multiple processes from opening the same database simultaneously (by design in TriviumDB).
- Chunk insert and flush failures now fail the Level0 build instead of silently claiming persistence success.
- No CLI integration for TDB adapter in this R11 iteration. The adapter is only usable programmatically.
- TDB is not integrated with the RRF retrieval pipeline or fast-context orchestration.
- The `TdbPlaceholderStore` still exists and reports `available=false` regardless of whether the `tdb` feature is enabled. A future iteration could make the placeholder aware of the feature flag.
- Warm/persistent query benchmark not yet run against the TDB adapter (not meaningful until search is implemented).

---

## 2026-06-11 — R12 Real-Repo Incremental Robustness Benchmark

### Objective

Add a real-repo sample benchmark using a safe temporary copy of the current OpenLocus repository to test R10 incremental index's real-scenario robustness. Do not change Rust core, default CLI/search/retrieve, and do not introduce watcher/daemon/TDB changes.

### Current hypothesis

R10 proved incremental update works on a synthetic 1000-file repo. A real repo with mixed file types, larger files, and real code patterns should also pass all safety gates (no stale VerifiedCurrent evidence, correct dirty detection, valid citations). Incremental update should be faster than full rebuild for small edits on a real repo.

### Implementation notes

- **New eval script**: `eval/real_repo_incremental_bench.py` with `report_kind=real_repo_incremental_bench`.
- **Source repo**: defaults to current working directory; copies to temp repo excluding `target`, `.git`, `.openlocus`, `runs`, `node_modules`, `dist`, `__pycache__`, etc. Creates empty `.git/` and `.openlocus/policy.toml` in temp repo.
- **All workload file mutations only in temp repo**: original source files are never modified. `--out` writes report to caller workspace.
- **Per-run unique markers**: Each run generates a random 8-hex-char suffix (e.g. `a3f7b2c1`). All markers are concatenated with this suffix (e.g. `r12addalphaa3f7b2c1`). Before baseline build, `assert_markers_absent` scans the temp repo to confirm no marker self-contamination from copied docs/scripts.
- **All search**: `openlocus search bm25 <query> --index persistent --json`. Search returncode must be 0 for positive gates.
- **Collected marker-search evidence validation**: `openlocus citations validate <file> --json` with `invalid_count=0` and validator returncode==0.
- **Positive gates use path+marker conjunction**: `evidence_has_path_and_marker` requires BOTH the target relative path AND the marker in the cited excerpt. Disjunction (path OR marker) is not used for positive assertions.
- **Empty evidence is not a pass for positive gates**: Where a marker must be found, `len(evidence) > 0` is required.
- **`sys.exit(1)` on safety failure**: `all_safety_checks_passed` false exits with code 1. Latency/growth gate false does NOT cause exit failure.
- **Latency comparison uses twin repo copies**: For each iteration, two fresh copies of the source repo are made. Same mutation is applied to both; one gets `update --dirty`, the other gets `build`. This ensures fair comparison of the same final state.
- **Growth gate renamed to catastrophic guard**: `growth_catastrophic_guard_passed` (not "bounded proof"). Observed 20-cycle growth ratio reported separately. The catastrophic bound (max(3×rebuild, rebuild+64MiB)) is a backstop, not proof of long-term bounded growth.
- **Branch-like batch checks are specific**: At least one add target path+marker AND one rename-new target path+marker must be found. At least one deleted path and one rename-old path must have no VerifiedCurrent evidence.

### Script CLI

```bash
python3 eval/real_repo_incremental_bench.py \
  --openlocus target/debug/openlocus \
  --source /workspace/OpenLocus/OpenLocus \
  --out runs/real-repo-incremental-bench.json \
  --iterations 3 \
  --growth-cycles 20
```

Optional `--keep-temp` to preserve temp repo after benchmark.

### Workloads

A. **modify_one**: Append per-run marker to existing file; dirty detects modified; search old marker has no VerifiedCurrent for modified path; update; new marker found at path with marker in evidence (path AND marker); old marker gone; clean + valid + citations invalid_count=0.

B. **add_one**: Add `r12_bench/r12_add_target_{sfx}.rs` with per-run marker; dirty detects added; update; search finds marker at path with marker in evidence; clean + valid + citations invalid_count=0.

C. **delete_one**: Delete `r12_bench/r12_delete_target_{sfx}.rs`; dirty detects deleted; update; search marker no VerifiedCurrent for deleted path; clean + valid + citations invalid_count=0.

D. **rename_one**: Rename `r12_rename_old_{sfx}.rs` → `r12_rename_new_{sfx}.rs` + change marker; dirty detects added+deleted; update; old gone (no VerifiedCurrent for old path); new found at new path with marker (path AND marker); clean + valid + citations invalid_count=0.

E. **policy_exclude**: Add `.env.r12bench{sfx}` with marker; dirty stays clean; no evidence from excluded path (path-aware check).

F. **branch_like_batch**: Batch add 5 + delete 3 + rename 3; old delete/rename markers are first proven indexed; dirty covers categories; update; add target 0 found at path+marker; rename-new target 0 found at path+marker; deleted path 0 no VerifiedCurrent; rename-old path 0 no VerifiedCurrent; clean + valid + citations invalid_count=0.

G. **latency_compare**: Twin repo copies per iteration; same mutation applied; compare `update --dirty` vs `build` for modify_one and batch. Gate: p50 update < p50 rebuild. If false, report only; does not cause exit failure.

H. **growth_cycles**: N cycles of modify same file + update. Each cycle: dirty detects modified, update succeeds, dirty clean, validate valid. Catastrophic guard: `final_after_updates_size <= max(3 * post_full_rebuild_size, post_full_rebuild_size + 64MiB)`. Observed growth ratio reported. This is a catastrophic guard, not proof of long-term bounded growth.

### R12 results (after per-run marker + safety gate fixes)

| Check | Result |
|---|---|
| Baseline markers absent pre-build (no self-contamination) | ✅ |
| Baseline build succeeds, file/chunk count >0 | ✅ |
| Baseline dirty clean after build | ✅ |
| Baseline validate valid | ✅ |
| modify_one: dirty detects modified | ✅ |
| modify_one: update succeeds | ✅ |
| modify_one: new marker found at path+marker | ✅ |
| modify_one: old marker gone (no VerifiedCurrent for path) | ✅ |
| modify_one: clean + valid after update | ✅ |
| add_one: dirty detects added | ✅ |
| add_one: search finds marker at path+marker | ✅ |
| add_one: clean + valid after update | ✅ |
| delete_one: dirty detects deleted | ✅ |
| delete_one: no VerifiedCurrent for deleted path | ✅ |
| delete_one: clean + valid after update | ✅ |
| rename_one: dirty detects added + deleted | ✅ |
| rename_one: old gone, new found at path+marker | ✅ |
| rename_one: clean + valid after update | ✅ |
| policy_exclude: dirty stays clean | ✅ |
| policy_exclude: no evidence from excluded path | ✅ |
| branch_batch: add target 0 found at path+marker | ✅ |
| branch_batch: rename-new target 0 found at path+marker | ✅ |
| branch_batch: deleted/rename-old markers indexed before removal | ✅ |
| branch_batch: deleted path 0 no VerifiedCurrent | ✅ |
| branch_batch: rename-old path 0 no VerifiedCurrent | ✅ |
| branch_batch: clean + valid after update | ✅ |
| growth_cycles: all cycles dirty→update→clean→valid | ✅ |
| growth_cycles: catastrophic guard passed | ✅ |
| total_invalid_citations | 0 |
| stale_verified_current_violations | [] |

### Latency compare (twin repo copies, same mutation)

| Scenario | Update p50 (ms) | Rebuild p50 (ms) | Update faster? |
|---|---|---|---|
| modify_one | 96.2 | 165.8 | ✅ yes |
| branch_like_batch | 99.7 | 169.4 | ✅ yes |

Incremental update is ~42% faster than full rebuild for both single-file and batch modifications on this real-repo sample (75 files, 804 chunks, dev profile). Latency gate is report-only; false does not cause exit failure.

### Growth result

| Metric | Value |
|---|---|
| Size after 20 update cycles | ~905 KB |
| Size after full rebuild | ~823 KB |
| Observed growth ratio | 1.11 |
| Catastrophic guard (≤ max(3×rebuild, rebuild+64MiB)) | ✅ passed |

20 cycles observed growth ~1.11×; catastrophic guard passed. Does not prove long-term bounded growth.

### Safety checks

149/149 hard safety checks passed. Script exits with code 1 if any hard safety check fails; latency/growth gate failures are report-only.

### Key findings

1. **Real-repo incremental update passes this Level0 sample**: On a temp copy of the OpenLocus repository (mixed Rust, Python, TypeScript, Markdown files), incremental update correctly handles sampled modify, add, delete, rename, policy-excluded, and batch workloads. No stale VerifiedCurrent evidence is produced.
2. **Citation validity is maintained for collected marker-search evidence**: Evidence validated through `openlocus citations validate` has `invalid_count=0` and validator returncode==0.
3. **Per-run unique markers avoid self-contamination**: Fixed markers appeared in copied docs/scripts, causing false positives. Per-run suffixes (8-hex chars) and pre-build assert prevent this.
4. **Positive gates use path+marker conjunction**: Previous `evidence_has_path_or_marker` (disjunction) could pass from unrelated evidence. New `evidence_has_path_and_marker` requires both path and marker in the cited excerpt of the same evidence item.
5. **Empty evidence is not a pass for positive gates**: Previous code could pass add/rename checks with empty evidence returning `invalid_citations=0`. Now requires `len(evidence) > 0`.
6. **Latency comparison uses twin repo copies**: Previous code compared update on a dirty repo vs rebuild on a clean repo (unfair). Now both start from the same state with the same mutation applied.
7. **Growth gate is honestly named as catastrophic guard**: 20 cycles observed growth ~1.11×; catastrophic guard passed. This does not prove long-term bounded growth.
8. **This is one real-repo sample (OpenLocus temp copy)**: Not a general performance claim. Different repos, hardware, and workloads may produce different results.

### Caveats

- This is a Level0 real-repo sample benchmark using one repo (OpenLocus temp copy). Not a general performance or robustness claim.
- Per-run unique markers avoid self-contamination but are not representative of real search queries.
- The temp repo excludes `.git` history; real repos with large git histories may behave differently in scan time.
- Tantivy deletes are tombstones until merge; index size after many updates may be larger than a fresh rebuild. The catastrophic guard bounds extreme growth but does not prove bounded growth.
- The latency comparison uses CLI wall-clock time which includes process startup overhead. Internal Rust timing would be more precise.
- No concurrent access testing; single-process only.
- No crash/recovery testing between Tantivy commit and manifest write.
- Branch-like batch does not actually switch git branches; it simulates the file-level effects of a branch switch.
- Collected marker-search evidence is validated; this is not "all evidence in repo" — only evidence from workload-specific marker searches.
