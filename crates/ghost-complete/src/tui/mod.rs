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

pub fn run_config_editor(config_path: Option<&str>) -> Result<()> {
    let path = match config_path {
        Some(p) => PathBuf::from(p),
        None => {
            let dir = gc_config::config_dir().context("cannot determine config directory")?;
            dir.join("config.toml")
        }
    };

    let config = gc_config::GhostConfig::load(config_path)?;
    let raw_toml = if path.exists() {
        std::fs::read_to_string(&path).unwrap_or_default()
    } else {
        String::new()
    };

    let mut app = app::App::new(config, raw_toml, path);

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_event_loop(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result
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
