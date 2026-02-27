use anyhow::Result;
use portable_pty::{MasterPty, PtySize};

pub fn get_terminal_size() -> Result<PtySize> {
    let (cols, rows) = crossterm::terminal::size()?;
    Ok(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })
}

pub fn resize_pty(master: &dyn MasterPty, size: PtySize) -> Result<()> {
    master.resize(size)?;
    Ok(())
}
