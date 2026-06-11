use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::{Parser, Subcommand};
use openlocus_ast::{AstSymbolKind, AstSymbolStatus, extract_ast_symbols};
use openlocus_context::plan::{FastContextPlan, fast_context};
use openlocus_core::{
    BudgetUsed, Channel, ContextLitePack, Evidence, EvidencePack, Freshness, JsonOutput, Policy,
    ScoreParts, Symbol, SymbolKind, TraceEvent, append_trace,
};
use openlocus_derived::generator;
use openlocus_derived::model::{DerivedIndexView, DerivedViewKind};
use openlocus_derived::store::JsonlDerivedViewStore;
use openlocus_derived::validation;
use openlocus_graph::graph::{self, EdgeKind, GraphEdge};
use openlocus_graph::materialize::materialize_graph_edges;
use openlocus_index::manifest::{ChunkStrategy, IndexManifest};
use openlocus_index::persistent::{
    PersistentBm25Index, build_index, dirty_index, purge_index, search_persistent_bm25,
    status_index, update_index, validate_index,
};
use openlocus_provider::audit;
use openlocus_provider::dense_store::JsonlEmbeddingStore;
use openlocus_provider::provider::{self, EmbeddingProvider};
use openlocus_repo::read::read_file;
use openlocus_repo::scan::scan_repo;
use openlocus_repo::validate_path;
use openlocus_retrieval::bm25_search::bm25_search;
use openlocus_retrieval::regex_search::{regex_search, text_search};
use openlocus_retrieval::rrf::rrf_combine;
use openlocus_retrieval::symbol_search::symbol_search;
use openlocus_store::StoreBackend;
use openlocus_store::conservative::ConservativeChunkStore;
use openlocus_store::tdb_placeholder::TdbPlaceholderStore;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "openlocus",
    version,
    about = "Code fact retrieval kernel for coding agents"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Read a file or line range, returning Evidence
    Read {
        /// Path spec: e.g. README.md or src/main.rs:10-20
        path_spec: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Scan repo for file records
    Scan {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Search code
    Search {
        #[command(subcommand)]
        search_cmd: SearchCommands,
    },
    /// RRF multi-channel retrieve
    Retrieve {
        /// Query
        query: String,
        /// Comma-separated channels (regex,bm25,symbol)
        #[arg(long, default_value = "regex,bm25,symbol")]
        channels: String,
        /// Maximum results
        #[arg(long, default_value_t = 20)]
        max_results: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// 4-turn deterministic fast-context loop
    FastContext {
        /// Query
        query: String,
        /// Approximate token budget cap (0 = no cap)
        #[arg(long, default_value_t = 0)]
        budget: usize,
        /// Maximum evidence count
        #[arg(long, default_value_t = 20)]
        max_evidence: usize,
        /// Comma-separated channels (regex,bm25,symbol,graph)
        #[arg(long, default_value = "regex,bm25,symbol,graph")]
        channels: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Validate citations
    Citations {
        #[command(subcommand)]
        citations_cmd: CitationsCommands,
    },
    /// Generate context-lite pack
    ContextLite {
        /// Write context files to .openlocus/context/
        #[arg(long)]
        write_files: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Store operations (build, status, purge)
    Store {
        #[command(subcommand)]
        store_cmd: StoreCommands,
    },
    /// Derived index operations (experimental)
    Derived {
        #[command(subcommand)]
        derived_cmd: DerivedCommands,
    },
    /// Graph operations (build, inspect)
    Graph {
        #[command(subcommand)]
        graph_cmd: GraphCommands,
    },
    /// Persistent index operations (build, status, validate, purge)
    Index {
        #[command(subcommand)]
        index_cmd: IndexCommands,
    },
    /// Benchmark operations
    Bench {
        #[command(subcommand)]
        bench_cmd: BenchCommands,
    },
    /// Impact analysis: files that depend on or test a given path
    Impact {
        /// Path spec: e.g. src/lib.rs or src/lib.rs:10
        path_spec: String,
        /// Traversal depth (only 1 supported in Level0)
        #[arg(long, default_value_t = 1)]
        depth: u8,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Select test files relevant to a path
    Tests {
        /// Filter by source path
        #[arg(long)]
        path: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Print version
    Version,
    /// Provider status and audit (experimental)
    Provider {
        #[command(subcommand)]
        provider_cmd: ProviderCommands,
    },
    /// Dense embedding search (experimental)
    Dense {
        #[command(subcommand)]
        dense_cmd: DenseCommands,
    },
}

#[derive(Subcommand)]
pub enum SearchCommands {
    /// Search with a regex pattern
    Regex {
        /// Regex pattern
        pattern: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Search with plain text query
    Text {
        /// Text query
        query: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Search with BM25
    Bm25 {
        /// Query
        query: String,
        /// Maximum results
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Index mode: temp (build per-query) or persistent (use pre-built index)
        #[arg(long, default_value = "temp")]
        index: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Search for symbol definitions
    Symbol {
        /// Symbol name query
        query: String,
        /// Maximum results
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Search mode: regex, ast, or auto (ast first for supported files, regex fallback)
        #[arg(long, default_value = "auto")]
        mode: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum CitationsCommands {
    /// Validate a JSON file of citations
    Validate {
        /// Path to JSON file containing Evidence array or object with evidence field
        json_file: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum StoreCommands {
    /// Show store backend status
    Status {
        /// Backend name: conservative or tdb
        #[arg(default_value = "conservative")]
        backend: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Build store from scanned files
    Build {
        /// Backend name: conservative or tdb
        #[arg(default_value = "conservative")]
        backend: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Purge store data
    Purge {
        /// Backend name: conservative or tdb
        #[arg(default_value = "conservative")]
        backend: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum DerivedCommands {
    /// Build derived index views
    Build {
        /// Kind of views to build: chunk-summary, symbol-tags, query-aliases, or all
        #[arg(default_value = "all")]
        kind: String,
        /// Must be set to enable experimental derived indexing
        #[arg(long)]
        experimental: bool,
        /// Write derived views to .openlocus/derived/
        #[arg(long)]
        write_files: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Maximum data level allowed (default 1)
        #[arg(long, default_value_t = 1)]
        max_data_level: u8,
    },
    /// Validate stored derived views
    Validate {
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Maximum data level allowed (default 1)
        #[arg(long, default_value_t = 1)]
        max_data_level: u8,
    },
    /// Inspect stored derived views
    Inspect {
        /// Filter by kind
        #[arg(long)]
        kind: Option<String>,
        /// Maximum number of views to show
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Purge all stored derived views
    Purge {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum GraphCommands {
    /// Build graph from repo files
    Build {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Inspect graph edges
    Inspect {
        /// Filter by edge kind: imports, tests, configures
        #[arg(long)]
        kind: Option<String>,
        /// Maximum number of edges to show
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum IndexCommands {
    /// Build persistent BM25 index from scanned files
    Build {
        /// Chunk strategy: line (fixed-size line windows) or ast (AST-bounded, experimental)
        #[arg(long, default_value = "line")]
        chunk_strategy: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show persistent index status
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show dirty summary (manifest-vs-current scan)
    Dirty {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Validate persistent index against filesystem
    Validate {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Incrementally update persistent index
    Update {
        /// Update all dirty files (added/modified/deleted)
        #[arg(long)]
        dirty: bool,
        /// Update a single file path
        #[arg(long)]
        path: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Purge persistent index artifacts
    Purge {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum BenchCommands {
    /// Warm SLO benchmark: open persistent index once, loop queries
    Warm {
        /// Path to dataset JSONL file (fixtures/r2.jsonl format)
        #[arg(long, default_value = "fixtures/r2.jsonl")]
        dataset: String,
        /// Number of iterations
        #[arg(long, default_value_t = 3)]
        iterations: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum ProviderCommands {
    /// Show provider status
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show embedding audit log
    Audit {
        /// Maximum events to show
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum DenseCommands {
    /// Build dense embedding index
    Build {
        /// Provider to use (only "mock" supported in R13)
        #[arg(long, default_value = "mock")]
        provider: String,
        /// Must be set to enable experimental dense indexing
        #[arg(long)]
        experimental: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Search with dense embeddings
    Search {
        /// Query text
        query: String,
        /// Provider to use (only "mock" supported in R13)
        #[arg(long, default_value = "mock")]
        provider: String,
        /// Maximum results
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Purge dense embedding index
    Purge {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct CitationValidationResult {
    valid: Vec<Evidence>,
    invalid: Vec<InvalidCitation>,
    total: usize,
    valid_count: usize,
    invalid_count: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct InvalidCitation {
    evidence: Evidence,
    reason: String,
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let repo_root = discover_repo_root()?;

    let policy = Policy::load_from_repo(&repo_root);

    match cli.command {
        Commands::Read { path_spec, json } => {
            let evidence = read_file(&repo_root, &path_spec)?;
            trace_event(
                &repo_root,
                "read",
                serde_json::json!({"path_spec": path_spec}),
                serde_json::json!({"path": evidence.core.path}),
            );
            print_output(&evidence, json)
        }
        Commands::Scan { json } => {
            let records = scan_repo(&repo_root, &policy)?;
            trace_event(
                &repo_root,
                "scan",
                serde_json::json!({}),
                serde_json::json!({"file_count": records.len()}),
            );
            print_output(&records, json)
        }
        Commands::Search { search_cmd } => match search_cmd {
            SearchCommands::Regex { pattern, json } => {
                let records = scan_repo(&repo_root, &policy)?;
                let results = regex_search(&repo_root, &records, &pattern, 100)?;
                trace_event(
                    &repo_root,
                    "search_regex",
                    serde_json::json!({"pattern": pattern}),
                    serde_json::json!({"result_count": results.len()}),
                );
                print_output(&results, json)
            }
            SearchCommands::Text { query, json } => {
                let records = scan_repo(&repo_root, &policy)?;
                let results = text_search(&repo_root, &records, &query, 100)?;
                trace_event(
                    &repo_root,
                    "search_text",
                    serde_json::json!({"query": query}),
                    serde_json::json!({"result_count": results.len()}),
                );
                print_output(&results, json)
            }
            SearchCommands::Bm25 {
                query,
                limit,
                index,
                json,
            } => {
                if index == "persistent" {
                    let (results, stats) =
                        search_persistent_bm25(&repo_root, &query, limit, &policy)?;
                    trace_event(
                        &repo_root,
                        "search_bm25_persistent",
                        serde_json::json!({"query": query, "limit": limit}),
                        serde_json::json!({"result_count": results.len(), "stale_hits_skipped": stats.stale_hits_skipped, "invalid_hits_skipped": stats.invalid_hits_skipped}),
                    );
                    let output = serde_json::json!({
                        "evidence": results,
                        "stats": stats,
                    });
                    print_output(&output, json)
                } else {
                    // Default: temp index (per-query build)
                    let records = scan_repo(&repo_root, &policy)?;
                    let results = bm25_search(&repo_root, &records, &query, limit)?;
                    trace_event(
                        &repo_root,
                        "search_bm25",
                        serde_json::json!({"query": query, "limit": limit}),
                        serde_json::json!({"result_count": results.len()}),
                    );
                    print_output(&results, json)
                }
            }
            SearchCommands::Symbol {
                query,
                limit,
                mode,
                json,
            } => {
                match mode.as_str() {
                    "regex" => {
                        let records = scan_repo(&repo_root, &policy)?;
                        let results = symbol_search(&repo_root, &records, &query, limit)?;
                        trace_event(
                            &repo_root,
                            "search_symbol_regex",
                            serde_json::json!({"query": query, "limit": limit, "mode": "regex"}),
                            serde_json::json!({"result_count": results.len()}),
                        );
                        print_output(&results, json)
                    }
                    "ast" => {
                        let records = scan_repo(&repo_root, &policy)?;
                        let results = ast_symbol_search(&repo_root, &records, &query, limit)?;
                        trace_event(
                            &repo_root,
                            "search_symbol_ast",
                            serde_json::json!({"query": query, "limit": limit, "mode": "ast"}),
                            serde_json::json!({"result_count": results.len()}),
                        );
                        print_output(&results, json)
                    }
                    _ => {
                        // "auto": AST first, regex fallback
                        let records = scan_repo(&repo_root, &policy)?;
                        let ast_results = ast_symbol_search(&repo_root, &records, &query, limit)?;
                        if !ast_results.is_empty() {
                            trace_event(
                                &repo_root,
                                "search_symbol_auto",
                                serde_json::json!({"query": query, "limit": limit, "mode": "auto", "used": "ast"}),
                                serde_json::json!({"result_count": ast_results.len()}),
                            );
                            print_output(&ast_results, json)
                        } else {
                            let results = symbol_search(&repo_root, &records, &query, limit)?;
                            trace_event(
                                &repo_root,
                                "search_symbol_auto",
                                serde_json::json!({"query": query, "limit": limit, "mode": "auto", "used": "regex"}),
                                serde_json::json!({"result_count": results.len()}),
                            );
                            print_output(&results, json)
                        }
                    }
                }
            }
        },
        Commands::Retrieve {
            query,
            channels,
            max_results,
            json,
        } => {
            let start = std::time::Instant::now();
            let records = scan_repo(&repo_root, &policy)?;

            let channel_list: Vec<String> =
                channels.split(',').map(|s| s.trim().to_string()).collect();

            let mut channel_evidence: Vec<(Vec<Evidence>, Channel)> = Vec::new();

            if channel_list.iter().any(|c| c == "regex") {
                let ev = regex_search(&repo_root, &records, &query, max_results)?;
                channel_evidence.push((ev, Channel::Regex));
            }
            if channel_list.iter().any(|c| c == "bm25") {
                let ev = bm25_search(&repo_root, &records, &query, max_results)?;
                channel_evidence.push((ev, Channel::Bm25));
            }
            if channel_list.iter().any(|c| c == "symbol") {
                let ev = symbol_search(&repo_root, &records, &query, max_results)?;
                channel_evidence.push((ev, Channel::Regex)); // symbol uses Regex channel
            }

            let fused = rrf_combine(channel_evidence);
            let top: Vec<Evidence> = fused.into_iter().take(max_results).collect();

            let latency_ms = start.elapsed().as_millis() as u64;

            let trace_id = format!("tr-{}", Utc::now().timestamp_millis());
            let pack = EvidencePack {
                task: query.clone(),
                intent: "implementation_search".into(),
                confidence: if top.is_empty() {
                    0.0
                } else {
                    top[0].core.score
                },
                evidence: top,
                entrypoints: vec![],
                related_tests: vec![],
                risks: vec![],
                missing_questions: vec![],
                trace_id: trace_id.clone(),
                budget_used: BudgetUsed {
                    latency_ms,
                    tokens_estimated: 0,
                    remote_cost_estimated: 0.0,
                },
            };

            trace_event(
                &repo_root,
                "retrieve",
                serde_json::json!({"query": query, "channels": channels, "max_results": max_results}),
                serde_json::json!({"result_count": pack.evidence.len(), "latency_ms": latency_ms}),
            );

            print_output(&pack, json)
        }
        Commands::FastContext {
            query,
            budget,
            max_evidence,
            channels,
            json,
        } => {
            let records = scan_repo(&repo_root, &policy)?;

            let channel_list: Vec<String> =
                channels.split(',').map(|s| s.trim().to_string()).collect();

            let plan = FastContextPlan {
                query: query.clone(),
                channels: channel_list,
                max_evidence,
                budget,
            };

            let result = match fast_context(&repo_root, &records, &plan) {
                Ok(r) => r,
                Err(e) => {
                    // Unknown channels or other plan errors
                    let err_output = serde_json::json!({
                        "success": false,
                        "error": e.to_string(),
                        "query": query,
                    });
                    if json {
                        println!("{}", serde_json::to_string_pretty(&err_output).unwrap());
                    } else {
                        eprintln!("error: {e}");
                    }
                    return Ok(());
                }
            };

            // Write trace file
            let trace_dir = repo_root.join(".openlocus/traces");
            let _ = std::fs::create_dir_all(&trace_dir);
            let trace_file = trace_dir.join(format!("fast-context-{}.json", result.trace_id));
            let trace_data = serde_json::json!({
                "trace_id": result.trace_id,
                "query": result.query,
                "actions": result.actions,
                "diagnostics": result.diagnostics,
            });
            let _ = std::fs::write(
                &trace_file,
                serde_json::to_string_pretty(&trace_data).unwrap(),
            );

            trace_event(
                &repo_root,
                "fast_context",
                serde_json::json!({"query": query, "channels": channels, "budget": budget, "max_evidence": max_evidence}),
                serde_json::json!({
                    "success": result.success,
                    "evidence_count": result.evidence.len(),
                    "confidence": result.confidence,
                    "remote_calls": result.remote_calls,
                    "turns": result.turns.len(),
                    "disabled_channels": result.disabled_channels,
                    "invalid_citations_dropped": result.diagnostics.invalid_citations_dropped,
                }),
            );

            let output = serde_json::json!({
                "success": result.success,
                "query": result.query,
                "trace_id": result.trace_id,
                "turns": result.turns,
                "actions": result.actions,
                "evidence": result.evidence,
                "pack": result.pack,
                "confidence": result.confidence,
                "missing_questions": result.missing_questions,
                "disabled_channels": result.disabled_channels,
                "remote_calls": result.remote_calls,
                "budget_used": result.budget_used,
                "diagnostics": result.diagnostics,
            });
            print_output(&output, json)
        }
        Commands::Citations { citations_cmd } => match citations_cmd {
            CitationsCommands::Validate { json_file, json } => {
                let result = validate_citations(&repo_root, &json_file)?;
                trace_event(
                    &repo_root,
                    "citations_validate",
                    serde_json::json!({"file": json_file}),
                    serde_json::json!({
                        "total": result.total,
                        "valid_count": result.valid_count,
                        "invalid_count": result.invalid_count
                    }),
                );
                print_output(&result, json)
            }
        },
        Commands::ContextLite { write_files, json } => {
            let pack = build_context_lite(&repo_root, write_files)?;
            trace_event(
                &repo_root,
                "context_lite",
                serde_json::json!({"write_files": write_files}),
                serde_json::json!({"generated_files": pack.generated_files}),
            );
            print_output(&pack, json)
        }
        Commands::Store { store_cmd } => match store_cmd {
            StoreCommands::Status { backend, json } => {
                let result = store_status(&repo_root, &policy, &backend)?;
                trace_event(
                    &repo_root,
                    "store_status",
                    serde_json::json!({"backend": backend}),
                    serde_json::json!({"available": result.available}),
                );
                print_output(&result, json)
            }
            StoreCommands::Build { backend, json } => {
                let result = store_build(&repo_root, &policy, &backend)?;
                trace_event(
                    &repo_root,
                    "store_build",
                    serde_json::json!({"backend": backend}),
                    serde_json::json!({"chunk_count": result.chunk_count, "file_count": result.file_count}),
                );
                print_output(&result, json)
            }
            StoreCommands::Purge { backend, json } => {
                let result = store_purge(&backend)?;
                trace_event(
                    &repo_root,
                    "store_purge",
                    serde_json::json!({"backend": backend}),
                    serde_json::json!({"purged": true}),
                );
                print_output(&result, json)
            }
        },
        Commands::Derived { derived_cmd } => match derived_cmd {
            DerivedCommands::Build {
                kind,
                experimental,
                write_files,
                json,
                max_data_level,
            } => {
                let result = derived_build(
                    &repo_root,
                    &policy,
                    &kind,
                    experimental,
                    write_files,
                    max_data_level,
                )?;
                trace_event(
                    &repo_root,
                    "derived_build",
                    serde_json::json!({"kind": kind, "experimental": experimental, "max_data_level": max_data_level}),
                    serde_json::json!({"generated": result.generated, "valid": result.valid, "blocked": result.blocked_kind}),
                );
                print_output(&result, json)
            }
            DerivedCommands::Validate {
                json,
                max_data_level,
            } => {
                let result = derived_validate(&repo_root, max_data_level)?;
                trace_event(
                    &repo_root,
                    "derived_validate",
                    serde_json::json!({"max_data_level": max_data_level}),
                    serde_json::json!({"valid": result.valid, "stale": result.stale}),
                );
                print_output(&result, json)
            }
            DerivedCommands::Inspect { kind, limit, json } => {
                let result = derived_inspect(&repo_root, kind.as_deref(), limit)?;
                trace_event(
                    &repo_root,
                    "derived_inspect",
                    serde_json::json!({"kind": kind, "limit": limit}),
                    serde_json::json!({"count": result.views.len()}),
                );
                print_output(&result, json)
            }
            DerivedCommands::Purge { json } => {
                let result = derived_purge(&repo_root)?;
                trace_event(
                    &repo_root,
                    "derived_purge",
                    serde_json::json!({}),
                    serde_json::json!({"purged": result.purged}),
                );
                print_output(&result, json)
            }
        },
        Commands::Version => {
            println!("openlocus {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Commands::Graph { graph_cmd } => match graph_cmd {
            GraphCommands::Build { json } => {
                let records = scan_repo(&repo_root, &policy)?;
                let (nodes, edges, result) = graph::build_graph(&repo_root, &records)?;
                trace_event(
                    &repo_root,
                    "graph_build",
                    serde_json::json!({}),
                    serde_json::json!({"node_count": result.node_count, "edge_count": result.edge_count}),
                );
                let output = serde_json::json!({
                    "success": true,
                    "node_count": result.node_count,
                    "edge_count": result.edge_count,
                    "edges_by_kind": result.edges_by_kind,
                    "skipped_stale": result.skipped_stale,
                    "skipped_path_unsafe": result.skipped_path_unsafe,
                });
                let _ = (nodes, edges); // used by inspect/impact via re-scan
                print_output(&output, json)
            }
            GraphCommands::Inspect { kind, limit, json } => {
                let records = scan_repo(&repo_root, &policy)?;
                let (_nodes, edges, _result) = graph::build_graph(&repo_root, &records)?;

                let filtered: Vec<&GraphEdge> = if let Some(k) = &kind {
                    let target = match k.as_str() {
                        "imports" => Some(EdgeKind::Imports),
                        "tests" => Some(EdgeKind::Tests),
                        "configures" => Some(EdgeKind::Configures),
                        _ => None,
                    };
                    edges
                        .iter()
                        .filter(|e| Some(&e.kind) == target.as_ref())
                        .take(limit)
                        .collect()
                } else {
                    edges.iter().take(limit).collect()
                };

                trace_event(
                    &repo_root,
                    "graph_inspect",
                    serde_json::json!({"kind": kind, "limit": limit}),
                    serde_json::json!({"edge_count": filtered.len()}),
                );

                // Wrap with artifact marker so consumers know these are edges, not Evidence
                let output = serde_json::json!({
                    "artifact": "graph_edges_not_evidence",
                    "note": "These are GraphEdge records, not citation-valid Evidence. Use 'impact' or 'tests' commands for materialized Evidence.",
                    "count": filtered.len(),
                    "edges": filtered,
                });
                print_output(&output, json)
            }
        },
        Commands::Impact {
            path_spec,
            depth,
            json,
        } => {
            if depth > 1 {
                let result = serde_json::json!({
                    "success": false,
                    "error": format!("R5 Level0 only supports depth=1; depth={} is not implemented", depth),
                    "depth": depth,
                });
                trace_event(
                    &repo_root,
                    "impact",
                    serde_json::json!({"path_spec": path_spec, "depth": depth}),
                    serde_json::json!({"success": false}),
                );
                return print_output(&result, json);
            }

            // Parse path spec (just the path part, ignoring line numbers for impact)
            let target_path = path_spec
                .split(':')
                .next()
                .unwrap_or(&path_spec)
                .to_string();

            let records = scan_repo(&repo_root, &policy)?;
            let (_nodes, edges, _result) = graph::build_graph(&repo_root, &records)?;

            let impact = graph::impact_edges(&edges, &target_path, depth)?;

            // Materialize evidence from impact edges
            let (evidence, skipped) = materialize_graph_edges(&repo_root, &impact);

            let result = serde_json::json!({
                "success": true,
                "path": target_path,
                "depth": depth,
                "impact_count": impact.len(),
                "evidence_count": evidence.len(),
                "skipped": skipped,
                "evidence": evidence,
            });

            trace_event(
                &repo_root,
                "impact",
                serde_json::json!({"path_spec": path_spec, "depth": depth}),
                serde_json::json!({"impact_count": impact.len(), "evidence_count": evidence.len()}),
            );
            print_output(&result, json)
        }
        Commands::Tests { path, json } => {
            let records = scan_repo(&repo_root, &policy)?;
            let (_nodes, edges, _result) = graph::build_graph(&repo_root, &records)?;

            let test_edges = graph::test_edges(&edges, path.as_deref());

            // Materialize evidence from test source files
            let test_edges_owned: Vec<_> = test_edges.into_iter().cloned().collect();
            let (test_evidence, skipped) = materialize_graph_edges(&repo_root, &test_edges_owned);

            let result = serde_json::json!({
                "success": true,
                "test_count": test_evidence.len(),
                "skipped": skipped,
                "evidence": test_evidence,
            });

            trace_event(
                &repo_root,
                "tests_select",
                serde_json::json!({"path": path}),
                serde_json::json!({"test_count": test_evidence.len(), "skipped": skipped}),
            );
            print_output(&result, json)
        }
        Commands::Index { index_cmd } => match index_cmd {
            IndexCommands::Build {
                chunk_strategy,
                json,
            } => {
                let strategy = ChunkStrategy::from_cli_str(&chunk_strategy)
                    .unwrap_or(ChunkStrategy::LineWindowV1);
                let records = scan_repo(&repo_root, &policy)?;
                let result = build_index(&repo_root, &records, &policy, strategy)?;
                trace_event(
                    &repo_root,
                    "index_build",
                    serde_json::json!({"chunk_strategy": chunk_strategy}),
                    serde_json::json!({"success": result.success, "file_count": result.file_count, "chunk_count": result.chunk_count}),
                );
                print_output(&result, json)
            }
            IndexCommands::Status { json } => {
                let result = status_index(&repo_root, &policy)?;
                trace_event(
                    &repo_root,
                    "index_status",
                    serde_json::json!({}),
                    serde_json::json!({"exists": result.exists, "requires_rebuild": result.requires_rebuild}),
                );
                print_output(&result, json)
            }
            IndexCommands::Dirty { json } => {
                let records = scan_repo(&repo_root, &policy)?;
                let result = dirty_index(&repo_root, &policy, &records)?;
                trace_event(
                    &repo_root,
                    "index_dirty",
                    serde_json::json!({}),
                    serde_json::json!({"clean": result.clean, "requires_update": result.requires_update, "requires_rebuild": result.requires_rebuild}),
                );
                print_output(&result, json)
            }
            IndexCommands::Validate { json } => {
                let result = validate_index(&repo_root, &policy)?;
                trace_event(
                    &repo_root,
                    "index_validate",
                    serde_json::json!({}),
                    serde_json::json!({"valid": result.valid, "stale_files": result.stale_files.len(), "deleted_files": result.deleted_files.len()}),
                );
                print_output(&result, json)
            }
            IndexCommands::Update { dirty, path, json } => {
                let records = scan_repo(&repo_root, &policy)?;
                let result = update_index(&repo_root, &policy, &records, dirty, path.as_deref())?;
                trace_event(
                    &repo_root,
                    "index_update",
                    serde_json::json!({"dirty": dirty, "path": path}),
                    serde_json::json!({"success": result.success, "added_count": result.added_count, "modified_count": result.modified_count, "deleted_count": result.deleted_count}),
                );
                print_output(&result, json)
            }
            IndexCommands::Purge { json } => {
                let result = purge_index(&repo_root)?;
                trace_event(
                    &repo_root,
                    "index_purge",
                    serde_json::json!({}),
                    serde_json::json!({"purged": result.purged}),
                );
                print_output(&result, json)
            }
        },
        Commands::Bench { bench_cmd } => match bench_cmd {
            BenchCommands::Warm {
                dataset,
                iterations,
                json,
            } => {
                let result = run_bench_warm(&repo_root, &policy, &dataset, iterations)?;
                trace_event(
                    &repo_root,
                    "bench_warm",
                    serde_json::json!({"dataset": dataset, "iterations": iterations}),
                    serde_json::json!({
                        "index_open_ms": result.index_open_ms,
                        "warm_query_p50_ms": result.warm_query_p50_ms,
                        "warm_query_p95_ms": result.warm_query_p95_ms,
                    }),
                );
                print_output(&result, json)
            }
        },
        Commands::Provider { provider_cmd } => match provider_cmd {
            ProviderCommands::Status { json } => {
                let result = provider_status(&repo_root);
                trace_event(
                    &repo_root,
                    "provider_status",
                    serde_json::json!({}),
                    serde_json::json!({"remote_default": result.remote_default, "outbound_default": result.outbound_default}),
                );
                print_output(&result, json)
            }
            ProviderCommands::Audit { limit, json } => {
                let result = provider_audit(&repo_root, limit)?;
                trace_event(
                    &repo_root,
                    "provider_audit",
                    serde_json::json!({"limit": limit}),
                    serde_json::json!({"event_count": result.events.len()}),
                );
                print_output(&result, json)
            }
        },
        Commands::Dense { dense_cmd } => match dense_cmd {
            DenseCommands::Build {
                provider: provider_name,
                experimental,
                json,
            } => {
                let result = dense_build(&repo_root, &policy, &provider_name, experimental)?;
                trace_event(
                    &repo_root,
                    "dense_build",
                    serde_json::json!({"provider": provider_name, "experimental": experimental}),
                    serde_json::json!({"success": result.success, "record_count": result.record_count}),
                );
                print_output(&result, json)
            }
            DenseCommands::Search {
                query,
                provider: provider_name,
                limit,
                json,
            } => {
                let result = dense_search(&repo_root, &policy, &query, &provider_name, limit)?;
                let query_sha = blake3::hash(query.as_bytes()).to_hex().to_string();
                trace_event(
                    &repo_root,
                    "dense_search",
                    serde_json::json!({"query_sha": query_sha, "query_len": query.len(), "provider": provider_name, "limit": limit}),
                    serde_json::json!({"success": result.success, "evidence_count": result.evidence.len(), "skipped_count": result.skipped_count}),
                );
                print_output(&result, json)
            }
            DenseCommands::Purge { json } => {
                let result = dense_purge(&repo_root)?;
                trace_event(
                    &repo_root,
                    "dense_purge",
                    serde_json::json!({}),
                    serde_json::json!({"purged": result.purged}),
                );
                print_output(&result, json)
            }
        },
    }
}

/// Discover repo root by walking up from CWD looking for .git directory.
fn discover_repo_root() -> Result<PathBuf> {
    let mut dir = std::env::current_dir()?;
    loop {
        if dir.join(".git").exists() || dir.join(".openlocus").exists() {
            return Ok(dir);
        }
        if !dir.pop() {
            return Ok(std::env::current_dir()?);
        }
    }
}

fn trace_event(root: &Path, event: &str, input: serde_json::Value, output: serde_json::Value) {
    let ev = TraceEvent::new(event).with_input(input).with_output(output);
    if let Err(e) = append_trace(root, &ev) {
        eprintln!("warning: failed to append trace: {}", e);
    }
}

fn print_output<T: Serialize>(val: &T, _json: bool) -> Result<()> {
    println!("{}", JsonOutput::to_json_pretty(val)?);
    Ok(())
}

/// Validate citations from a JSON file.
/// Accepts three input formats:
/// 1. A single Evidence object (not wrapped in array)
/// 2. An array of Evidence
/// 3. An object with an "evidence" field containing an array
fn validate_citations(repo_root: &Path, json_file: &str) -> Result<CitationValidationResult> {
    let content = fs::read_to_string(json_file)?;
    let trimmed = content.trim_start();

    let evidences: Vec<Evidence> = if trimmed.starts_with('[') {
        serde_json::from_str(&content)?
    } else if trimmed.starts_with('{') {
        let obj: serde_json::Value = serde_json::from_str(&content)?;
        if let Some(arr) = obj.get("evidence") {
            serde_json::from_value(arr.clone())?
        } else if obj.get("path").is_some() || obj.get("content_sha").is_some() {
            let single: Evidence = serde_json::from_value(obj)?;
            vec![single]
        } else {
            bail!("JSON object must be a single Evidence or have an 'evidence' field");
        }
    } else {
        bail!("JSON must start with '{{' or '['");
    };

    let mut valid = Vec::new();
    let mut invalid = Vec::new();

    for ev in evidences {
        match validate_single_citation(repo_root, &ev) {
            Ok(()) => valid.push(ev),
            Err(reason) => invalid.push(InvalidCitation {
                evidence: ev,
                reason: reason.to_string(),
            }),
        }
    }

    let valid_count = valid.len();
    let invalid_count = invalid.len();
    let total = valid_count + invalid_count;

    Ok(CitationValidationResult {
        valid,
        invalid_count,
        valid_count,
        invalid,
        total,
    })
}

fn validate_single_citation(repo_root: &Path, evidence: &Evidence) -> Result<()> {
    if evidence.core.start_line == 0 {
        bail!("start_line must be >= 1, got 0");
    }
    if evidence.core.start_line > evidence.core.end_line {
        bail!(
            "start_line ({}) > end_line ({})",
            evidence.core.start_line,
            evidence.core.end_line
        );
    }

    let full_path = validate_path(repo_root, &evidence.core.path)?;

    if !full_path.exists() {
        bail!("path does not exist: {}", evidence.core.path);
    }

    if !full_path.is_file() {
        bail!("not a file: {}", evidence.core.path);
    }

    let current_sha = openlocus_repo::read::compute_content_sha(&full_path)?;
    if current_sha != evidence.core.content_sha {
        bail!(
            "content_sha mismatch: expected {}, got {}",
            evidence.core.content_sha,
            current_sha
        );
    }

    let content = std::fs::read_to_string(&full_path)
        .with_context(|| format!("failed to read {}", evidence.core.path))?;
    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len() as u64;

    if evidence.core.end_line > total_lines {
        bail!(
            "end_line ({}) exceeds file line count ({})",
            evidence.core.end_line,
            total_lines
        );
    }

    if let Some(ref meta) = evidence.meta
        && let Some(ref excerpt) = meta.excerpt
    {
        let start_idx = (evidence.core.start_line - 1) as usize;
        let end_idx = evidence.core.end_line as usize;
        let actual_excerpt = lines[start_idx..end_idx].join("\n");
        if excerpt != &actual_excerpt {
            bail!(
                "excerpt mismatch for {}:{}-{}: excerpt content does not match current file",
                evidence.core.path,
                evidence.core.start_line,
                evidence.core.end_line
            );
        }
    }

    Ok(())
}

/// Build a context-lite pack.
fn build_context_lite(repo_root: &Path, write_files: bool) -> Result<ContextLitePack> {
    let trace_id = format!("ctx-{}", Utc::now().timestamp_millis());
    let mut generated_files = Vec::new();

    if write_files {
        let ctx_dir = repo_root.join(".openlocus").join("context");
        fs::create_dir_all(&ctx_dir)?;

        let dirty_summary_path = ctx_dir.join("dirty-summary.json");
        // R10: populate dirty summary with actual index status
        let dirty_data = if IndexManifest::exists(repo_root) {
            let policy = Policy::load_from_repo(repo_root);
            let records = scan_repo(repo_root, &policy)?;
            let dirty_result = dirty_index(repo_root, &policy, &records)?;
            serde_json::json!({
                "repo_root": repo_root.to_string_lossy(),
                "timestamp": Utc::now().to_rfc3339(),
                "clean": dirty_result.clean,
                "requires_update": dirty_result.requires_update,
                "requires_rebuild": dirty_result.requires_rebuild,
                "added_count": dirty_result.added_count,
                "modified_count": dirty_result.modified_count,
                "deleted_count": dirty_result.deleted_count,
                "added_files": dirty_result.added_files,
                "modified_files": dirty_result.modified_files,
                "deleted_files": dirty_result.deleted_files,
                "policy_hash_matches": dirty_result.policy_hash_matches,
                "schema_matches": dirty_result.schema_matches,
                "chunk_strategy": dirty_result.chunk_strategy,
            })
        } else {
            serde_json::json!({
                "repo_root": repo_root.to_string_lossy(),
                "timestamp": Utc::now().to_rfc3339(),
                "clean": false,
                "requires_update": false,
                "requires_rebuild": true,
                "added_count": 0,
                "modified_count": 0,
                "deleted_count": 0,
                "added_files": [],
                "modified_files": [],
                "deleted_files": [],
                "policy_hash_matches": false,
                "schema_matches": false,
                "chunk_strategy": null,
            })
        };
        fs::write(
            &dirty_summary_path,
            serde_json::to_string_pretty(&dirty_data)?,
        )?;
        generated_files.push(".openlocus/context/dirty-summary.json".into());

        let retrieval_path = ctx_dir.join("retrieval-latest.jsonl");
        fs::write(&retrieval_path, "")?;
        generated_files.push(".openlocus/context/retrieval-latest.jsonl".into());
    }

    Ok(ContextLitePack {
        session_id: None,
        generated_files,
        diagnostics: None,
        dirty_summary: None,
        recent_reads: None,
        recent_edits: None,
        test_outputs: None,
        trace_id,
    })
}

// ── Store helpers ────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct StoreStatusResult {
    backend: String,
    available: bool,
    mode: String,
    persistent: bool,
    success: bool,
    capabilities: openlocus_store::StoreCapabilities,
    #[serde(skip_serializing_if = "Option::is_none")]
    snapshot_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

// ── AST symbol search helper ──────────────────────────────────────────

/// AST-based symbol search. Uses Tree-sitter to extract symbols from
/// supported language files. Returns narrow header/signature Evidence.
/// For unsupported languages or parse errors, returns empty (callers
/// should fall back to regex).
fn ast_symbol_search(
    repo_root: &Path,
    records: &[openlocus_repo::scan::FileRecord],
    query: &str,
    max_results: usize,
) -> Result<Vec<Evidence>> {
    let mut results = Vec::new();

    for record in records {
        if results.len() >= max_results {
            break;
        }

        let full_path = match validate_path(repo_root, &record.path) {
            Ok(path) => path,
            Err(_) => continue,
        };
        let content = match std::fs::read_to_string(&full_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let content_sha = blake3::hash(content.as_bytes()).to_hex().to_string();
        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len() as u64;

        let ast_result = extract_ast_symbols(&record.path, &record.language, &content);

        // Only use results from supported AST parsing
        if ast_result.status != AstSymbolStatus::Supported {
            continue;
        }

        for sym in &ast_result.symbols {
            if results.len() >= max_results {
                break;
            }

            // Match symbol name against query (case-insensitive contains)
            if !sym.name.to_lowercase().contains(&query.to_lowercase()) {
                continue;
            }

            // Validate range
            if sym.start_line < 1 || sym.end_line > total_lines || sym.start_line > sym.end_line {
                continue;
            }

            let excerpt = lines[(sym.start_line - 1) as usize..sym.end_line as usize].join("\n");

            let core_symbol_kind = match sym.kind {
                AstSymbolKind::Function => SymbolKind::Function,
                AstSymbolKind::Method => SymbolKind::Method,
                AstSymbolKind::Class => SymbolKind::Class,
                AstSymbolKind::Interface => SymbolKind::Interface,
                AstSymbolKind::Type => SymbolKind::Type,
                AstSymbolKind::Enum => SymbolKind::Type,
                AstSymbolKind::Trait => SymbolKind::Interface,
                AstSymbolKind::Module => SymbolKind::Module,
                AstSymbolKind::Variable => SymbolKind::Variable,
                AstSymbolKind::Constant => SymbolKind::Variable,
                AstSymbolKind::Macro => SymbolKind::Function,
                AstSymbolKind::Decorator => SymbolKind::Function,
                AstSymbolKind::Unknown => SymbolKind::Unknown,
            };

            let evidence = Evidence::new(
                &record.path,
                sym.start_line,
                sym.end_line,
                &content_sha,
                1.0,
                vec![format!("ast_symbol: {}", sym.name)],
                vec![Channel::TreeSitter],
            )
            .with_excerpt(&excerpt)
            .with_language(&record.language)
            .with_freshness(Freshness::VerifiedCurrent)
            .with_symbol(Symbol {
                name: sym.name.clone(),
                kind: core_symbol_kind,
                qualified_name: None,
                symbol_id: None,
            })
            .with_score_parts(ScoreParts {
                symbol: Some(1.0),
                ..Default::default()
            });

            results.push(evidence);
        }
    }

    Ok(results)
}

fn store_status(_repo_root: &Path, _policy: &Policy, backend: &str) -> Result<StoreStatusResult> {
    match backend {
        "conservative" => {
            let store = ConservativeChunkStore::new();
            let health = store.health();
            Ok(StoreStatusResult {
                backend: store.name().to_string(),
                available: health.available,
                mode: "ephemeral_in_memory".to_string(),
                persistent: false,
                success: true,
                capabilities: health.capabilities,
                snapshot_id: health.snapshot_id,
                error: health.error,
            })
        }
        "tdb" => {
            let store = TdbPlaceholderStore::new();
            let health = store.health();
            Ok(StoreStatusResult {
                backend: store.name().to_string(),
                available: false,
                mode: "placeholder".to_string(),
                persistent: false,
                success: false,
                capabilities: health.capabilities.clone(),
                snapshot_id: health.snapshot_id,
                error: health.error.or_else(|| {
                    Some("TDB backend not available: feature 'tdb' is not enabled".into())
                }),
            })
        }
        _ => Ok(StoreStatusResult {
            backend: backend.to_string(),
            available: false,
            mode: "unknown".to_string(),
            persistent: false,
            success: false,
            capabilities: openlocus_store::StoreCapabilities::none(),
            snapshot_id: None,
            error: Some(format!("unknown backend: {}", backend)),
        }),
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct StoreBuildResult {
    backend: String,
    chunk_count: usize,
    file_count: usize,
    mode: String,
    persistent: bool,
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    snapshot_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn store_build(repo_root: &Path, policy: &Policy, backend: &str) -> Result<StoreBuildResult> {
    match backend {
        "conservative" => {
            let records = scan_repo(repo_root, policy)?;
            let mut store = ConservativeChunkStore::new();
            match store.build(repo_root, &records) {
                Ok(debug) => Ok(StoreBuildResult {
                    backend: debug.backend_name,
                    chunk_count: debug.chunk_count,
                    file_count: debug.file_count,
                    mode: "ephemeral_in_memory".to_string(),
                    persistent: false,
                    success: true,
                    snapshot_id: debug.snapshot_id,
                    error: None,
                }),
                Err(e) => Ok(StoreBuildResult {
                    backend: backend.to_string(),
                    chunk_count: 0,
                    file_count: 0,
                    mode: "ephemeral_in_memory".to_string(),
                    persistent: false,
                    success: false,
                    snapshot_id: None,
                    error: Some(e.to_string()),
                }),
            }
        }
        "tdb" => Ok(StoreBuildResult {
            backend: "tdb".to_string(),
            chunk_count: 0,
            file_count: 0,
            mode: "placeholder".to_string(),
            persistent: false,
            success: false,
            snapshot_id: None,
            error: Some("TDB backend not available: feature 'tdb' is not enabled".to_string()),
        }),
        _ => Ok(StoreBuildResult {
            backend: backend.to_string(),
            chunk_count: 0,
            file_count: 0,
            mode: "unknown".to_string(),
            persistent: false,
            success: false,
            snapshot_id: None,
            error: Some(format!("unknown backend: {}", backend)),
        }),
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct StorePurgeResult {
    backend: String,
    purged: bool,
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn store_purge(backend: &str) -> Result<StorePurgeResult> {
    match backend {
        "conservative" => {
            let mut store = ConservativeChunkStore::new();
            match store.purge() {
                Ok(()) => Ok(StorePurgeResult {
                    backend: store.name().to_string(),
                    purged: true,
                    success: true,
                    error: None,
                }),
                Err(e) => Ok(StorePurgeResult {
                    backend: backend.to_string(),
                    purged: false,
                    success: false,
                    error: Some(e.to_string()),
                }),
            }
        }
        "tdb" => Ok(StorePurgeResult {
            backend: "tdb".to_string(),
            purged: false,
            success: false,
            error: Some("TDB backend not available: feature 'tdb' is not enabled".to_string()),
        }),
        _ => Ok(StorePurgeResult {
            backend: backend.to_string(),
            purged: false,
            success: false,
            error: Some(format!("unknown backend: {}", backend)),
        }),
    }
}

// ── Derived helpers ───────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct DerivedBuildResult {
    success: bool,
    experimental: bool,
    remote_calls: u64,
    generated: usize,
    valid: usize,
    invalid: usize,
    blocked_kind: usize,
    blocked_data_level: usize,
    data_level: u8,
    policy_mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    views_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    audit_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn derived_build(
    repo_root: &Path,
    policy: &Policy,
    kind_str: &str,
    experimental: bool,
    write_files: bool,
    max_data_level: u8,
) -> Result<DerivedBuildResult> {
    if !experimental {
        return Ok(DerivedBuildResult {
            success: false,
            experimental: false,
            remote_calls: 0,
            generated: 0,
            valid: 0,
            invalid: 0,
            blocked_kind: 0,
            blocked_data_level: 0,
            data_level: max_data_level,
            policy_mode: "local_only".to_string(),
            views_path: None,
            audit_path: None,
            error: Some("derived indexing requires --experimental flag to opt in".to_string()),
        });
    }

    // Level0 hard gate: max_data_level > 1 not allowed
    if max_data_level > 1 {
        return Ok(DerivedBuildResult {
            success: false,
            experimental: true,
            remote_calls: 0,
            generated: 0,
            valid: 0,
            invalid: 0,
            blocked_kind: 0,
            blocked_data_level: 1, // signal that data_level was blocked
            data_level: max_data_level,
            policy_mode: "local_only".to_string(),
            views_path: None,
            audit_path: None,
            error: Some(format!(
                "R4 Level0 does not allow --max-data-level > 1 (got {}); snippet output path not available",
                max_data_level
            )),
        });
    }

    // Parse kinds
    let kinds: Vec<DerivedViewKind> = if kind_str == "all" {
        DerivedViewKind::l1_kinds().to_vec()
    } else {
        match DerivedViewKind::from_str_loose(kind_str) {
            Some(k) => vec![k],
            None => {
                return Ok(DerivedBuildResult {
                    success: false,
                    experimental: true,
                    remote_calls: 0,
                    generated: 0,
                    valid: 0,
                    invalid: 0,
                    blocked_kind: 0,
                    blocked_data_level: 0,
                    data_level: max_data_level,
                    policy_mode: "local_only".to_string(),
                    views_path: None,
                    audit_path: None,
                    error: Some(format!("unknown kind: {}", kind_str)),
                });
            }
        }
    };

    let records = scan_repo(repo_root, policy)?;
    let (views, blocked_kind, blocked_data_level) =
        generator::generate_views(repo_root, &records, &kinds, max_data_level)?;

    let generated = views.len();

    // Validate views
    let (valid, stale, bk, bdl, pu, ir) =
        validation::validate_all_views(repo_root, &views, max_data_level);
    let invalid = stale + bk + bdl + pu + ir;
    let blocked_kind = blocked_kind + bk;
    let blocked_data_level = blocked_data_level + bdl;

    let mut views_path = None;
    let mut audit_path = None;

    if write_files {
        let store = JsonlDerivedViewStore::new(repo_root);
        store.upsert(&views)?;
        views_path = Some(store.views_path().to_str().unwrap_or_default().to_string());
        audit_path = Some(store.audit_path().to_str().unwrap_or_default().to_string());
    }

    Ok(DerivedBuildResult {
        success: true,
        experimental: true,
        remote_calls: 0,
        generated,
        valid,
        invalid,
        blocked_kind,
        blocked_data_level,
        data_level: max_data_level,
        policy_mode: "local_only".to_string(),
        views_path,
        audit_path,
        error: None,
    })
}

#[derive(Debug, Serialize, Deserialize)]
struct DerivedValidateResult {
    total: usize,
    valid: usize,
    stale: usize,
    blocked_kind: usize,
    blocked_data_level: usize,
    path_unsafe: usize,
    invalid_range: usize,
    parse_errors: usize,
    data_level: u8,
}

fn derived_validate(repo_root: &Path, max_data_level: u8) -> Result<DerivedValidateResult> {
    let store = JsonlDerivedViewStore::new(repo_root);
    let list_result = store.list_with_errors()?;

    let (valid, stale, blocked_kind, blocked_data_level, path_unsafe, invalid_range) =
        validation::validate_all_views(repo_root, &list_result.views, max_data_level);

    Ok(DerivedValidateResult {
        total: list_result.views.len(),
        valid,
        stale,
        blocked_kind,
        blocked_data_level,
        path_unsafe,
        invalid_range,
        parse_errors: list_result.parse_errors,
        data_level: max_data_level,
    })
}

#[derive(Debug, Serialize, Deserialize)]
struct DerivedInspectResult {
    total: usize,
    views: Vec<DerivedIndexView>,
}

fn derived_inspect(
    repo_root: &Path,
    kind_filter: Option<&str>,
    limit: usize,
) -> Result<DerivedInspectResult> {
    let store = JsonlDerivedViewStore::new(repo_root);
    let mut views = store.list()?;

    if let Some(k) = kind_filter {
        let target = DerivedViewKind::from_str_loose(k);
        views.retain(|v| Some(&v.kind) == target.as_ref());
    }

    views.truncate(limit);

    Ok(DerivedInspectResult {
        total: views.len(),
        views,
    })
}

#[derive(Debug, Serialize, Deserialize)]
struct DerivedPurgeResult {
    purged: bool,
    count: usize,
}

fn derived_purge(repo_root: &Path) -> Result<DerivedPurgeResult> {
    let store = JsonlDerivedViewStore::new(repo_root);
    let count = store.purge()?;
    Ok(DerivedPurgeResult {
        purged: true,
        count,
    })
}

// ── Bench warm ────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct BenchWarmResult {
    success: bool,
    index_build_ms: Option<u64>,
    index_open_ms: u64,
    queries: usize,
    iterations: usize,
    warm_query_p50_ms: u64,
    warm_query_p95_ms: u64,
    warm_query_max_ms: u64,
    invalid_citations: u64,
    stale_hits_skipped: u64,
    notes: Vec<String>,
}

/// Run warm SLO benchmark: build index if needed, then open once and loop queries.
fn run_bench_warm(
    repo_root: &Path,
    policy: &Policy,
    dataset_path: &str,
    iterations: usize,
) -> Result<BenchWarmResult> {
    use std::time::Instant;

    // Build persistent index if it doesn't exist or is stale
    let mut index_build_ms: Option<u64> = None;
    let status = status_index(repo_root, policy)?;
    if !status.exists || status.requires_rebuild {
        let build_start = Instant::now();
        let records = scan_repo(repo_root, policy)?;
        let _build_result = build_index(repo_root, &records, policy, ChunkStrategy::LineWindowV1)?;
        index_build_ms = Some(build_start.elapsed().as_millis() as u64);
    }

    // Open the persistent index once (this is what we're measuring as "warm open")
    let open_start = Instant::now();
    let index_handle = match PersistentBm25Index::open(repo_root, policy) {
        Ok(h) => h,
        Err(e) => {
            return Ok(BenchWarmResult {
                success: false,
                index_build_ms,
                index_open_ms: 0,
                queries: 0,
                iterations,
                warm_query_p50_ms: 0,
                warm_query_p95_ms: 0,
                warm_query_max_ms: 0,
                invalid_citations: 0,
                stale_hits_skipped: 0,
                notes: vec![format!("failed to open index: {}", e)],
            });
        }
    };
    let index_open_ms = open_start.elapsed().as_millis() as u64;

    // Load queries from dataset
    let dataset_content = match fs::read_to_string(dataset_path) {
        Ok(c) => c,
        Err(e) => {
            return Ok(BenchWarmResult {
                success: false,
                index_build_ms,
                index_open_ms,
                queries: 0,
                iterations,
                warm_query_p50_ms: 0,
                warm_query_p95_ms: 0,
                warm_query_max_ms: 0,
                invalid_citations: 0,
                stale_hits_skipped: 0,
                notes: vec![format!("failed to read dataset {}: {}", dataset_path, e)],
            });
        }
    };

    let queries: Vec<String> = dataset_content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .and_then(|v| {
                    v.get("query")
                        .and_then(|q| q.as_str())
                        .map(|s| s.to_string())
                })
        })
        .collect();

    if queries.is_empty() {
        return Ok(BenchWarmResult {
            success: false,
            index_build_ms,
            index_open_ms,
            queries: 0,
            iterations,
            warm_query_p50_ms: 0,
            warm_query_p95_ms: 0,
            warm_query_max_ms: 0,
            invalid_citations: 0,
            stale_hits_skipped: 0,
            notes: vec!["no queries found in dataset".into()],
        });
    }

    // Run warm benchmark: reuse the same index handle for all queries
    let mut all_latencies: Vec<u64> = Vec::new();
    let mut total_stale_skipped: u64 = 0;
    let mut total_invalid_citations: u64 = 0;

    for _iteration in 0..iterations {
        for query in &queries {
            let q_start = Instant::now();
            let (evidence, stats) = index_handle.search(repo_root, query, 10)?;
            let q_ms = q_start.elapsed().as_millis() as u64;
            all_latencies.push(q_ms);
            total_stale_skipped += stats.stale_hits_skipped;

            // Real citation validation: hash/range/excerpt/freshness
            for ev in &evidence {
                // Range check
                if ev.core.start_line < 1 || ev.core.start_line > ev.core.end_line {
                    total_invalid_citations += 1;
                    continue;
                }
                // Content sha check
                let full_path = repo_root.join(&ev.core.path);
                if let Ok(bytes) = std::fs::read(&full_path) {
                    let current_sha = blake3::hash(&bytes).to_hex().to_string();
                    if current_sha != ev.core.content_sha {
                        total_invalid_citations += 1;
                        continue;
                    }
                    // Excerpt check
                    if let Ok(content) = std::str::from_utf8(&bytes) {
                        let lines: Vec<&str> = content.lines().collect();
                        let total_lines = lines.len() as u64;
                        if ev.core.end_line > total_lines {
                            total_invalid_citations += 1;
                            continue;
                        }
                        if let Some(ref meta) = ev.meta
                            && let Some(ref excerpt) = meta.excerpt
                        {
                            let start_idx = (ev.core.start_line - 1) as usize;
                            let end_idx = ev.core.end_line as usize;
                            if end_idx <= lines.len() {
                                let actual = lines[start_idx..end_idx].join("\n");
                                if excerpt != &actual {
                                    total_invalid_citations += 1;
                                }
                            }
                        }
                    }
                }
                // Freshness check
                if let Some(ref meta) = ev.meta
                    && meta.freshness != Some(Freshness::VerifiedCurrent)
                {
                    total_invalid_citations += 1;
                }
            }
        }
    }

    // Compute percentiles
    all_latencies.sort_unstable();
    let warm_query_p50_ms = percentile(&all_latencies, 50);
    let warm_query_p95_ms = percentile(&all_latencies, 95);
    let warm_query_max_ms = *all_latencies.last().unwrap_or(&0);

    Ok(BenchWarmResult {
        success: true,
        index_build_ms,
        index_open_ms,
        queries: queries.len(),
        iterations,
        warm_query_p50_ms,
        warm_query_p95_ms,
        warm_query_max_ms,
        invalid_citations: total_invalid_citations,
        stale_hits_skipped: total_stale_skipped,
        notes: vec![format!(
            "warm benchmark: {} queries x {} iterations = {} total queries (index opened once)",
            queries.len(),
            iterations,
            all_latencies.len()
        )],
    })
}

/// Compute the percentile value from a sorted slice of u64 values.
fn percentile(sorted: &[u64], p: u64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((p as usize) * (sorted.len() - 1)) / 100;
    sorted[idx.min(sorted.len() - 1)]
}

// ── Provider/Dense helpers ──────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct ProviderStatusResult {
    success: bool,
    remote_default: bool,
    outbound_default: bool,
    supported_providers: Vec<String>,
    audit_path: String,
    policy_mode: String,
}

fn provider_status(repo_root: &Path) -> ProviderStatusResult {
    let audit_path = repo_root
        .join(openlocus_provider::audit::AUDIT_RELATIVE_PATH)
        .to_str()
        .unwrap_or_default()
        .to_string();
    ProviderStatusResult {
        success: true,
        remote_default: false,
        outbound_default: false,
        supported_providers: vec!["mock".into(), "disabled".into()],
        audit_path,
        policy_mode: "local_only".into(),
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ProviderAuditResult {
    success: bool,
    event_count: usize,
    events: Vec<serde_json::Value>,
}

fn provider_audit(repo_root: &Path, limit: usize) -> Result<ProviderAuditResult> {
    let events = audit::read_audit_events(repo_root)?;
    // Convert to JSON values, taking last `limit` events
    let json_events: Vec<serde_json::Value> = events
        .iter()
        .rev()
        .take(limit)
        .map(|e| serde_json::to_value(e).unwrap_or_default())
        .collect();
    Ok(ProviderAuditResult {
        success: true,
        event_count: json_events.len(),
        events: json_events,
    })
}

#[derive(Debug, Serialize, Deserialize)]
struct DenseBuildResult {
    success: bool,
    experimental: bool,
    provider: String,
    record_count: usize,
    skipped: usize,
    blocked: usize,
    remote_calls: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    store_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    audit_path: Option<String>,
}

fn dense_build(
    repo_root: &Path,
    policy: &Policy,
    provider_name: &str,
    experimental: bool,
) -> Result<DenseBuildResult> {
    if !experimental {
        return Ok(DenseBuildResult {
            success: false,
            experimental: false,
            provider: provider_name.into(),
            record_count: 0,
            skipped: 0,
            blocked: 0,
            remote_calls: 0,
            error: Some("dense indexing requires --experimental flag to opt in".into()),
            store_path: None,
            audit_path: None,
        });
    }

    let prov: Box<dyn EmbeddingProvider> = match provider::create_provider(provider_name) {
        Ok(p) => p,
        Err(e) => {
            // Write audit event for unknown provider
            let audit_event = openlocus_provider::model::EmbeddingAuditEvent {
                timestamp: chrono::Utc::now().to_rfc3339(),
                event: "provider_unavailable".into(),
                request_id: format!("build-{}", chrono::Utc::now().timestamp_millis()),
                provider_id: provider_name.into(),
                model_id: "unknown".into(),
                locality: openlocus_provider::model::ProviderLocality::Disabled,
                purpose: "index".into(),
                path: None,
                line_range: None,
                data_level: 0,
                view_kind: "metadata".into(),
                bytes_selected: 0,
                text_sha: String::new(),
                secret_scan: "skipped".into(),
                policy_decision: "deny".into(),
                cache_key: String::new(),
                outbound_attempted: false,
                reason: Some(e.to_string()),
            };
            let _ = openlocus_provider::audit::append_audit_event(repo_root, &audit_event);
            return Ok(DenseBuildResult {
                success: false,
                experimental: true,
                provider: provider_name.into(),
                record_count: 0,
                skipped: 0,
                blocked: 0,
                remote_calls: 0,
                error: Some(format!(
                    "unknown provider '{}'; supported providers: mock, disabled",
                    provider_name
                )),
                store_path: None,
                audit_path: None,
            });
        }
    };

    let metadata = prov.metadata().clone();

    if !metadata.locality.is_available() {
        // Write audit event for disabled provider
        let audit_event = openlocus_provider::model::EmbeddingAuditEvent {
            timestamp: chrono::Utc::now().to_rfc3339(),
            event: "provider_unavailable".into(),
            request_id: format!("build-{}", chrono::Utc::now().timestamp_millis()),
            provider_id: provider_name.into(),
            model_id: "disabled".into(),
            locality: openlocus_provider::model::ProviderLocality::Disabled,
            purpose: "index".into(),
            path: None,
            line_range: None,
            data_level: 0,
            view_kind: "metadata".into(),
            bytes_selected: 0,
            text_sha: String::new(),
            secret_scan: "skipped".into(),
            policy_decision: "deny".into(),
            cache_key: String::new(),
            outbound_attempted: false,
            reason: Some(format!("provider '{}' is not available", provider_name)),
        };
        let _ = openlocus_provider::audit::append_audit_event(repo_root, &audit_event);
        return Ok(DenseBuildResult {
            success: false,
            experimental: true,
            provider: provider_name.into(),
            record_count: 0,
            skipped: 0,
            blocked: 0,
            remote_calls: 0,
            error: Some(format!("provider '{}' is not available", provider_name)),
            store_path: None,
            audit_path: None,
        });
    }

    let records = scan_repo(repo_root, policy)?;
    let build_result =
        JsonlEmbeddingStore::build(repo_root, &records, prov.as_ref(), &metadata, policy)?;

    let store_path = repo_root
        .join(openlocus_provider::dense_store::STORE_RELATIVE_PATH)
        .to_str()
        .unwrap_or_default()
        .to_string();
    let audit_path = repo_root
        .join(openlocus_provider::audit::AUDIT_RELATIVE_PATH)
        .to_str()
        .unwrap_or_default()
        .to_string();

    Ok(DenseBuildResult {
        success: true,
        experimental: true,
        provider: provider_name.into(),
        record_count: build_result.record_count,
        skipped: build_result.skipped,
        blocked: build_result.blocked,
        remote_calls: 0,
        error: None,
        store_path: Some(store_path),
        audit_path: Some(audit_path),
    })
}

#[derive(Debug, Serialize, Deserialize)]
struct DenseSearchResult {
    success: bool,
    query_sha: String,
    query_len: usize,
    provider: String,
    evidence: Vec<Evidence>,
    skipped_count: usize,
    blocked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

fn dense_search(
    repo_root: &Path,
    policy: &Policy,
    query: &str,
    provider_name: &str,
    limit: usize,
) -> Result<DenseSearchResult> {
    let query_sha = blake3::hash(query.as_bytes()).to_hex().to_string();
    let query_len = query.len();

    let prov: Box<dyn EmbeddingProvider> = match provider::create_provider(provider_name) {
        Ok(p) => p,
        Err(e) => {
            // Write audit event for unknown provider
            let audit_event = openlocus_provider::model::EmbeddingAuditEvent {
                timestamp: chrono::Utc::now().to_rfc3339(),
                event: "provider_unavailable".into(),
                request_id: format!("search-{}", chrono::Utc::now().timestamp_millis()),
                provider_id: provider_name.into(),
                model_id: "unknown".into(),
                locality: openlocus_provider::model::ProviderLocality::Disabled,
                purpose: "query".into(),
                path: None,
                line_range: None,
                data_level: 0,
                view_kind: "query".into(),
                bytes_selected: 0,
                text_sha: query_sha.clone(),
                secret_scan: "skipped".into(),
                policy_decision: "deny".into(),
                cache_key: String::new(),
                outbound_attempted: false,
                reason: Some(e.to_string()),
            };
            let _ = openlocus_provider::audit::append_audit_event(repo_root, &audit_event);
            return Ok(DenseSearchResult {
                success: false,
                query_sha,
                query_len,
                provider: provider_name.into(),
                evidence: vec![],
                skipped_count: 0,
                blocked: false,
                reason: Some(format!(
                    "unknown provider '{}'; supported providers: mock, disabled",
                    provider_name
                )),
            });
        }
    };

    let metadata = prov.metadata().clone();

    if !metadata.locality.is_available() {
        // Write audit event for disabled provider
        let audit_event = openlocus_provider::model::EmbeddingAuditEvent {
            timestamp: chrono::Utc::now().to_rfc3339(),
            event: "provider_unavailable".into(),
            request_id: format!("search-{}", chrono::Utc::now().timestamp_millis()),
            provider_id: provider_name.into(),
            model_id: "disabled".into(),
            locality: openlocus_provider::model::ProviderLocality::Disabled,
            purpose: "query".into(),
            path: None,
            line_range: None,
            data_level: 0,
            view_kind: "query".into(),
            bytes_selected: 0,
            text_sha: query_sha.clone(),
            secret_scan: "skipped".into(),
            policy_decision: "deny".into(),
            cache_key: String::new(),
            outbound_attempted: false,
            reason: Some(format!("provider '{}' is not available", provider_name)),
        };
        let _ = openlocus_provider::audit::append_audit_event(repo_root, &audit_event);
        return Ok(DenseSearchResult {
            success: false,
            query_sha,
            query_len,
            provider: provider_name.into(),
            evidence: vec![],
            skipped_count: 0,
            blocked: false,
            reason: Some(format!("provider '{}' is not available", provider_name)),
        });
    }

    let search_result =
        JsonlEmbeddingStore::search(repo_root, query, prov.as_ref(), &metadata, policy, limit)?;

    if search_result.blocked {
        return Ok(DenseSearchResult {
            success: false,
            query_sha,
            query_len,
            provider: provider_name.into(),
            evidence: vec![],
            skipped_count: 0,
            blocked: true,
            reason: search_result.reason,
        });
    }

    if search_result.reason.is_some() && search_result.hits.is_empty() {
        return Ok(DenseSearchResult {
            success: false,
            query_sha,
            query_len,
            provider: provider_name.into(),
            evidence: vec![],
            skipped_count: 0,
            blocked: false,
            reason: search_result.reason,
        });
    }

    // Materialize StoreHits to Evidence
    let mut evidence = Vec::new();
    let mut skipped_count = 0usize;
    for hit in &search_result.hits {
        match openlocus_store::materialize_evidence(repo_root, hit, Channel::Dense) {
            Ok(ev) => evidence.push(ev),
            Err(_) => skipped_count += 1,
        }
    }

    Ok(DenseSearchResult {
        success: true,
        query_sha,
        query_len,
        provider: provider_name.into(),
        evidence,
        skipped_count,
        blocked: false,
        reason: None,
    })
}

#[derive(Debug, Serialize, Deserialize)]
struct DensePurgeResult {
    success: bool,
    purged: bool,
    record_count: usize,
}

fn dense_purge(repo_root: &Path) -> Result<DensePurgeResult> {
    let count = JsonlEmbeddingStore::purge(repo_root)?;
    Ok(DensePurgeResult {
        success: true,
        purged: true,
        record_count: count,
    })
}
