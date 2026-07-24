use anyhow::Result;
use openlocus_core::Policy;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use crate::read::guess_language;

/// A record for a scanned file in the repo.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRecord {
    pub path: String,
    pub size: u64,
    pub content_sha: String,
    pub language: String,
}

/// Default ignore patterns used when no policy is loaded.
const DEFAULT_IGNORE_PATTERNS: &[&str] = &[".git", "target", "node_modules", "dist", ".openlocus"];

/// Scan the repo at `root`, respecting basic ignore defaults and policy.
/// Returns a list of FileRecord with path, size, content_sha, and language guess.
pub fn scan_repo(root: &Path, policy: &Policy) -> Result<Vec<FileRecord>> {
    let mut builder = ignore::WalkBuilder::new(root);
    builder.hidden(false);
    // Policy controls gitignore handling
    builder.git_ignore(!policy.index.include_gitignored);
    builder.git_global(!policy.index.include_gitignored);
    builder.git_exclude(!policy.index.include_gitignored);

    let mut records = Vec::new();

    for result in builder.build() {
        match result {
            Ok(entry) => {
                if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                    continue;
                }

                let path = entry.path();

                // Get relative path
                let relative = path.strip_prefix(root).unwrap_or(path);
                let relative_str = relative
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");

                // Skip files matching default ignore dirs
                if is_default_ignored(&relative_str) {
                    continue;
                }

                // Skip files matching policy exclude patterns
                if is_excluded(&relative_str, &policy.index.exclude) {
                    continue;
                }

                // Apply policy include filter (default "**/*" matches everything)
                if !is_included(&relative_str, &policy.index.include) {
                    continue;
                }

                // Skip binary-ish files
                if looks_binary(path) {
                    continue;
                }

                let metadata = match fs::metadata(path) {
                    Ok(m) => m,
                    Err(_) => continue,
                };

                let size = metadata.len();

                let content_sha = match compute_file_sha(path) {
                    Ok(sha) => sha,
                    Err(_) => continue,
                };

                let language = guess_language(&relative_str);

                records.push(FileRecord {
                    path: relative_str,
                    size,
                    content_sha,
                    language,
                });
            }
            Err(_) => continue,
        }
    }

    // Sort by path for deterministic output
    records.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(records)
}

fn compute_file_sha(path: &Path) -> Result<String> {
    let bytes = fs::read(path)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

/// Simple check: if the first 8KB contain a null byte, consider it binary.
fn looks_binary(path: &Path) -> bool {
    match fs::read(path) {
        Ok(bytes) => {
            let check_len = bytes.len().min(8192);
            bytes[..check_len].contains(&0)
        }
        Err(_) => true,
    }
}

/// Check if path falls under one of the default ignore directories.
fn is_default_ignored(path: &str) -> bool {
    let path = path.replace('\\', "/");
    for pat in DEFAULT_IGNORE_PATTERNS {
        // e.g. ".git" matches any path component
        let pat_str = *pat;
        for component in path.split('/') {
            if component == pat_str {
                return true;
            }
        }
    }
    false
}

/// Simple glob-style exclusion check.
fn is_excluded(path: &str, patterns: &[String]) -> bool {
    for pattern in patterns {
        if glob_match(pattern, path) {
            return true;
        }
    }
    false
}

/// Simple glob-style inclusion check.
/// A path is included if it matches ANY include pattern.
/// Default include is ["**/*"] which matches everything.
fn is_included(path: &str, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return true;
    }
    for pattern in patterns {
        if glob_match(pattern, path) {
            return true;
        }
    }
    false
}

/// Very simple glob matching sufficient for exclude patterns like
/// `.git/**`, `target/**`, `node_modules/**`, `dist/**`, `.env*`, `**/*.pem`, `**/*`.
fn glob_match(pattern: &str, path: &str) -> bool {
    let pattern = pattern.replace('\\', "/");
    let path = path.replace('\\', "/");

    // "**" or "**/*" matches everything
    if pattern == "**" || pattern == "**/*" {
        return true;
    }

    if pattern.contains("/**") {
        let dir = &pattern[..pattern.len() - 3];
        if path == dir || path.starts_with(&format!("{}/", dir)) {
            return true;
        }
    }

    if pattern.starts_with("/**") {
        return true;
    }

    if pattern.starts_with("**/*.") {
        // e.g. **/*.pem -> match any file ending in .pem at any depth
        let ext_pattern = &pattern[3..]; // after "**/" -> "*.pem"
        if let Some(ext) = ext_pattern.strip_prefix("*.")
            && path.ends_with(&format!(".{}", ext))
        {
            return true;
        }
    }

    if pattern.starts_with("*.") {
        let path_file = path.rsplit('/').next().unwrap_or(&path);
        if let Some(ext) = pattern.strip_prefix("*.")
            && path_file.ends_with(&format!(".{}", ext))
        {
            return true;
        }
    }

    if pattern.starts_with(".env*") {
        let path_file = path.rsplit('/').next().unwrap_or(&path);
        if path_file.starts_with(".env") {
            return true;
        }
    }

    // Exact match
    if path == pattern {
        return true;
    }

    false
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matching() {
        assert!(glob_match(".git/**", ".git/config"));
        assert!(glob_match(".git/**", ".git"));
        assert!(glob_match("target/**", "target/debug/app"));
        assert!(glob_match("**/*.pem", "secrets/key.pem"));
        assert!(glob_match(".env*", ".env"));
        assert!(glob_match(".env*", ".env.local"));
        assert!(!glob_match(".git/**", "src/main.rs"));
    }

    #[test]
    fn scan_finds_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("README.md"), "# Hello").unwrap();

        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/config"), "stuff").unwrap();

        let policy = Policy::default();
        let records = scan_repo(root, &policy).unwrap();

        let paths: Vec<&str> = records.iter().map(|r| r.path.as_str()).collect();
        assert!(paths.contains(&"src/main.rs"));
        assert!(paths.contains(&"README.md"));
        assert!(!paths.iter().any(|p| p.contains(".git")));
    }

    #[test]
    fn scan_computes_sha() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("hello.txt"), "hello world").unwrap();

        let policy = Policy::default();
        let records = scan_repo(root, &policy).unwrap();
        assert_eq!(records.len(), 1);
        assert!(!records[0].content_sha.is_empty());
        assert_eq!(records[0].size, 11);
    }

    #[test]
    fn scan_skips_binary() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("binary.dat"), b"hello\x00world").unwrap();
        fs::write(root.join("text.txt"), "hello world").unwrap();

        let policy = Policy::default();
        let records = scan_repo(root, &policy).unwrap();
        let paths: Vec<&str> = records.iter().map(|r| r.path.as_str()).collect();
        assert!(paths.contains(&"text.txt"));
        assert!(!paths.contains(&"binary.dat"));
    }

    #[test]
    fn scan_include_restricts_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("tests")).unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("tests/test.rs"), "#[test] fn t() {}").unwrap();
        fs::write(root.join("docs/readme.md"), "# Doc").unwrap();
        fs::write(root.join("Cargo.toml"), "[workspace]").unwrap();

        let mut policy = Policy::default();
        policy.index.include = vec!["src/**".into(), "tests/**".into()];

        let records = scan_repo(root, &policy).unwrap();
        let paths: Vec<&str> = records.iter().map(|r| r.path.as_str()).collect();
        assert!(
            paths.contains(&"src/main.rs"),
            "src/main.rs should be included"
        );
        assert!(
            paths.contains(&"tests/test.rs"),
            "tests/test.rs should be included"
        );
        assert!(
            !paths.contains(&"docs/readme.md"),
            "docs/readme.md should be excluded by include filter"
        );
        assert!(
            !paths.contains(&"Cargo.toml"),
            "Cargo.toml should be excluded by include filter"
        );
    }

    #[test]
    fn scan_include_gitignored() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // The ignore crate only respects .gitignore if it detects a git repo.
        // Create a .git directory so the walker recognizes this as a git repo.
        fs::create_dir_all(root.join(".git")).unwrap();

        // Create a .gitignore that ignores *.log
        fs::write(root.join(".gitignore"), "*.log\n").unwrap();
        fs::write(root.join("keep.txt"), "content").unwrap();
        fs::write(root.join("skip.log"), "log content").unwrap();

        // With default policy (include_gitignored=false), .log should be skipped
        let policy = Policy::default();
        let records = scan_repo(root, &policy).unwrap();
        let paths: Vec<&str> = records.iter().map(|r| r.path.as_str()).collect();
        assert!(paths.contains(&"keep.txt"));
        assert!(
            !paths.contains(&"skip.log"),
            "gitignored files should be skipped by default"
        );

        // With include_gitignored=true, .log should appear
        let mut policy_incl = Policy::default();
        policy_incl.index.include_gitignored = true;
        let records_incl = scan_repo(root, &policy_incl).unwrap();
        let paths_incl: Vec<&str> = records_incl.iter().map(|r| r.path.as_str()).collect();
        assert!(
            paths_incl.contains(&"skip.log"),
            "gitignored files should appear when include_gitignored=true"
        );
    }
}
