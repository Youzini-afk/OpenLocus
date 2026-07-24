use anyhow::{Context, Result, bail};
use openlocus_core::{Channel, Evidence, Freshness};
use std::fs;
use std::path::{Path, PathBuf};

/// Parse a path spec like `README.md` or `README.md:1-20`.
/// Returns (relative_path, optional start_line, optional end_line).
/// Lines are 1-indexed.
pub fn parse_path_spec(spec: &str) -> Result<(String, Option<u64>, Option<u64>)> {
    if let Some((path_part, range_part)) = spec.rsplit_once(':') {
        // Check if range_part looks like "N-M" or "N"
        if let Some((s, e)) = range_part.split_once('-') {
            let start: u64 = s
                .parse()
                .with_context(|| format!("invalid start line: {}", s))?;
            let end: u64 = e
                .parse()
                .with_context(|| format!("invalid end line: {}", e))?;
            if start == 0 || end == 0 {
                bail!("line numbers are 1-indexed, got 0");
            }
            if start > end {
                bail!("start_line ({}) > end_line ({})", start, end);
            }
            return Ok((path_part.to_string(), Some(start), Some(end)));
        }
        // Could be just a line number like "file.rs:5"
        if let Ok(n) = range_part.parse::<u64>() {
            if n == 0 {
                bail!("line numbers are 1-indexed, got 0");
            }
            return Ok((path_part.to_string(), Some(n), Some(n)));
        }
    }
    Ok((spec.to_string(), None, None))
}

/// Validate that a relative path doesn't escape the repo root.
///
/// Security checks:
/// - Rejects absolute paths.
/// - Rejects `..` traversal components.
/// - For paths that resolve to existing targets, canonicalizes the target
///   and verifies it stays under the canonical repo root (prevents symlink escape).
pub fn validate_path(repo_root: &Path, relative: &str) -> Result<PathBuf> {
    // Reject absolute paths
    if relative.starts_with('/') {
        bail!("absolute paths are not allowed: {}", relative);
    }

    // Check components for traversal before joining
    for comp in Path::new(relative).components() {
        match comp {
            std::path::Component::ParentDir => {
                bail!("path traversal (..) is not allowed: {}", relative);
            }
            std::path::Component::CurDir => {}
            std::path::Component::Normal(_) => {}
            _ => bail!("invalid path component in: {}", relative),
        }
    }

    let canonical_root = repo_root
        .canonicalize()
        .context("repo root does not exist")?;

    let composed = canonical_root.join(relative);

    // If the target already exists, canonicalize and verify it stays under root.
    // This catches symlinks that escape the repo.
    if composed.exists() {
        let canonical_target = composed
            .canonicalize()
            .with_context(|| format!("cannot canonicalize {}", relative))?;
        if !canonical_target.starts_with(&canonical_root) {
            bail!("path escapes repo root (possible symlink): {}", relative);
        }
        return Ok(canonical_target);
    }

    // Target doesn't exist yet — we've already rejected .. and absolute paths,
    // so composed is guaranteed to be under canonical_root.
    Ok(composed)
}

/// Compute blake3 hash of file bytes.
pub fn compute_content_sha(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

/// Read a file from the repo, optionally extracting a line range.
/// Returns Evidence with excerpt in meta.
pub fn read_file(repo_root: &Path, path_spec: &str) -> Result<Evidence> {
    let (relative, start_opt, end_opt) = parse_path_spec(path_spec)?;
    let full_path = validate_path(repo_root, &relative)?;

    if !full_path.exists() {
        bail!("file not found: {}", relative);
    }
    if !full_path.is_file() {
        bail!("not a file: {}", relative);
    }

    let content_sha = compute_content_sha(&full_path)?;
    let content =
        fs::read_to_string(&full_path).with_context(|| format!("failed to read {}", relative))?;

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len() as u64;

    let (start_line, end_line) = match (start_opt, end_opt) {
        (Some(s), Some(e)) => (s, e.min(total_lines)),
        _ => (1, total_lines),
    };

    if start_line > total_lines {
        bail!(
            "start_line {} exceeds file length {}",
            start_line,
            total_lines
        );
    }

    let excerpt_lines = &lines[(start_line as usize - 1)..(end_line as usize)];
    let excerpt = excerpt_lines.join("\n");

    let language = guess_language(&relative);

    let evidence = Evidence::new(
        relative,
        start_line,
        end_line,
        content_sha,
        1.0,
        vec![format!("read {}", path_spec)],
        vec![Channel::Path],
    )
    .with_excerpt(&excerpt)
    .with_language(&language)
    .with_freshness(Freshness::VerifiedCurrent);

    Ok(evidence)
}

/// Simple language guess from file extension.
pub fn guess_language(path: &str) -> String {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    match ext {
        "rs" => "rust",
        "ts" => "typescript",
        "tsx" => "tsx",
        "js" => "javascript",
        "jsx" => "jsx",
        "py" => "python",
        "go" => "go",
        "java" => "java",
        "kt" => "kotlin",
        "rb" => "ruby",
        "cpp" | "cc" | "cxx" => "cpp",
        "c" => "c",
        "h" | "hpp" => "cpp",
        "cs" => "csharp",
        "swift" => "swift",
        "scala" => "scala",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "json" => "json",
        "md" => "markdown",
        "html" => "html",
        "css" => "css",
        "sh" | "bash" => "bash",
        "sql" => "sql",
        _ => "unknown",
    }
    .to_string()
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn symlink_file(src: &Path, dst: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(src, dst)
    }

    #[cfg(windows)]
    fn symlink_file(src: &Path, dst: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_file(src, dst)
    }

    #[cfg(windows)]
    fn symlink_unavailable_for_test(err: &std::io::Error) -> bool {
        err.raw_os_error() == Some(1314)
    }

    #[cfg(not(windows))]
    fn symlink_unavailable_for_test(_err: &std::io::Error) -> bool {
        false
    }

    fn create_symlink_file_for_test(src: &Path, dst: &Path) -> bool {
        match symlink_file(src, dst) {
            Ok(()) => true,
            Err(err) if symlink_unavailable_for_test(&err) => false,
            Err(err) => panic!("failed to create symlink test fixture: {err}"),
        }
    }
    use std::io::Write;

    #[test]
    fn parse_simple_path() {
        let (p, s, e) = parse_path_spec("README.md").unwrap();
        assert_eq!(p, "README.md");
        assert_eq!(s, None);
        assert_eq!(e, None);
    }

    #[test]
    fn parse_range_path() {
        let (p, s, e) = parse_path_spec("src/main.rs:10-20").unwrap();
        assert_eq!(p, "src/main.rs");
        assert_eq!(s, Some(10));
        assert_eq!(e, Some(20));
    }

    #[test]
    fn parse_single_line() {
        let (p, s, e) = parse_path_spec("lib.rs:5").unwrap();
        assert_eq!(p, "lib.rs");
        assert_eq!(s, Some(5));
        assert_eq!(e, Some(5));
    }

    #[test]
    fn reject_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let result = validate_path(dir.path(), "/etc/passwd");
        assert!(result.is_err());
    }

    #[test]
    fn reject_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let result = validate_path(dir.path(), "../../../etc/passwd");
        assert!(result.is_err());
    }

    #[test]
    fn reject_symlink_escape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Create a real file outside the repo (as a sibling directory)
        let outside_dir = dir.path().join("..").join("outside_escape_test");
        fs::create_dir_all(&outside_dir).unwrap();
        fs::write(outside_dir.join("secret.txt"), "secret").unwrap();

        let outside_file = outside_dir.join("secret.txt");
        if !create_symlink_file_for_test(&outside_file, &root.join("escape.txt")) {
            return;
        }

        let result = validate_path(root, "escape.txt");
        assert!(result.is_err(), "symlink escaping repo should be rejected");
    }

    #[test]
    fn accept_symlink_within_repo() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Create a real file inside the repo
        fs::write(root.join("real.txt"), "content").unwrap();

        if !create_symlink_file_for_test(&root.join("real.txt"), &root.join("link.txt")) {
            return;
        }

        let result = validate_path(root, "link.txt");
        assert!(result.is_ok(), "symlink within repo should be accepted");
    }

    #[test]
    fn read_file_with_range() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        let mut f = fs::File::create(&file_path).unwrap();
        for i in 1..=10u64 {
            writeln!(f, "line {}", i).unwrap();
        }

        let evidence = read_file(dir.path(), "test.txt:2-4").unwrap();
        assert_eq!(evidence.core.start_line, 2);
        assert_eq!(evidence.core.end_line, 4);
        assert!(evidence.core.content_sha.len() > 10);
        let meta = evidence.meta.as_ref().unwrap();
        assert_eq!(meta.excerpt.as_deref(), Some("line 2\nline 3\nline 4"));
    }

    #[test]
    fn read_file_full() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("hello.rs");
        fs::write(&file_path, "fn main() {}\n").unwrap();

        let evidence = read_file(dir.path(), "hello.rs").unwrap();
        assert_eq!(evidence.core.start_line, 1);
        assert_eq!(evidence.core.end_line, 1);
        assert_eq!(
            evidence.meta.as_ref().unwrap().language.as_deref(),
            Some("rust")
        );
    }

    #[test]
    fn guess_language_tests() {
        assert_eq!(guess_language("foo.rs"), "rust");
        assert_eq!(guess_language("bar.py"), "python");
        assert_eq!(guess_language("baz.tsx"), "tsx");
        assert_eq!(guess_language("Makefile"), "unknown");
    }
}
