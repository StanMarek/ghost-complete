//! Conservative, generic completion discovery from a command's help output.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use anyhow::{anyhow, Context, Result};
use gc_buffer::CommandContext;
use gc_jsrt::ShellRunError;

use crate::script::{run_script_full_with_env, MAX_GENERATOR_STDOUT_BYTES};
use crate::{Suggestion, SuggestionKind, SuggestionSource};

const MAX_CACHE_ENTRIES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LogicalKey {
    command: String,
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

#[derive(Debug)]
struct CacheEntry {
    resolution: ResolutionKey,
    value: Cached,
}

#[derive(Debug, Default)]
struct CacheState {
    entries: HashMap<LogicalKey, CacheEntry>,
    insertion_order: VecDeque<LogicalKey>,
}

impl CacheState {
    fn get(&self, key: &LogicalKey, resolution: &ResolutionKey) -> Option<&Cached> {
        self.entries
            .get(key)
            .filter(|entry| &entry.resolution == resolution)
            .map(|entry| &entry.value)
    }

    fn insert(&mut self, key: LogicalKey, resolution: ResolutionKey, value: Cached) {
        if let std::collections::hash_map::Entry::Occupied(mut entry) =
            self.entries.entry(key.clone())
        {
            entry.insert(CacheEntry { resolution, value });
            return;
        }
        while self.entries.len() >= MAX_CACHE_ENTRIES {
            let Some(oldest) = self.insertion_order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
        self.insertion_order.push_back(key.clone());
        self.entries.insert(key, CacheEntry { resolution, value });
    }
}

#[derive(Debug)]
struct Lookup {
    key: LogicalKey,
    resolution: ResolutionKey,
    executable: Option<PathBuf>,
}

#[derive(Debug, Default)]
pub struct HelpIntrospector {
    cache: Mutex<CacheState>,
    #[cfg(test)]
    lookup_count: std::sync::atomic::AtomicUsize,
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
        cwd: &Path,
        shell_env: Option<&HashMap<String, String>>,
    ) -> Option<Vec<Suggestion>> {
        #[cfg(test)]
        self.lookup_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let lookup = lookup(ctx, cwd, shell_env, &cache)?;
        cache
            .get(&lookup.key, &lookup.resolution)
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
        let lookup = {
            let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            lookup(ctx, cwd, shell_env.as_deref(), &cache).context("missing command")?
        };
        if let Some(hit) = self
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&lookup.key, &lookup.resolution)
        {
            return match hit {
                Cached::Success(v) => Ok((**v).clone()),
                Cached::Failed => Ok(Vec::new()),
            };
        }

        let executable = match lookup.executable.clone() {
            Some(executable) => executable,
            None => {
                self.cache.lock().unwrap_or_else(|e| e.into_inner()).insert(
                    lookup.key,
                    lookup.resolution,
                    Cached::Failed,
                );
                anyhow::bail!("command not found on shell PATH")
            }
        };

        let result = run_help(
            &executable,
            &lookup.key.path,
            cwd,
            timeout_ms,
            shell_env.as_deref(),
        )
        .await
        .map(|text| parse_help(&text));
        match result {
            Ok(output) if !output.is_empty() => {
                self.cache.lock().unwrap_or_else(|e| e.into_inner()).insert(
                    lookup.key,
                    lookup.resolution,
                    Cached::Success(Arc::new(output.clone())),
                );
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

    #[cfg(test)]
    pub(crate) fn lookup_count(&self) -> usize {
        self.lookup_count.load(std::sync::atomic::Ordering::Relaxed)
    }

    #[cfg(test)]
    fn cache_len(&self) -> usize {
        self.cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entries
            .len()
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
    let executable = executable
        .to_str()
        .context("resolved help executable path is not valid UTF-8")?
        .to_string();
    let mut failures = Vec::with_capacity(2);
    for args in attempts {
        let mut argv = Vec::with_capacity(args.len() + 1);
        argv.push(executable.as_str());
        argv.extend(args.iter().map(String::as_str));
        match run_script_full_with_env(&argv, cwd, timeout_ms.max(1), shell_env).await {
            Ok(output) => {
                if output.exit_code.is_none()
                    || output.stdout.len() >= MAX_GENERATOR_STDOUT_BYTES
                    || output.stderr.len() >= MAX_GENERATOR_STDOUT_BYTES
                {
                    failures.push(format!(
                        "{}: help output exceeded the capture limit",
                        args.join(" ")
                    ));
                    continue;
                }
                let text = combined_output(&output.stdout, &output.stderr);
                if !parse_help(&text).is_empty() {
                    return Ok(text);
                }
                failures.push(format!("{}: produced no parseable help", args.join(" ")));
            }
            Err(ShellRunError::NonZeroExit {
                exit_code,
                stdout,
                stderr,
            }) => {
                let text = combined_output(&stdout, &stderr);
                if !parse_help(&text).is_empty() {
                    return Ok(text);
                }
                failures.push(format!(
                    "{}: exited with status {} without parseable help",
                    args.join(" "),
                    exit_code
                        .map(|code| code.to_string())
                        .unwrap_or_else(|| "signal".to_string())
                ));
            }
            Err(ShellRunError::Timeout) => failures.push(format!(
                "{}: timed out after {}ms",
                args.join(" "),
                timeout_ms.max(1)
            )),
            Err(error) => failures.push(format!("{}: {error}", args.join(" "))),
        }
    }
    Err(anyhow!(
        "help introspection failed: {}",
        failures.join("; ")
    ))
}

fn combined_output(stdout: &str, stderr: &str) -> String {
    match (stdout.is_empty(), stderr.is_empty()) {
        (_, true) => stdout.to_string(),
        (true, false) => stderr.to_string(),
        (false, false) => format!("{stdout}\n{stderr}"),
    }
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

fn lookup(
    ctx: &CommandContext,
    cwd: &Path,
    shell_env: Option<&HashMap<String, String>>,
    cache: &CacheState,
) -> Option<Lookup> {
    let command = ctx.command.as_deref()?.to_string();
    let path_value = shell_env
        .and_then(|env| env.get("PATH").cloned())
        .or_else(|| std::env::var("PATH").ok());
    let executable = resolve_executable(&command, path_value.as_deref(), cwd);
    let resolution = if let Some(executable) = executable.as_ref() {
        let metadata = std::fs::metadata(executable).ok();
        ResolutionKey::Found {
            executable: executable.clone(),
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
                let directory = if directory.as_os_str().is_empty() || directory.is_relative() {
                    cwd.join(directory)
                } else {
                    directory
                };
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

    // Only descend through tokens that a successfully generated parent node
    // explicitly identified as subcommands. Ordinary positional arguments are
    // never guessed to be command-path components.
    let mut path = Vec::new();
    for arg in ctx.args.iter().filter(|arg| !arg.starts_with('-')) {
        let parent = LogicalKey {
            command: command.clone(),
            path: path.clone(),
        };
        let Some(Cached::Success(suggestions)) = cache.get(&parent, &resolution) else {
            break;
        };
        if suggestions.iter().any(|suggestion| {
            suggestion.kind == SuggestionKind::Subcommand && suggestion.text == *arg
        }) {
            path.push(arg.clone());
        } else {
            break;
        }
    }
    Some(Lookup {
        key: LogicalKey { command, path },
        resolution,
        executable,
    })
}

fn resolve_executable(command: &str, path_value: Option<&str>, cwd: &Path) -> Option<PathBuf> {
    if command.contains(std::path::MAIN_SEPARATOR) {
        let p = PathBuf::from(command);
        let resolved = if p.is_absolute() { p } else { cwd.join(p) };
        return is_executable(&resolved).then_some(resolved);
    }
    path_value.and_then(|path| {
        std::env::split_paths(std::ffi::OsStr::new(path))
            .map(|directory| {
                let directory = if directory.as_os_str().is_empty() || directory.is_relative() {
                    cwd.join(directory)
                } else {
                    directory
                };
                directory.join(command)
            })
            .find(|candidate| is_executable(candidate))
    })
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
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
            .cached_suggestions(&root, dir.path(), None)
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
    async fn ordinary_positionals_reuse_parent_instead_of_triggering_lazy_descent() {
        let (dir, cli) = fake_cli(
            r#"echo x >> "$0.count"
printf 'Commands:\n  real-subcommand  A real child\nOptions:\n  --config FILE  Config\n'"#,
        );
        let introspector = HelpIntrospector::new();
        let root_buffer = format!("{} ", cli.display());
        let root = parse_command_context(&root_buffer, root_buffer.chars().count());
        introspector
            .suggestions(&root, dir.path(), 500, None)
            .await
            .unwrap();

        for buffer in [
            format!("{} pattern ", cli.display()),
            format!("{} --config value ", cli.display()),
        ] {
            let ctx = parse_command_context(&buffer, buffer.chars().count());
            let cached = introspector
                .cached_suggestions(&ctx, dir.path(), None)
                .expect("an ordinary positional should resolve to the generated parent node");
            assert!(cached.iter().any(|item| item.text == "real-subcommand"));
        }
        assert_eq!(
            std::fs::read_to_string(format!("{}.count", cli.display()))
                .unwrap()
                .lines()
                .count(),
            1,
            "ordinary positional values must not launch more help processes"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn accepts_stderr_and_leaves_timeout_failure_retryable() {
        let (dir, cli) = fake_cli(
            r#"echo x >> "$0.count"
if [ "$1" = "slow" ] || [ "$2" = "slow" ]; then sleep 2; fi
printf 'Commands:\n  slow  Slow child\nOptions:\n  --quiet  Be quiet\n' >&2"#,
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
        let first_error = introspector
            .suggestions(&slow, dir.path(), 10, None)
            .await
            .unwrap_err();
        assert!(first_error.to_string().contains("timed out after 10ms"));
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
            .cached_suggestions(&ctx, dir.path(), Some(env.as_ref()))
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
                .cached_suggestions(&ctx, missing_dir.path(), Some(missing_env.as_ref()))
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
            .cached_suggestions(&ctx, missing_dir.path(), Some(missing_env.as_ref()))
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
        assert_eq!(
            introspector.cache_len(),
            1,
            "resolution invalidation must replace the stable logical entry"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cache_is_bounded_under_many_resolution_misses() {
        let dir = tempfile::tempdir().unwrap();
        let env = Arc::new(HashMap::from([(
            "PATH".to_string(),
            dir.path().display().to_string(),
        )]));
        let introspector = HelpIntrospector::new();
        for index in 0..(MAX_CACHE_ENTRIES + 64) {
            let buffer = format!("missing-command-{index} ");
            let ctx = parse_command_context(&buffer, buffer.chars().count());
            let _ = introspector
                .suggestions(&ctx, dir.path(), 50, Some(Arc::clone(&env)))
                .await;
        }
        assert_eq!(introspector.cache_len(), MAX_CACHE_ENTRIES);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn resolution_uses_shell_cwd_and_skips_non_executable_shadow() {
        use std::os::unix::fs::PermissionsExt;

        let cwd = tempfile::tempdir().unwrap();
        let shadow_dir = cwd.path().join("shadow");
        let executable_dir = cwd.path().join("bin");
        std::fs::create_dir_all(&shadow_dir).unwrap();
        std::fs::create_dir_all(&executable_dir).unwrap();
        std::fs::write(shadow_dir.join("probe"), "not executable").unwrap();
        let executable = executable_dir.join("probe");
        std::fs::write(
            &executable,
            "#!/bin/sh\nprintf 'Options:\\n  --resolved  Resolved\\n'\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&executable, permissions).unwrap();

        let relative_path = std::env::join_paths([Path::new("shadow"), Path::new("bin")])
            .unwrap()
            .into_string()
            .unwrap();
        let env = Arc::new(HashMap::from([("PATH".to_string(), relative_path)]));
        let ctx = parse_command_context("probe ", 6);
        let got = HelpIntrospector::new()
            .suggestions(&ctx, cwd.path(), 500, Some(env))
            .await
            .unwrap();
        assert!(got.iter().any(|item| item.text == "--resolved"));

        let local = cwd.path().join("local-probe");
        std::fs::write(
            &local,
            "#!/bin/sh\nprintf 'Options:\\n  --local  Local\\n'\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&local).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&local, permissions).unwrap();
        for (buffer, path) in [("local-probe ", ""), ("./local-probe ", "/bin")] {
            let ctx = parse_command_context(buffer, buffer.chars().count());
            let env = Arc::new(HashMap::from([("PATH".to_string(), path.to_string())]));
            let got = HelpIntrospector::new()
                .suggestions(&ctx, cwd.path(), 500, Some(env))
                .await
                .unwrap();
            assert!(got.iter().any(|item| item.text == "--local"));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn excessive_help_output_fails_with_a_specific_diagnostic() {
        let (dir, cli) =
            fake_cli("head -c 1100000 /dev/zero\nprintf 'Options:\\n  --too-late  Too late\\n'");
        let buffer = format!("{} ", cli.display());
        let ctx = parse_command_context(&buffer, buffer.chars().count());
        let error = HelpIntrospector::new()
            .suggestions(&ctx, dir.path(), 2_000, None)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("exceeded the capture limit"));
    }
}
