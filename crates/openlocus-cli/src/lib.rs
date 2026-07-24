use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use openlocus::{Citation, Engine, QueryRequest, default_state_root};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Parser)]
#[command(
    name = "openlocus",
    version,
    about = "Local, current-source-verified code evidence"
)]
struct Cli {
    /// Source tree. Defaults to the nearest parent containing .git.
    #[arg(long, global = true)]
    root: Option<PathBuf>,

    /// Index state directory. Defaults to the operating-system cache directory.
    #[arg(long, global = true)]
    state_root: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Read a policy-allowed source path, optionally with :LINE or :START-END.
    Read { path_spec: String },

    /// Retrieve verified evidence with persistent BM25 plus exact literal search.
    Query {
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },

    /// Manage the persistent index.
    Index {
        #[command(subcommand)]
        command: IndexCommand,
    },

    /// Validate source citations against current files.
    Citations {
        #[command(subcommand)]
        command: CitationCommand,
    },
}

#[derive(Debug, Subcommand)]
enum IndexCommand {
    Build,
    Status,
    Update {
        #[arg(required = true, num_args = 1..)]
        paths: Vec<PathBuf>,
    },
    Purge,
}

#[derive(Debug, Subcommand)]
enum CitationCommand {
    /// Validate a JSON array of {path,start_line,end_line,content_sha} objects.
    Validate { file: PathBuf },
}

pub fn run() -> Result<()> {
    run_with(Cli::parse())
}

fn run_with(cli: Cli) -> Result<()> {
    let source_root = match cli.root {
        Some(root) => root,
        None => discover_source_root(&std::env::current_dir()?)?,
    };
    let state_root = match cli.state_root {
        Some(root) => root,
        None => default_state_root(&source_root)?,
    };
    let mut engine = Engine::open(&source_root, &state_root)?;

    match cli.command {
        Command::Read { path_spec } => print_json(&engine.read(&path_spec)?),
        Command::Query { query, limit } => {
            let mut request = QueryRequest::new(query);
            request.max_results = limit;
            print_json(&engine.query(request)?)
        }
        Command::Index { command } => match command {
            IndexCommand::Build => print_json(&engine.build_index()?),
            IndexCommand::Status => print_json(&engine.index_status()),
            IndexCommand::Update { paths } => print_json(&engine.update_paths(&paths)?),
            IndexCommand::Purge => {
                engine.purge_index()?;
                print_json(&serde_json::json!({ "purged": true }))
            }
        },
        Command::Citations { command } => match command {
            CitationCommand::Validate { file } => {
                let content = fs::read_to_string(&file)
                    .with_context(|| format!("failed to read {}", file.display()))?;
                let citations: Vec<Citation> = serde_json::from_str(&content)
                    .with_context(|| format!("invalid citation JSON: {}", file.display()))?;
                print_json(&engine.validate_citations(citations))
            }
        },
    }
}

fn discover_source_root(start: &Path) -> Result<PathBuf> {
    let mut directory = start
        .canonicalize()
        .with_context(|| format!("working directory does not exist: {}", start.display()))?;
    loop {
        if directory.join(".git").exists() {
            return Ok(directory);
        }
        if !directory.pop() {
            bail!("no source root found; pass --root explicitly");
        }
    }
}

fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_is_strict() {
        let root = tempfile::tempdir().unwrap();
        let child = root.path().join("nested");
        fs::create_dir(&child).unwrap();
        assert!(discover_source_root(&child).is_err());
        fs::create_dir(root.path().join(".git")).unwrap();
        assert_eq!(
            discover_source_root(&child).unwrap(),
            root.path().canonicalize().unwrap()
        );
    }
}
