//! PTY proxy event loop.
//!
//! Spawns the user's shell via `portable-pty`, multiplexes stdin/stdout with
//! `tokio::select!`, handles `SIGWINCH` resize, and intercepts keystrokes
//! for popup navigation.

mod handler;
pub mod input;
mod proxy;
mod resize;
mod spawn;

pub use proxy::run_proxy;
