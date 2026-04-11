mod config_cmd;
mod doctor;
mod install;
mod status;
mod validate;

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "ghost-complete",
    version,
    about = "Terminal-native autocomplete engine",
    after_help = "COMMANDS:\n  install          Install shell integration (zsh)\n  uninstall        Remove shell integration\n  validate-specs   Validate completion spec files\n  status           Show loaded specs and JS compatibility\n  config           Show resolved configuration\n  doctor           Run health checks\n\nSHELL SUPPORT:\n  zsh   Full support (auto-installed into ~/.zshrc)"
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
            dir.display()
        );
        return None;
    }
    Some(
        dir.join("ghost-complete.log")
            .to_string_lossy()
            .into_owned(),
    )
}

fn init_tracing(level: &str, log_file: Option<&str>) -> Result<()> {
    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("warn"));

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
            return status::run_status(cli.config.as_deref());
        }
        Some("config") => {
            init_tracing(&cli.log_level, cli.log_file.as_deref())?;
            return config_cmd::run_config(cli.config.as_deref());
        }
        Some("doctor") => {
            init_tracing(&cli.log_level, cli.log_file.as_deref())?;
            return doctor::run_doctor(cli.config.as_deref());
        }
        _ => {}
    }

    // Proxy mode — default to log file, never stderr
    let log_file = cli.log_file.or_else(default_log_file);
    init_tracing(&cli.log_level, log_file.as_deref())?;

    let (shell, args) = if cli.shell_args.is_empty() {
        let default_shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        (default_shell, vec![])
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
