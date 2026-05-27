mod config_cmd;
mod doctor;
mod install;
mod sanitize;
mod status;
mod tui;
mod validate;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use tracing_subscriber::EnvFilter;

/// Closed set of `--log-level` values, validated by clap at parse time.
///
/// Before this enum existed, `--log-level deubg` would silently fall back
/// to `warn` inside `init_tracing`. Modeling the flag as a `ValueEnum`
/// lets clap reject typos at the parse boundary so the fallback path is
/// unreachable for `--log-level`; the `RUST_LOG` env-var path stays
/// free-form because `EnvFilter` syntax is richer than a single level.
#[derive(Debug, Clone, Copy, ValueEnum)]
#[value(rename_all = "lower")]
enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    fn as_filter_str(self) -> &'static str {
        match self {
            LogLevel::Trace => "trace",
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }
}

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
    after_help = "SHELL SUPPORT:\n  zsh   Full support (auto-installed into ~/.zshrc)\n\nWith no subcommand, ghost-complete starts in proxy mode wrapping $SHELL.\nTo wrap a specific shell, run e.g. `ghost-complete /bin/zsh -l`.\nIf your shell binary is named like a subcommand, prefix with `--`:\n  ghost-complete -- install --some-flag"
)]
struct Cli {
    /// Path to config file
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Log level (trace, debug, info, warn, error)
    #[arg(long, global = true, value_enum, default_value_t = LogLevel::Warn)]
    log_level: LogLevel,

    /// Log to file instead of stderr
    #[arg(long, global = true)]
    log_file: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Install shell integration (zsh)
    Install {
        /// Print what would be installed without writing files
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove shell integration
    Uninstall,
    /// Validate completion spec files
    #[command(name = "validate-specs")]
    ValidateSpecs {
        /// Treat warnings as failures
        #[arg(long)]
        strict: bool,
        /// Emit JSON output
        #[arg(long)]
        json: bool,
    },
    /// Show loaded specs and JS compatibility
    Status {
        /// Exit nonzero if any spec failed to parse or no runtime specs are available
        #[arg(long)]
        strict: bool,
        /// Emit JSON output
        #[arg(long)]
        json: bool,
        /// Override the embedded coverage baseline
        #[arg(long, value_name = "PATH")]
        baseline: Option<PathBuf>,
        /// Show the full per-command JS-only list (plain output is summary-only)
        #[arg(long)]
        verbose: bool,
    },
    /// Show or edit resolved configuration
    Config {
        /// `None` means "print resolved config to stdout"; `Some(Edit)` launches the interactive editor.
        #[command(subcommand)]
        subcommand: Option<ConfigCommand>,
    },
    /// Run health checks
    Doctor,
    // Catch-all for argv that doesn't match a named subcommand — clap routes
    // it here so we can dispatch to proxy mode without emitting an "unknown
    // subcommand" error. clap suppresses `///` docs on `external_subcommand`
    // variants from `--help`, so the user-facing description lives in the
    // top-level `after_help` block. `Vec<OsString>` preserves non-UTF-8 argv
    // (e.g. file-system names) the shell-wrapper invocations may carry; clap
    // derives the right parser automatically under `external_subcommand`.
    #[command(external_subcommand)]
    External(Vec<OsString>),
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Open the interactive config editor
    Edit,
}

fn default_log_file() -> Option<PathBuf> {
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
    Some(dir.join("ghost-complete.log"))
}

/// Default fallback shell when `$SHELL` is unset, empty, or unreadable.
const DEFAULT_FALLBACK_SHELL: &str = "/bin/zsh";

/// Resolve the default shell from `$SHELL`, falling back to [`DEFAULT_FALLBACK_SHELL`].
///
/// `env::var_os("SHELL")` returns `Some("")` when the variable is set but
/// empty — passing that straight to the PTY spawn produces an opaque
/// `ENOENT` and a confused user. Treat empty as missing so the fallback
/// applies. Returns `OsString` so a hypothetically non-UTF-8 `$SHELL`
/// survives end-to-end into the spawn.
fn resolve_default_shell() -> OsString {
    resolve_default_shell_from(|name| std::env::var_os(name))
}

/// Pure helper used by [`resolve_default_shell`]; takes an env-lookup closure
/// so the resolution rules can be unit-tested without touching process state.
fn resolve_default_shell_from<F>(lookup: F) -> OsString
where
    F: Fn(&str) -> Option<OsString>,
{
    lookup("SHELL")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| OsString::from(DEFAULT_FALLBACK_SHELL))
}

fn init_tracing(level: LogLevel, log_file: Option<&Path>) -> Result<()> {
    // Prefer `RUST_LOG` (standard ecosystem env var) when set; fall back to
    // the `--log-level` flag value otherwise. This matches how every other
    // tracing/log-based Rust binary behaves and keeps `--log-level` as a
    // convenient default for users who don't want to export an env var.
    // `level.as_filter_str()` returns one of the five fixed level directives
    // (`trace`/`debug`/`info`/`warn`/`error`), all of which are valid
    // `EnvFilter` syntax, so `try_new` cannot fail here — `expect` documents
    // the invariant and makes any future regression in `as_filter_str` loud
    // rather than silent.
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::try_new(level.as_filter_str())
            .expect("LogLevel ValueEnum variant maps to a valid EnvFilter directive")
    });

    if let Some(path) = log_file {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("failed to open log file: {}", path.display()))?;

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

/// Silently refresh `~/.config/ghost-complete/specs/` when the embedded
/// corpus is newer than the version stamped on disk.
///
/// The auto-refresh is gated on the **default** install mirror path
/// only. A user who pointed `[paths] spec_dirs` at a custom location is
/// expressing intent to manage their own corpus, and we never overwrite
/// that. We also never delete or recreate the mirror dir — we only
/// rewrite the embedded filenames in place and bump the stamp, so any
/// extra files a user dropped in survive untouched.
///
/// All outcomes route through `tracing` and never to stdout/stderr —
/// the proxy is about to take over the user's terminal and any
/// spontaneous printf here would smear into the shell prompt. Doctor
/// surfaces a `[WARN]` separately when the auto-refresh failed.
fn auto_refresh_install_mirror_if_stale(config: &gc_config::GhostConfig) {
    if !config.paths.spec_dirs.is_empty() {
        // Honor explicit overrides: a user who configured
        // `[paths] spec_dirs` is in control of their own mirror.
        return;
    }
    let Some(install_dir) = gc_suggest::mirror::default_install_mirror_dir() else {
        // HOME unset — nothing we can do.
        return;
    };
    let outcome = gc_suggest::mirror::refresh_install_mirror_if_stale(
        &install_dir,
        gc_suggest::mirror::CURRENT_VERSION,
        gc_suggest::mirror::write_embedded_mirror,
    );
    match outcome {
        gc_suggest::mirror::RefreshOutcome::NotInstalled
        | gc_suggest::mirror::RefreshOutcome::AlreadyFresh => {}
        gc_suggest::mirror::RefreshOutcome::Refreshed {
            previous_version,
            skipped_user_edited,
        } => {
            tracing::info!(
                previous = %previous_version,
                current = %gc_suggest::mirror::CURRENT_VERSION,
                install_dir = %install_dir.display(),
                skipped_user_edited = skipped_user_edited.len(),
                "refreshed stale install-mirror spec corpus"
            );
        }
        gc_suggest::mirror::RefreshOutcome::Failed { reason } => {
            tracing::warn!(
                install_dir = %install_dir.display(),
                error = %reason,
                "could not refresh stale install-mirror spec corpus — \
                 stale specs may win precedence over the embedded set. \
                 Run `ghost-complete install` to refresh manually."
            );
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config_path = cli.config;
    let log_level = cli.log_level;
    let log_file = cli.log_file;

    match cli.command {
        Some(Command::Install { dry_run }) => {
            init_tracing(log_level, log_file.as_deref())?;
            install::run_install(dry_run)
        }
        Some(Command::Uninstall) => {
            init_tracing(log_level, log_file.as_deref())?;
            install::run_uninstall()
        }
        Some(Command::ValidateSpecs { strict, json }) => {
            init_tracing(log_level, log_file.as_deref())?;
            validate::run_validate_specs_with_opts(config_path.as_deref(), strict, json)
        }
        Some(Command::Status {
            strict,
            json,
            baseline,
            verbose,
        }) => {
            init_tracing(log_level, log_file.as_deref())?;
            status::run_status_with_opts(
                config_path.as_deref(),
                strict,
                json,
                baseline.as_deref(),
                verbose,
            )
        }
        Some(Command::Config {
            subcommand: Some(ConfigCommand::Edit),
        }) => {
            init_tracing(log_level, log_file.as_deref())?;
            tui::run_config_editor(config_path.as_deref())
        }
        Some(Command::Config { subcommand: None }) => {
            init_tracing(log_level, log_file.as_deref())?;
            config_cmd::run_config(config_path.as_deref())
        }
        Some(Command::Doctor) => {
            init_tracing(log_level, log_file.as_deref())?;
            doctor::run_doctor(config_path.as_deref())
        }
        Some(Command::External(argv)) => {
            run_proxy(log_level, log_file, config_path.as_deref(), argv)
        }
        None => run_proxy(log_level, log_file, config_path.as_deref(), Vec::new()),
    }
}

// `cli_log_file` is taken by value because the body consumes it for the
// `.or_else(default_log_file)` chain below; `config_path` is borrowed
// because no caller hands ownership down beyond this stack frame.
fn run_proxy(
    log_level: LogLevel,
    cli_log_file: Option<PathBuf>,
    config_path: Option<&Path>,
    argv: Vec<OsString>,
) -> Result<()> {
    // Proxy mode — default to log file, never stderr
    let log_file = cli_log_file.or_else(default_log_file);
    init_tracing(log_level, log_file.as_deref())?;

    let (shell, args) = if argv.is_empty() {
        (resolve_default_shell(), vec![])
    } else {
        let mut iter = argv.into_iter();
        let shell = iter.next().expect("argv non-empty branch already checked");
        let args: Vec<OsString> = iter.collect();
        (shell, args)
    };

    let config = gc_config::GhostConfig::load(config_path).context("failed to load config")?;

    // Auto-refresh the install mirror (`~/.config/ghost-complete/specs/`)
    // if a previous binary version installed it. The mirror takes
    // filesystem precedence over the embedded corpus, so without this
    // refresh an operator who installed v0.15 and upgraded the binary to
    // v0.16 would silently keep serving v0.15 completions at every
    // keystroke. Skipped for users who explicitly configured
    // `[paths] spec_dirs` to a non-default location — those are
    // intentional overrides we must not clobber. See `gc_suggest::mirror`
    // for the full rationale.
    auto_refresh_install_mirror_if_stale(&config);

    tracing::info!(shell = %Path::new(&shell).display(), "starting ghost-complete proxy");

    // SAFETY: must run while the process is still single-threaded.
    // We're in `fn run_proxy`, called synchronously from `fn main`
    // before any `std::thread::spawn` or tokio runtime construction;
    // the AWS SDK reads this env var later from many threads but never
    // writes it, and nothing else in our process mutates the
    // environment after this point. See the
    // `gc_suggest::aws::set_imds_disabled_env` SAFETY doc.
    unsafe {
        gc_suggest::aws::set_imds_disabled_env();
    }

    let rt = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    let exit_code = rt.block_on(gc_pty::run_proxy(&shell, &args, &config))?;

    std::process::exit(exit_code);
}

#[cfg(test)]
mod tests {
    use super::{resolve_default_shell_from, DEFAULT_FALLBACK_SHELL};
    use std::ffi::OsString;

    #[test]
    fn resolve_default_shell_uses_env_when_set() {
        let shell = resolve_default_shell_from(|name| {
            assert_eq!(name, "SHELL");
            Some(OsString::from("/usr/local/bin/fish"))
        });
        assert_eq!(shell, OsString::from("/usr/local/bin/fish"));
    }

    #[test]
    fn resolve_default_shell_falls_back_when_unset() {
        let shell = resolve_default_shell_from(|_| None);
        assert_eq!(shell, OsString::from(DEFAULT_FALLBACK_SHELL));
    }

    #[test]
    fn resolve_default_shell_falls_back_when_empty() {
        // Regression: `env::var_os("SHELL")` returns `Some("")` when SHELL is
        // set but empty. Without the empty filter, the PTY spawn fails with a
        // cryptic ENOENT instead of using the fallback.
        let shell = resolve_default_shell_from(|_| Some(OsString::new()));
        assert_eq!(shell, OsString::from(DEFAULT_FALLBACK_SHELL));
    }
}
