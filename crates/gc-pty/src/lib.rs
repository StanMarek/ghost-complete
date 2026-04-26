//! PTY proxy event loop.
//!
//! Spawns the user's shell via `portable-pty`, multiplexes stdin/stdout with
//! `tokio::select!`, handles `SIGWINCH` resize, and intercepts keystrokes
//! for popup navigation.

mod config_watch;
pub mod handler;
pub mod input;
mod proxy;
mod resize;
mod spawn;

pub use gc_overlay::parse_style;
pub use handler::parse_key_name;
pub use proxy::run_proxy;
