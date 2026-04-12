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
use std::io;
use std::path::PathBuf;

/// RAII guard that restores terminal state on Drop — including during a panic
/// unwind. Without this, a panic in the draw/event loop leaves the user's
/// terminal in raw mode + alternate screen with the cursor hidden.
struct TerminalSession {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl TerminalSession {
    fn new() -> Result<Self> {
        enable_raw_mode().context("failed to enable raw mode")?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
            .context("failed to enter alternate screen")?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).context("failed to create terminal")?;
        Ok(Self { terminal })
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        // Best-effort cleanup — swallow errors so Drop never panics.
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        );
        let _ = self.terminal.show_cursor();
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

    let mut session = TerminalSession::new()?;
    run_event_loop(&mut session.terminal, &mut app)
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
        if let Event::Key(key) = event::read()? {
            editor::handle_key(app, key);
        }
    }
}
