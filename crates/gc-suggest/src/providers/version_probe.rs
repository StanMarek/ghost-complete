use std::collections::HashMap;
use std::path::Path;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use crate::providers::util::spawn_with_timeout;

const VERSION_PROBE_TIMEOUT: Duration = Duration::from_millis(500);

static VERSION_CACHE: LazyLock<Mutex<HashMap<String, Option<Version>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl Version {
    pub(crate) const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

pub(crate) fn parse_min_version(s: &str) -> Option<Version> {
    parse_version(s)
}

pub(crate) fn parse_version(output: &str) -> Option<Version> {
    for token in output.split(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-')) {
        let token = token
            .trim_start_matches(['v', 'V'])
            .trim_matches(|c: char| c == '.' || c == '-');
        if token.is_empty() {
            continue;
        }
        let numeric_prefix = token.split('-').next().unwrap_or(token).trim_matches('.');
        let parts: Vec<&str> = numeric_prefix.split('.').collect();
        if parts.is_empty() || parts.len() > 3 {
            continue;
        }
        let Ok(major) = parts[0].parse::<u64>() else {
            continue;
        };
        let minor = parts
            .get(1)
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        let patch = parts
            .get(2)
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        return Some(Version::new(major, minor, patch));
    }

    None
}

#[allow(dead_code)]
pub(crate) async fn probe_version(binary: &str, min_version: &str) -> Option<bool> {
    probe_version_with_args(binary, ["--version"], min_version).await
}

pub(crate) async fn probe_version_with_args<I, S>(
    binary: &str,
    args: I,
    min_version: &str,
) -> Option<bool>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let min = parse_min_version(min_version)?;
    let args_vec: Vec<String> = args
        .into_iter()
        .map(|arg| arg.as_ref().to_string_lossy().into_owned())
        .collect();
    let cache_key = format!("{binary}\0{}", args_vec.join("\0"));

    if let Some(cached) = match VERSION_CACHE.lock() {
        Ok(g) => g.get(&cache_key).copied(),
        Err(poisoned) => poisoned.into_inner().get(&cache_key).copied(),
    } {
        return cached.map(|version| version >= min);
    }

    let version = match spawn_with_timeout(
        Path::new("/tmp"),
        binary,
        args_vec.iter().map(String::as_str),
        None,
        VERSION_PROBE_TIMEOUT,
    )
    .await
    {
        Ok(stdout) => parse_version(&stdout),
        Err(e) => {
            tracing::warn!(binary, error = %e, "provider version probe failed");
            None
        }
    };

    match VERSION_CACHE.lock() {
        Ok(mut g) => {
            g.insert(cache_key, version);
        }
        Err(poisoned) => {
            poisoned.into_inner().insert(cache_key, version);
        }
    }

    version.map(|version| version >= min)
}

#[cfg(test)]
mod tests {
    use super::Version;

    #[test]
    fn parse_version_finds_semver_like_token() {
        assert_eq!(
            super::parse_version("Client Version: v1.29.3\n"),
            Some(Version::new(1, 29, 3))
        );
        assert_eq!(
            super::parse_version("Homebrew 4.2.10"),
            Some(Version::new(4, 2, 10))
        );
    }

    #[test]
    fn parse_version_handles_major_only_systemd_output() {
        assert_eq!(
            super::parse_version("systemd 255 (255.5-1-arch)\n"),
            Some(Version::new(255, 0, 0))
        );
    }

    #[test]
    fn parse_min_version_accepts_major_minor() {
        assert_eq!(
            super::parse_min_version("1.11"),
            Some(Version::new(1, 11, 0))
        );
        assert_eq!(
            super::parse_min_version("247"),
            Some(Version::new(247, 0, 0))
        );
    }
}
