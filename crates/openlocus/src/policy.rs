use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;

const POLICY_PATH: &str = "openlocus.toml";
const MAX_CONFIGURED_FILE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    #[serde(default = "default_include")]
    pub include: Vec<String>,
    #[serde(default = "default_exclude")]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub include_gitignored: bool,
    #[serde(default = "default_max_file_bytes")]
    pub max_file_bytes: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            include: default_include(),
            exclude: default_exclude(),
            include_gitignored: false,
            max_file_bytes: default_max_file_bytes(),
        }
    }
}

impl Policy {
    pub fn load(source_root: &Path) -> Result<Self> {
        let path = source_root.join(POLICY_PATH);
        if !path.exists() {
            return Ok(Self::default());
        }
        if std::fs::symlink_metadata(&path)?.file_type().is_symlink() {
            bail!(
                "policy file must not be a symbolic link: {}",
                path.display()
            );
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let policy: Self =
            toml::from_str(&text).with_context(|| format!("invalid {}", path.display()))?;
        if !(1..=MAX_CONFIGURED_FILE_BYTES).contains(&policy.max_file_bytes) {
            bail!("max_file_bytes must be between 1 and {MAX_CONFIGURED_FILE_BYTES}");
        }
        Ok(policy)
    }

    pub(crate) fn hash(&self) -> Result<String> {
        let canonical = toml::to_string(self).context("failed to serialize policy")?;
        Ok(blake3::hash(canonical.as_bytes()).to_hex().to_string())
    }

    pub(crate) fn allows(&self, path: &str) -> bool {
        let path = path.replace('\\', "/");
        let included = self.include.is_empty()
            || self
                .include
                .iter()
                .any(|pattern| glob_match(pattern, &path));
        included
            && !self
                .exclude
                .iter()
                .any(|pattern| glob_match(pattern, &path))
    }
}

fn default_include() -> Vec<String> {
    vec!["**/*".into()]
}

fn default_exclude() -> Vec<String> {
    vec![
        ".git/**".into(),
        ".openlocus/**".into(),
        "target/**".into(),
        "node_modules/**".into(),
        "dist/**".into(),
        ".env*".into(),
        "**/*.pem".into(),
    ]
}

fn default_max_file_bytes() -> u64 {
    2 * 1024 * 1024
}

fn glob_match(pattern: &str, path: &str) -> bool {
    let pattern = pattern.replace('\\', "/");
    if pattern == "**" || pattern == "**/*" {
        return true;
    }
    if let Some(dir) = pattern.strip_suffix("/**") {
        return path == dir || path.starts_with(&format!("{dir}/"));
    }
    if let Some(extension) = pattern.strip_prefix("**/*.") {
        return path.ends_with(&format!(".{extension}"));
    }
    if let Some(extension) = pattern.strip_prefix("*.") {
        return path
            .rsplit('/')
            .next()
            .is_some_and(|name| name.ends_with(&format!(".{extension}")));
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return path
            .rsplit('/')
            .next()
            .is_some_and(|name| name.starts_with(prefix));
    }
    path == pattern
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_policy_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(POLICY_PATH), "include = [this is not toml").unwrap();
        assert!(Policy::load(dir.path()).is_err());
    }

    #[test]
    fn unsafe_file_size_limit_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(POLICY_PATH), "max_file_bytes = 0\n").unwrap();
        assert!(Policy::load(dir.path()).is_err());
    }

    #[test]
    fn default_policy_excludes_sensitive_and_generated_paths() {
        let policy = Policy::default();
        assert!(policy.allows("src/lib.rs"));
        assert!(!policy.allows(".env.local"));
        assert!(!policy.allows("secrets/key.pem"));
        assert!(!policy.allows("target/debug/app"));
    }
}
