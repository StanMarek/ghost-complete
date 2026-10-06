//! Conservative, generic completion discovery from a command's help output.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use anyhow::{Context, Result};
use gc_buffer::CommandContext;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

use crate::{Suggestion, SuggestionKind, SuggestionSource};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    resolution: ResolutionKey,
    path: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ResolutionKey {
    Found {
        executable: PathBuf,
        modified: Option<SystemTime>,
        len: Option<u64>,
    },
    Missing {
        command: String,
        path: Option<String>,
        /// Directory mtimes invalidate a miss when an executable is installed
        /// into an existing PATH entry without the PATH string changing.
        directories: Vec<(PathBuf, Option<SystemTime>)>,
    },
}

#[derive(Debug, Clone)]
enum Cached {
    Success(Arc<Vec<Suggestion>>),
    Failed,
}

#[derive(Debug, Default)]
pub struct HelpIntrospector {
    cache: Mutex<HashMap<CacheKey, Cached>>,
}

impl HelpIntrospector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return a generated node synchronously. `Some(empty)` is a negatively
    /// cached lookup; `None` is a genuine miss that may schedule introspection.
    pub fn cached_suggestions(
        &self,
        ctx: &CommandContext,
        shell_env: Option<&HashMap<String, String>>,
    ) -> Option<Vec<Suggestion>> {
        let key = cache_key(ctx, shell_env)?;
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .map(|hit| match hit {
                Cached::Success(v) => (**v).clone(),
                Cached::Failed => Vec::new(),
            })
    }

    pub async fn suggestions(
        &self,
        ctx: &CommandContext,
        cwd: &Path,
        timeout_ms: u64,
        shell_env: Option<Arc<HashMap<String, String>>>,
    ) -> Result<Vec<Suggestion>> {
        let key = cache_key(ctx, shell_env.as_deref()).context("missing command")?;
        if let Some(hit) = self
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            return match hit {
                Cached::Success(v) => Ok((**v).clone()),
                Cached::Failed => Ok(Vec::new()),
            };
        }

        let executable = match &key.resolution {
            ResolutionKey::Found { executable, .. } => executable.clone(),
            ResolutionKey::Missing { .. } => {
                self.cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(key, Cached::Failed);
                anyhow::bail!("command not found on shell PATH")
            }
        };

        let result = run_help(
            &executable,
            &key.path,
            cwd,
            timeout_ms,
            shell_env.as_deref(),
        )
        .await
        .map(|text| parse_help(&text));
        match result {
            Ok(output) if !output.is_empty() => {
                self.cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(key, Cached::Success(Arc::new(output.clone())));
                Ok(output)
            }
            // An executable that produced no parseable help may produce useful
            // output later (or for a corrected invocation). Do not turn that
            // transient outcome into a generated spec or a negative cache hit.
            Ok(output) => Ok(output),
            // Resolution misses are negatively cached above. Execution and
            // parsing failures remain retryable, especially for ask mode.
            Err(error) => Err(error),
        }
    }
}

async fn run_help(
    executable: &Path,
    path: &[String],
    cwd: &Path,
    timeout_ms: u64,
    shell_env: Option<&HashMap<String, String>>,
) -> Result<String> {
    // Prefer the ubiquitous `subcommand --help`; try `help subcommand` only
    // when it did not produce usable output. No shell is involved.
    let mut attempts = Vec::with_capacity(2);
    let mut first = path.to_vec();
    first.push("--help".into());
    attempts.push(first);
    let mut second = vec!["help".to_string()];
    second.extend_from_slice(path);
    attempts.push(second);
    for args in attempts {
        let mut child = Command::new(executable);
        child.args(&args).current_dir(cwd).kill_on_drop(true);
        if let Some(env) = shell_env {
            child.env_clear().envs(env);
        }
        child.stdin(std::process::Stdio::null());
        child.stdout(std::process::Stdio::piped());
        child.stderr(std::process::Stdio::piped());
        // These streams prevent ordinary CLI I/O from reaching the terminal.
        // Fully revoking an inherited controlling terminal (and therefore an
        // explicit open("/dev/tty")) requires Unix `setsid`/Windows job-object
        // infrastructure that the shared process runner does not currently
        // provide. Keep that stronger isolation as a process-abstraction
        // follow-up rather than adding an introspection-only unsafe pre_exec.
        let Ok(spawned) = child.spawn() else { continue };
        let Ok(output) = timeout(
            Duration::from_millis(timeout_ms.max(1)),
            spawned.wait_with_output(),
        )
        .await
        else {
            continue;
        };
        let Ok(output) = output else { continue };
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        if !output.stderr.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&String::from_utf8_lossy(&output.stderr));
        }
        if !text.trim().is_empty() && (output.status.success() || looks_like_help(&text)) {
            return Ok(text);
        }
    }
    anyhow::bail!("command did not produce help output")
}

fn looks_like_help(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.contains("usage:") || lower.contains("options:") || lower.contains("commands:")
}

/// Parse common clap/cobra/click-style help tables. Deliberately ignores
/// prose that cannot be recognized with high confidence.
pub fn parse_help(help: &str) -> Vec<Suggestion> {
    let mut out = Vec::new();
    let mut section = "";
    for raw in help.lines() {
        let line = raw.trim();
        let heading = line.trim_end_matches(':').to_ascii_lowercase();
        if matches!(
            heading.as_str(),
            "commands" | "subcommands" | "available commands"
        ) {
            section = "commands";
            continue;
        }
        if matches!(heading.as_str(), "options" | "flags" | "global options") {
            section = "options";
            continue;
        }
        if line.is_empty() {
            continue;
        }
        let Some((left, desc)) = split_columns(line) else {
            continue;
        };
        if section == "options" || left.starts_with('-') {
            let names = left
                .split(',')
                .map(str::trim)
                .filter_map(|part| part.split_whitespace().next())
                .filter(|n| n.starts_with('-') && n.len() > 1)
                .collect::<Vec<_>>();
            for name in names {
                push_unique(&mut out, name, desc, SuggestionKind::Flag);
            }
        } else if section == "commands" {
            let name = left.split_whitespace().next().unwrap_or("");
            if valid_command_name(name) {
                push_unique(&mut out, name, desc, SuggestionKind::Subcommand);
            }
        }
    }
    out
}

fn split_columns(line: &str) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i].is_ascii_whitespace() && bytes[i + 1].is_ascii_whitespace() {
            let left = line[..i].trim();
            let right = line[i..].trim();
            if !left.is_empty() && !right.is_empty() {
                return Some((left, right));
            }
        }
        i += 1;
    }
    None
}

fn valid_command_name(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'))
}

fn push_unique(out: &mut Vec<Suggestion>, text: &str, desc: &str, kind: SuggestionKind) {
    if out.iter().any(|s| s.text == text) {
        return;
    }
    out.push(Suggestion {
        text: text.to_string(),
        description: Some(desc.to_string()),
        kind,
        source: SuggestionSource::Introspection,
        ..Default::default()
    });
}

fn cache_key(
    ctx: &CommandContext,
    shell_env: Option<&HashMap<String, String>>,
) -> Option<CacheKey> {
    let command = ctx.command.as_deref()?;
    let path = ctx
        .args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .cloned()
        .collect::<Vec<_>>();
    let path_value = shell_env
        .and_then(|env| env.get("PATH").cloned())
        .or_else(|| std::env::var("PATH").ok());
    let executable = resolve_executable(command, path_value.as_deref());
    let resolution = if let Some(executable) = executable {
        let metadata = std::fs::metadata(&executable).ok();
        ResolutionKey::Found {
            executable,
            modified: metadata.as_ref().and_then(|m| m.modified().ok()),
            len: metadata.as_ref().map(std::fs::Metadata::len),
        }
    } else {
        let directories = path_value
            .as_deref()
            .map(std::ffi::OsStr::new)
            .map(std::env::split_paths)
            .into_iter()
            .flatten()
            .map(|directory| {
                let modified = std::fs::metadata(&directory)
                    .ok()
                    .and_then(|metadata| metadata.modified().ok());
                (directory, modified)
            })
            .collect();
        ResolutionKey::Missing {
            command: command.to_string(),
            path: path_value,
            directories,
        }
    };
    Some(CacheKey { resolution, path })
}

fn resolve_executable(command: &str, path_value: Option<&str>) -> Option<PathBuf> {
    if command.contains(std::path::MAIN_SEPARATOR) {
        let p = PathBuf::from(command);
        return p.is_file().then_some(p);
    }
    path_value.and_then(|path| {
        std::env::split_paths(std::ffi::OsStr::new(path))
            .map(|d| d.join(command))
            .find(|p| p.is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gc_buffer::parse_command_context;
    #[test]
    fn parses_common_commands_and_options() {
        let got = parse_help("Commands:\n  serve       Run server\n  deploy      Ship it\n\nOptions:\n  -v, --verbose  More output\n  --config FILE  Config path\n");
        assert!(got
            .iter()
            .any(|s| s.text == "serve" && s.kind == SuggestionKind::Subcommand));
        assert!(got
            .iter()
            .any(|s| s.text == "-v" && s.kind == SuggestionKind::Flag));
        assert!(got
            .iter()
            .any(|s| s.text == "--config" && s.kind == SuggestionKind::Flag));
    }

    #[cfg(unix)]
    fn fake_cli(body: &str) -> (tempfile::TempDir, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake-cli");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        (dir, path)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn caches_success_and_lazily_queries_nested_path() {
        let (dir, cli) = fake_cli(
            r#"echo x >> "$0.count"
if [ "$1" = "use" ]; then
  printf 'Options:\n  --global  Install globally\n'
else
  printf 'Commands:\n  use  Install a tool\n'
fi"#,
        );
        let introspector = HelpIntrospector::new();
        let root = parse_command_context(
            &format!("{} ", cli.display()),
            cli.display().to_string().chars().count() + 1,
        );
        let first = introspector
            .suggestions(&root, dir.path(), 500, None)
            .await
            .unwrap();
        let second = introspector
            .suggestions(&root, dir.path(), 500, None)
            .await
            .unwrap();
        assert!(first.iter().any(|s| s.text == "use"));
        assert_eq!(first.len(), second.len());
        assert!(introspector
            .cached_suggestions(&root, None)
            .is_some_and(|cached| cached.iter().any(|s| s.text == "use")));
        assert_eq!(
            std::fs::read_to_string(format!("{}.count", cli.display()))
                .unwrap()
                .lines()
                .count(),
            1
        );

        let nested_buffer = format!("{} use ", cli.display());
        let nested = parse_command_context(&nested_buffer, nested_buffer.chars().count());
        let got = introspector
            .suggestions(&nested, dir.path(), 500, None)
            .await
            .unwrap();
        assert!(got.iter().any(|s| s.text == "--global"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn accepts_stderr_and_leaves_timeout_failure_retryable() {
        let (dir, cli) = fake_cli(
            r#"echo x >> "$0.count"
if [ "$1" = "slow" ] || [ "$2" = "slow" ]; then sleep 2; fi
printf 'Options:\n  --quiet  Be quiet\n' >&2"#,
        );
        let introspector = HelpIntrospector::new();
        let root_buffer = format!("{} ", cli.display());
        let root = parse_command_context(&root_buffer, root_buffer.chars().count());
        assert!(introspector
            .suggestions(&root, dir.path(), 500, None)
            .await
            .unwrap()
            .iter()
            .any(|s| s.text == "--quiet"));

        let slow_buffer = format!("{} slow ", cli.display());
        let slow = parse_command_context(&slow_buffer, slow_buffer.chars().count());
        assert!(introspector
            .suggestions(&slow, dir.path(), 10, None)
            .await
            .is_err());
        assert!(introspector
            .suggestions(&slow, dir.path(), 10, None)
            .await
            .is_err());
        // Root once, then both safe help forms for each retryable slow attempt.
        assert_eq!(
            std::fs::read_to_string(format!("{}.count", cli.display()))
                .unwrap()
                .lines()
                .count(),
            5
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn passes_metacharacters_as_literal_argv_without_a_shell() {
        let (dir, cli) = fake_cli("printf 'Options:\\n  --safe  Safe\\n'");
        let marker = dir.path().join("must-not-exist");
        let buffer = format!("{} '$(touch {})' ", cli.display(), marker.display());
        let ctx = parse_command_context(&buffer, buffer.chars().count());
        let got = HelpIntrospector::new()
            .suggestions(&ctx, dir.path(), 500, None)
            .await
            .unwrap();
        assert!(got.iter().any(|s| s.text == "--safe"));
        assert!(!marker.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn uses_shell_path_and_environment_instead_of_proxy_environment() {
        let (dir, cli) = fake_cli(
            r#"if [ "$GC_INTROSPECTION_PROBE" = "from-shell" ]; then
  printf 'Options:\n  --from-shell  Shell environment used\n'
fi"#,
        );
        let buffer = "fake-cli ";
        let ctx = parse_command_context(buffer, buffer.chars().count());
        let env = Arc::new(HashMap::from([
            ("PATH".to_string(), dir.path().display().to_string()),
            (
                "GC_INTROSPECTION_PROBE".to_string(),
                "from-shell".to_string(),
            ),
        ]));
        let introspector = HelpIntrospector::new();
        let got = introspector
            .suggestions(&ctx, dir.path(), 500, Some(Arc::clone(&env)))
            .await
            .unwrap();
        assert!(got.iter().any(|s| s.text == "--from-shell"));
        assert!(introspector
            .cached_suggestions(&ctx, Some(env.as_ref()))
            .is_some());
        assert_eq!(cli.parent(), Some(dir.path()));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn path_resolution_failures_are_negative_cached_and_invalidated() {
        let missing_dir = tempfile::tempdir().unwrap();
        let buffer = "late-cli ";
        let ctx = parse_command_context(buffer, buffer.chars().count());
        let missing_env = Arc::new(HashMap::from([(
            "PATH".to_string(),
            missing_dir.path().display().to_string(),
        )]));
        let introspector = HelpIntrospector::new();
        assert!(introspector
            .suggestions(
                &ctx,
                missing_dir.path(),
                500,
                Some(Arc::clone(&missing_env))
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("shell PATH"));
        assert!(
            introspector
                .cached_suggestions(&ctx, Some(missing_env.as_ref()))
                .is_some_and(|cached| cached.is_empty()),
            "the same PATH and directory state must be a synchronous negative hit"
        );

        // Installing into the same PATH directory changes its metadata and
        // must invalidate the negative entry even though PATH is unchanged.
        use std::os::unix::fs::PermissionsExt;
        let installed = missing_dir.path().join("late-cli");
        std::fs::write(
            &installed,
            "#!/bin/sh\nprintf 'Options:\\n  --installed  Found later\\n'\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&installed).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&installed, permissions).unwrap();
        assert!(introspector
            .cached_suggestions(&ctx, Some(missing_env.as_ref()))
            .is_none());
        let got = introspector
            .suggestions(
                &ctx,
                missing_dir.path(),
                500,
                Some(Arc::clone(&missing_env)),
            )
            .await
            .unwrap();
        assert!(got.iter().any(|s| s.text == "--installed"));
    }
}
