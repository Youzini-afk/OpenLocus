use crate::model::{Candidate, Channel, Citation, CitationValidation, Evidence};
use crate::policy::Policy;
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Component, Path, PathBuf};

const DEFAULT_IGNORED_COMPONENTS: &[&str] =
    &[".git", ".openlocus", "target", "node_modules", "dist"];
const CONTEXT_LINES: u64 = 2;
const MAX_EVIDENCE_LINES: u64 = 7;

#[derive(Debug, Clone)]
pub(crate) struct FileRecord {
    pub path: String,
    pub size: u64,
    pub content_sha: String,
}

pub(crate) enum MaterializedCandidate {
    Evidence(Evidence),
    Stale,
    Invalid,
}

pub(crate) fn canonical_source_root(root: &Path) -> Result<PathBuf> {
    let root = root
        .canonicalize()
        .with_context(|| format!("source root does not exist: {}", root.display()))?;
    if !root.is_dir() {
        bail!("source root is not a directory: {}", root.display());
    }
    Ok(root)
}

pub(crate) fn normalize_relative(path: &Path) -> Result<String> {
    if path.is_absolute() {
        bail!("path must be relative to the source root");
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => parts.push(value.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("path traversal is not allowed")
            }
        }
    }
    if parts.is_empty() {
        bail!("path must not be empty");
    }
    Ok(parts.join("/"))
}

pub(crate) fn validate_source_path(source_root: &Path, relative: &str) -> Result<PathBuf> {
    let normalized = normalize_relative(Path::new(relative))?;
    let source_root = canonical_source_root(source_root)?;
    let joined = source_root.join(normalized.replace('/', std::path::MAIN_SEPARATOR_STR));
    let target = joined
        .canonicalize()
        .with_context(|| format!("source path does not exist: {relative}"))?;
    if !target.starts_with(&source_root) {
        bail!("source path escapes the source root: {relative}");
    }
    Ok(target)
}

pub(crate) fn scan(source_root: &Path, policy: &Policy) -> Result<Vec<FileRecord>> {
    let mut builder = ignore::WalkBuilder::new(source_root);
    builder.hidden(false);
    builder.git_ignore(!policy.include_gitignored);
    builder.git_global(!policy.include_gitignored);
    builder.git_exclude(!policy.include_gitignored);

    let mut records = Vec::new();
    for entry in builder.build().filter_map(Result::ok) {
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let relative = match entry.path().strip_prefix(source_root) {
            Ok(path) => path,
            Err(_) => continue,
        };
        let path = relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        if is_default_ignored(&path) || !policy.allows(&path) {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) if metadata.len() <= policy.max_file_bytes => metadata,
            _ => continue,
        };
        let bytes = match fs::read(entry.path()) {
            Ok(bytes) if !bytes.contains(&0) && std::str::from_utf8(&bytes).is_ok() => bytes,
            _ => continue,
        };
        records.push(FileRecord {
            path,
            size: metadata.len(),
            content_sha: blake3::hash(&bytes).to_hex().to_string(),
        });
    }
    records.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(records)
}

fn is_default_ignored(path: &str) -> bool {
    path.split('/')
        .any(|part| DEFAULT_IGNORED_COMPONENTS.contains(&part))
}

pub(crate) fn literal_search(
    source_root: &Path,
    policy: &Policy,
    query: &str,
    max_results: usize,
) -> Result<Vec<Evidence>> {
    if query.is_empty() {
        bail!("query must not be empty");
    }
    // ponytail: O(repo bytes); add a literal index only after profiling proves this is the bottleneck.
    let records = scan(source_root, policy)?;
    let mut results = Vec::new();
    for record in records {
        if results.len() >= max_results {
            break;
        }
        let path = match validate_source_path(source_root, &record.path) {
            Ok(path) => path,
            Err(_) => continue,
        };
        let bytes = match fs::read(path) {
            Ok(bytes)
                if bytes.len() as u64 == record.size
                    && blake3::hash(&bytes).to_hex().as_str() == record.content_sha =>
            {
                bytes
            }
            Err(_) => continue,
            _ => continue,
        };
        let content = match std::str::from_utf8(&bytes) {
            Ok(content) => content,
            Err(_) => continue,
        };
        let content_sha = record.content_sha;
        let lines = content.lines().collect::<Vec<_>>();
        for (index, line) in lines.iter().enumerate() {
            if results.len() >= max_results {
                break;
            }
            if line.contains(query) {
                let line_number = index as u64 + 1;
                let (start, end, excerpt) = excerpt_window(&lines, line_number);
                results.push(Evidence::verified(
                    record.path.clone(),
                    start,
                    end,
                    content_sha.clone(),
                    excerpt,
                    1.0,
                    vec![format!("literal match: {query}")],
                    vec![Channel::Literal],
                ));
            }
        }
    }
    Ok(results)
}

pub(crate) fn read(source_root: &Path, policy: &Policy, path_spec: &str) -> Result<Evidence> {
    let (path, requested) = parse_path_spec(path_spec)?;
    if !policy.allows(&path) {
        bail!("source path is excluded by policy: {path}");
    }
    let full_path = validate_source_path(source_root, &path)?;
    if fs::metadata(&full_path)?.len() > policy.max_file_bytes {
        bail!("source file exceeds max_file_bytes: {path}");
    }
    let content = fs::read_to_string(&full_path)
        .with_context(|| format!("source file is not valid UTF-8: {path}"))?;
    let lines = content.lines().collect::<Vec<_>>();
    if lines.is_empty() {
        bail!("source file is empty: {path}");
    }
    let (start, end) = match requested {
        Some((start, end)) => {
            if start == 0 || start > end || start > lines.len() as u64 {
                bail!("invalid line range {start}-{end} for {path}");
            }
            (start, end.min(lines.len() as u64))
        }
        None => (1, lines.len() as u64),
    };
    let excerpt = lines[(start - 1) as usize..end as usize].join("\n");
    Ok(Evidence::verified(
        path,
        start,
        end,
        blake3::hash(content.as_bytes()).to_hex().to_string(),
        excerpt,
        1.0,
        vec!["direct source read".into()],
        vec![Channel::Path],
    ))
}

fn parse_path_spec(spec: &str) -> Result<(String, Option<(u64, u64)>)> {
    if let Some((path, suffix)) = spec.rsplit_once(':') {
        if let Some((start, end)) = suffix.split_once('-') {
            if let (Ok(start), Ok(end)) = (start.parse(), end.parse()) {
                return Ok((normalize_relative(Path::new(path))?, Some((start, end))));
            }
        } else if let Ok(line) = suffix.parse() {
            return Ok((normalize_relative(Path::new(path))?, Some((line, line))));
        }
    }
    Ok((normalize_relative(Path::new(spec))?, None))
}

pub(crate) fn materialize_candidate(
    source_root: &Path,
    candidate: Candidate,
    query: &str,
) -> MaterializedCandidate {
    if candidate.expected_sha.is_empty() {
        return MaterializedCandidate::Invalid;
    }
    let path = match validate_source_path(source_root, &candidate.path) {
        Ok(path) => path,
        Err(_) => return MaterializedCandidate::Invalid,
    };
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(_) => return MaterializedCandidate::Invalid,
    };
    let current_sha = blake3::hash(content.as_bytes()).to_hex().to_string();
    if current_sha != candidate.expected_sha {
        return MaterializedCandidate::Stale;
    }
    let lines = content.lines().collect::<Vec<_>>();
    let total_lines = lines.len() as u64;
    if candidate.start_line == 0
        || candidate.start_line > candidate.end_line
        || candidate.end_line > total_lines
    {
        return MaterializedCandidate::Invalid;
    }
    let tokens = query_tokens(query);
    let best_line = (candidate.start_line..=candidate.end_line)
        .map(|line_number| {
            let line = lines[(line_number - 1) as usize].to_ascii_lowercase();
            let score = tokens.iter().filter(|token| line.contains(*token)).count();
            (line_number, score)
        })
        .filter(|(_, score)| *score > 0)
        .max_by(|left, right| left.1.cmp(&right.1).then_with(|| right.0.cmp(&left.0)))
        .map(|(line, _)| line);
    let Some(best_line) = best_line else {
        return MaterializedCandidate::Invalid;
    };
    let (start, end, excerpt) = excerpt_window(&lines, best_line);
    MaterializedCandidate::Evidence(Evidence::verified(
        candidate.path,
        start,
        end,
        current_sha,
        excerpt,
        candidate.score,
        vec![format!("bm25 match: {query}")],
        vec![Channel::Bm25],
    ))
}

fn query_tokens(query: &str) -> Vec<String> {
    query
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn excerpt_window(lines: &[&str], center: u64) -> (u64, u64, String) {
    let start = center.saturating_sub(CONTEXT_LINES).max(1);
    let end = (center + CONTEXT_LINES)
        .min(lines.len() as u64)
        .min(start + MAX_EVIDENCE_LINES - 1);
    let excerpt = lines[(start - 1) as usize..end as usize].join("\n");
    (start, end, excerpt)
}

pub(crate) fn validate_citation(
    source_root: &Path,
    policy: &Policy,
    citation: Citation,
) -> CitationValidation {
    let invalid = |citation, reason: String| CitationValidation {
        citation,
        valid: false,
        reason: Some(reason),
    };
    if !policy.allows(&citation.path) {
        return invalid(citation, "source path is excluded by policy".into());
    }
    let path = match validate_source_path(source_root, &citation.path) {
        Ok(path) => path,
        Err(error) => return invalid(citation, error.to_string()),
    };
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) => return invalid(citation, error.to_string()),
    };
    let current_sha = blake3::hash(content.as_bytes()).to_hex().to_string();
    if current_sha != citation.content_sha {
        return invalid(citation, "content hash is stale".into());
    }
    let total_lines = content.lines().count() as u64;
    if citation.start_line == 0
        || citation.start_line > citation.end_line
        || citation.end_line > total_lines
    {
        return invalid(citation, "line range is invalid".into());
    }
    CitationValidation {
        citation,
        valid: true,
        reason: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traversal_is_rejected() {
        assert!(normalize_relative(Path::new("../secret.txt")).is_err());
        assert!(normalize_relative(Path::new("src/lib.rs")).is_ok());
    }

    #[test]
    fn literal_results_are_current_and_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("b.rs"), "needle\n").unwrap();
        fs::write(dir.path().join("a.rs"), "needle\n").unwrap();
        let results = literal_search(dir.path(), &Policy::default(), "needle", 10).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].path(), "a.rs");
        assert_eq!(results[1].path(), "b.rs");
    }

    #[test]
    fn stale_candidate_is_not_materialized() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "fn current() {}\n").unwrap();
        let candidate = Candidate {
            path: "a.rs".into(),
            start_line: 1,
            end_line: 1,
            expected_sha: "old".into(),
            score: 1.0,
        };
        assert!(matches!(
            materialize_candidate(dir.path(), candidate, "current"),
            MaterializedCandidate::Stale
        ));
    }
}
