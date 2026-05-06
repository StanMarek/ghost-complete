mod config_cmd;
mod doctor;
mod import_history;
mod install;
mod sanitize;
mod status;
mod tui;
mod validate;

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "ghost-complete",
    version = concat!(
        env!("CARGO_PKG_VERSION"),
        " (",
        env!("VERGEN_GIT_SHA"),
        " ",
        env!("VERGEN_BUILD_TIMESTAMP"),
        ")"
    ),
    about = "Terminal-native autocomplete engine",
    after_help = "COMMANDS:\n  install          Install shell integration (zsh)\n  uninstall        Remove shell integration\n  validate-specs   Validate completion spec files\n  status           Show loaded specs and JS compatibility\n  config           Show resolved configuration\n  config edit      Open interactive config editor\n  doctor           Run health checks\n  import-history   Seed frecency scores from $HISTFILE / ~/.zsh_history\n\nSHELL SUPPORT:\n  zsh   Full support (auto-installed into ~/.zshrc)"
)]
struct Cli {
    /// Path to config file
    #[arg(long)]
    config: Option<String>,

    /// Log level (trace, debug, info, warn, error)
    #[arg(long, default_value = "warn")]
    log_level: String,

    /// Log to file instead of stderr
    #[arg(long)]
    log_file: Option<String>,

    /// Shell command and arguments (default: $SHELL or /bin/zsh)
    #[arg(trailing_var_arg = true)]
    shell_args: Vec<String>,
}

fn default_log_file() -> Option<String> {
    let state_dir = dirs::state_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/state")))
        .map(|d| d.join("ghost-complete"));
    let dir = state_dir?;
    // Use eprintln! rather than tracing because init_tracing has not
    // been called yet at this point — we're computing its log file path.
    // Returning None here falls back to stderr logging, which is strictly
    // better than silently continuing with a nonexistent log file and
    // then failing to open it a few milliseconds later.
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "ghost-complete: could not create log directory {}: {e} — falling back to stderr",
            sanitize::sanitize_path(&dir)
        );
        return None;
    }
    Some(
        dir.join("ghost-complete.log")
            .to_string_lossy()
            .into_owned(),
    )
}

/// Default fallback shell when `$SHELL` is unset, empty, or unreadable.
const DEFAULT_FALLBACK_SHELL: &str = "/bin/zsh";

/// Resolve the default shell from `$SHELL`, falling back to [`DEFAULT_FALLBACK_SHELL`].
///
/// `env::var("SHELL")` returns `Ok("")` when the variable is set but empty —
/// passing that straight to the PTY spawn produces an opaque `ENOENT` and a
/// confused user. Treat empty as missing so the fallback applies.
fn resolve_default_shell() -> String {
    resolve_default_shell_from(|name| std::env::var(name).ok())
}

/// Pure helper used by [`resolve_default_shell`]; takes an env-lookup closure
/// so the resolution rules can be unit-tested without touching process state.
fn resolve_default_shell_from<F>(lookup: F) -> String
where
    F: Fn(&str) -> Option<String>,
{
    lookup("SHELL")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_FALLBACK_SHELL.to_string())
}

/// Parse `--baseline <path>` (or `--baseline=PATH`) out of the trailing
/// arg list `shell_args`. A bare `--baseline` with no following value —
/// or a `--baseline` whose next token starts with `-` — is a user
/// error, not a silent fallback to the embedded baseline; the latter
/// would mask typos like `ghost-complete status --baseline --json`.
fn parse_baseline_flag(shell_args: &[String]) -> Result<Option<std::path::PathBuf>> {
    parse_value_flag(shell_args, "--baseline")
        .map_err(|_| anyhow::anyhow!("--baseline requires a path argument"))
        .map(|opt| opt.map(std::path::PathBuf::from))
}

/// Extract a `--<name> VALUE` or `--<name>=VALUE` flag from `args`.
/// Returns the last occurrence so `--from a --from b` resolves to `b`.
/// Errors on a bare `--<name>` with no value or one followed by another flag.
fn parse_value_flag(args: &[String], name: &str) -> Result<Option<String>> {
    let with_eq = format!("{name}=");
    let mut out: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == name {
            match args.get(i + 1) {
                Some(v) if !v.starts_with('-') => {
                    out = Some(v.clone());
                    i += 2;
                    continue;
                }
                _ => anyhow::bail!("{name} requires a value"),
            }
        } else if let Some(rest) = a.strip_prefix(&with_eq) {
            if rest.is_empty() {
                anyhow::bail!("{name} requires a value");
            }
            out = Some(rest.to_string());
        }
        i += 1;
    }
    Ok(out)
}

fn init_tracing(level: &str, log_file: Option<&str>) -> Result<()> {
    // Prefer `RUST_LOG` (standard ecosystem env var) when set; fall back to
    // the `--log-level` flag value otherwise. This matches how every other
    // tracing/log-based Rust binary behaves and keeps `--log-level` as a
    // convenient default for users who don't want to export an env var.
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("warn")));

    if let Some(path) = log_file {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("failed to open log file: {}", path))?;

        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(file)
            .with_ansi(false)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .init();
    }

    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.shell_args.first().map(|s| s.as_str()) {
        Some("install") => {
            init_tracing(&cli.log_level, cli.log_file.as_deref())?;
            let dry_run = cli.shell_args.iter().any(|s| s == "--dry-run");
            return install::run_install(dry_run);
        }
        Some("uninstall") => {
            init_tracing(&cli.log_level, cli.log_file.as_deref())?;
            return install::run_uninstall();
        }
        Some("validate-specs") => {
            init_tracing(&cli.log_level, cli.log_file.as_deref())?;
            return validate::run_validate_specs(cli.config.as_deref());
        }
        Some("status") => {
            init_tracing(&cli.log_level, cli.log_file.as_deref())?;
            // Mirror `validate-specs --strict` / `install --dry-run`: the
            // top-level clap parser just collects a trailing arg list, so we
            // scan it ourselves for the status-specific flags.
            let strict = cli.shell_args.iter().any(|s| s == "--strict");
            let json = cli.shell_args.iter().any(|s| s == "--json");
            let baseline_path = parse_baseline_flag(&cli.shell_args)?;
            return status::run_status_with_opts(
                cli.config.as_deref(),
                strict,
                json,
                baseline_path.as_deref(),
            );
        }
        Some("config") => {
            if cli.shell_args.get(1).map(|s| s.as_str()) == Some("edit") {
                init_tracing(&cli.log_level, cli.log_file.as_deref())?;
                tui::run_config_editor(cli.config.as_deref())?;
                std::process::exit(0);
            }
            init_tracing(&cli.log_level, cli.log_file.as_deref())?;
            return config_cmd::run_config(cli.config.as_deref());
        }
        Some("doctor") => {
            init_tracing(&cli.log_level, cli.log_file.as_deref())?;
            return doctor::run_doctor(cli.config.as_deref());
        }
        Some("import-history") => {
            init_tracing(&cli.log_level, cli.log_file.as_deref())?;
            let dry_run = cli.shell_args.iter().any(|s| s == "--dry-run");
            let path_override = parse_value_flag(&cli.shell_args, "--from")?;
            let max_entries = parse_value_flag(&cli.shell_args, "--max")?
                .map(|s| s.parse::<usize>())
                .transpose()
                .context("--max requires a non-negative integer")?;
            return import_history::run_import_history(
                path_override.as_deref(),
                max_entries,
                dry_run,
            );
        }
        _ => {}
    }

    // Proxy mode — default to log file, never stderr
    let log_file = cli.log_file.or_else(default_log_file);
    init_tracing(&cli.log_level, log_file.as_deref())?;

    let (shell, args) = if cli.shell_args.is_empty() {
        (resolve_default_shell(), vec![])
    } else {
        let mut iter = cli.shell_args.into_iter();
        let shell = iter.next().unwrap();
        let args: Vec<String> = iter.collect();
        (shell, args)
    };

    let config =
        gc_config::GhostConfig::load(cli.config.as_deref()).context("failed to load config")?;

    tracing::info!(shell = %shell, "starting ghost-complete proxy");

    let rt = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    let exit_code = rt.block_on(gc_pty::run_proxy(&shell, &args, &config))?;

    std::process::exit(exit_code);
}

#[cfg(test)]
mod tests {
    use super::{parse_baseline_flag, resolve_default_shell_from, DEFAULT_FALLBACK_SHELL};

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn resolve_default_shell_uses_env_when_set() {
        let shell = resolve_default_shell_from(|name| {
            assert_eq!(name, "SHELL");
            Some("/usr/local/bin/fish".to_string())
        });
        assert_eq!(shell, "/usr/local/bin/fish");
    }

    #[test]
    fn resolve_default_shell_falls_back_when_unset() {
        let shell = resolve_default_shell_from(|_| None);
        assert_eq!(shell, DEFAULT_FALLBACK_SHELL);
    }

    #[test]
    fn resolve_default_shell_falls_back_when_empty() {
        // Regression: `env::var("SHELL")` returns `Ok("")` when SHELL is set
        // but empty. Without the empty filter, the PTY spawn fails with a
        // cryptic ENOENT instead of using the fallback.
        let shell = resolve_default_shell_from(|_| Some(String::new()));
        assert_eq!(shell, DEFAULT_FALLBACK_SHELL);
    }

    #[test]
    fn status_baseline_flag_with_value_parses() {
        let args = argv(&["status", "--baseline", "/tmp/b.json"]);
        let parsed = parse_baseline_flag(&args).unwrap();
        assert_eq!(parsed, Some(std::path::PathBuf::from("/tmp/b.json")));
    }

    #[test]
    fn status_baseline_equals_form_parses() {
        let args = argv(&["status", "--baseline=/tmp/b.json"]);
        let parsed = parse_baseline_flag(&args).unwrap();
        assert_eq!(parsed, Some(std::path::PathBuf::from("/tmp/b.json")));
    }

    #[test]
    fn status_baseline_flag_without_value_errors() {
        // Bare `--baseline` (no trailing value) — must produce a clear
        // error rather than silently falling back to the embedded
        // baseline, so typos like `ghost-complete status --baseline
        // --json` are caught at the flag boundary.
        let args = argv(&["status", "--baseline"]);
        let err = parse_baseline_flag(&args).unwrap_err();
        assert!(
            err.to_string()
                .contains("--baseline requires a path argument"),
            "expected clear error message, got: {err}"
        );

        // `--baseline` followed by another flag is equivalently bad:
        // the next token is consumed as a value today, which eats the
        // real flag. Forbid it.
        let args = argv(&["status", "--baseline", "--json"]);
        let err = parse_baseline_flag(&args).unwrap_err();
        assert!(err
            .to_string()
            .contains("--baseline requires a path argument"));

        // Empty `--baseline=` form — same contract.
        let args = argv(&["status", "--baseline="]);
        let err = parse_baseline_flag(&args).unwrap_err();
        assert!(err
            .to_string()
            .contains("--baseline requires a path argument"));
    }
}
