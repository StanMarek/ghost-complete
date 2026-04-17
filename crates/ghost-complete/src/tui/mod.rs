pub mod app;
pub mod editor;
pub mod fields;
pub mod preview;
pub mod sample;
pub mod toml_patch;
pub mod ui;

use anyhow::{Context, Result};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Once;

/// RAII guard that restores terminal state on Drop — including during a panic
/// unwind or a partial setup failure. The guard owns each step's flag, so if
/// `execute!` fails after raw mode was enabled (or the caller drops the
/// session before a `Terminal` was built), Drop still tears down everything
/// that was successfully activated.
struct TerminalSession {
    raw_enabled: bool,
    alt_entered: bool,
    mouse_captured: bool,
}

impl TerminalSession {
    fn new() -> Result<Self> {
        enable_raw_mode().context("failed to enable raw mode")?;
        let mut this = Self {
            raw_enabled: true,
            alt_entered: false,
            mouse_captured: false,
        };
        execute!(io::stdout(), EnterAlternateScreen).context("failed to enter alternate screen")?;
        this.alt_entered = true;
        execute!(io::stdout(), EnableMouseCapture).context("failed to enable mouse capture")?;
        this.mouse_captured = true;
        Ok(this)
    }

    fn terminal(&self) -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
        let backend = CrosstermBackend::new(io::stdout());
        Terminal::new(backend).context("failed to create terminal")
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        // Best-effort cleanup — swallow errors so Drop never panics.
        if self.mouse_captured {
            let _ = execute!(io::stdout(), DisableMouseCapture);
        }
        if self.alt_entered {
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
        }
        if self.raw_enabled {
            let _ = disable_raw_mode();
        }
        let _ = execute!(io::stdout(), crossterm::cursor::Show);
    }
}

pub fn run_config_editor(config_path: Option<&str>) -> Result<()> {
    let path = match config_path {
        Some(p) => PathBuf::from(p),
        None => {
            let dir = gc_config::config_dir().context("cannot determine config directory")?;
            dir.join("config.toml")
        }
    };

    let config = gc_config::GhostConfig::load(config_path)?;
    let raw_toml = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(anyhow::Error::new(e)
                .context(format!("failed to read config file: {}", path.display())));
        }
    };

    let mut app = app::App::new(config, raw_toml, path);

    // Install the panic hook before we touch terminal state so even an
    // `enable_raw_mode` panic would leave a usable terminal. Safe to call more
    // than once — the `Once` ensures we wrap the previous hook a single time.
    install_panic_hook();

    // Session owns the raw-mode + alt-screen state; Terminal is constructed
    // after, so a Terminal::new failure still triggers session cleanup on drop.
    let _session = TerminalSession::new()?;
    let mut terminal = _session.terminal()?;
    run_event_loop(&mut terminal, &mut app)
}

/// Install a panic hook that restores terminal state before printing the panic
/// message, so a crash during the event loop doesn't leave the user in raw
/// mode on the alternate screen. Chains to the previously-installed hook so
/// other crash reporters (color-eyre, sentry, etc.) still run.
fn install_panic_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // Best-effort teardown — ignore errors because we're already panicking.
            let _ = execute!(io::stdout(), DisableMouseCapture);
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), crossterm::cursor::Show);
            let _ = io::stdout().flush();

            // Write panic details to stderr so the user gets a readable trace
            // instead of a corrupted terminal.
            let backtrace = std::backtrace::Backtrace::capture();
            let _ = writeln!(io::stderr(), "\nghost-complete TUI panicked: {info}");
            let _ = writeln!(io::stderr(), "backtrace:\n{backtrace}");

            prev(info);
        }));
    });
}

fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut app::App,
) -> Result<()> {
    loop {
        terminal.draw(|frame| ui::render(frame, app))?;
        if app.should_quit {
            return Ok(());
        }
        match event::read()? {
            Event::Key(key) => editor::handle_key(app, key),
            // `autoresize` + a fresh draw on the next loop iteration is all
            // ratatui needs; nothing in App state depends on the size.
            Event::Resize(_, _) => continue,
            // Mouse / focus / paste events are not wired up yet — swallow them
            // so they don't block the redraw.
            _ => continue,
        }
    }
}
