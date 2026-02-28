use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "ghost-complete", about = "Terminal-native autocomplete engine")]
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
        // Write logs to stderr — but only if a log file isn't specified.
        // In practice, for a PTY proxy you almost always want --log-file
        // because stderr goes to the terminal and would corrupt output.
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .init();
    }

    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    init_tracing(&cli.log_level, cli.log_file.as_deref())?;

    let (shell, args) = if cli.shell_args.is_empty() {
        let default_shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        (default_shell, vec![])
    } else {
        let mut iter = cli.shell_args.into_iter();
        let shell = iter.next().unwrap();
        let args: Vec<String> = iter.collect();
        (shell, args)
    };

    let config = gc_config::GhostConfig::load(cli.config.as_deref())
        .context("failed to load config")?;

    tracing::info!(shell = %shell, "starting ghost-complete proxy");

    let rt = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    let exit_code = rt.block_on(gc_pty::run_proxy(&shell, &args, &config))?;

    std::process::exit(exit_code);
}
