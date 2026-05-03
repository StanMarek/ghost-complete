//! Synchronous adapter that lets the gc-jsrt worker thread run shell
//! commands by blocking on the engine's tokio runtime.
//!
//! # Why a separate file
//!
//! `gc-jsrt` defines the [`gc_jsrt::ShellRunner`] trait without taking
//! a tokio dependency, so it can be used in pure-test contexts. The
//! engine-side implementation is the only place that knows how to
//! reach `script::run_script`, so it lives here next to the rest of
//! the engine.
//!
//! # Threading model
//!
//! Phase 5 calls `executeShellCommand` synchronously from the JS
//! worker thread. That worker is a regular OS thread (spawned by
//! `JsWorker::spawn` inside `gc-jsrt`), NOT a tokio task. Calling
//! `tokio::runtime::Handle::block_on(...)` from a non-runtime thread
//! is supported by tokio and is the documented way to bridge sync ↔
//! async at a thread boundary.
//!
//! We intentionally do NOT call `block_on` from inside a tokio task —
//! tokio panics in that situation (`Cannot block the current thread
//! from within a runtime`). The JS worker thread is the only caller.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::runtime::Handle;

use gc_jsrt::{ShellRunError, ShellRunOutput, ShellRunner};

use crate::script::run_script;

/// Engine-side [`ShellRunner`] backed by `tokio::process::Command` via
/// the existing `script::run_script` helper.
pub struct EngineShellRunner {
    /// Tokio handle the worker thread uses to drive `run_script`.
    handle: Handle,
}

impl EngineShellRunner {
    /// Construct a runner from the current tokio runtime handle. The
    /// caller MUST be inside a tokio runtime when this is called.
    pub fn from_current_handle() -> Self {
        Self {
            handle: Handle::current(),
        }
    }

    /// Wrap the runner in an `Arc` for the worker.
    pub fn into_arc(self) -> Arc<dyn ShellRunner> {
        Arc::new(self)
    }
}

impl ShellRunner for EngineShellRunner {
    fn run_argv(
        &self,
        argv: &[String],
        cwd: &Path,
        timeout: Duration,
    ) -> Result<ShellRunOutput, ShellRunError> {
        let argv_owned: Vec<String> = argv.to_vec();
        let cwd_owned = cwd.to_path_buf();
        let timeout_ms = timeout.as_millis() as u64;
        // `block_on` on a tokio Handle from outside the runtime works:
        // it parks the calling (worker) thread until the future
        // resolves on the runtime threadpool. We are NEVER on a tokio
        // task here — see the module-level safety note.
        let result = self.handle.block_on(async move {
            let argv_refs: Vec<&str> = argv_owned.iter().map(|s| s.as_str()).collect();
            run_script(&argv_refs, &cwd_owned, timeout_ms).await
        });
        match result {
            Ok(stdout) => Ok(ShellRunOutput {
                stdout,
                stderr: String::new(),
                exit_code: Some(0),
            }),
            Err(e) => {
                // `run_script` collapses several distinct failure modes
                // into a single `anyhow::Error`. We pattern-match on
                // the message string for the common cases (timeout,
                // non-zero exit) so the JS-side diagnostic carries a
                // tighter classification.
                let msg = e.to_string();
                if msg.contains("timed out") {
                    Err(ShellRunError::Timeout)
                } else if msg.contains("exited with status") {
                    Err(ShellRunError::NonZeroExit {
                        exit_code: None,
                        stdout: String::new(),
                        stderr: msg,
                    })
                } else {
                    Err(ShellRunError::Spawn(msg))
                }
            }
        }
    }

    fn run_string(
        &self,
        command: &str,
        cwd: &Path,
        timeout: Duration,
    ) -> Result<ShellRunOutput, ShellRunError> {
        // Phase 5 ships with `allow_shell_command=false` for every
        // shipped spec; this branch only fires when a future spec
        // explicitly opts in. Parse via `shlex::split` (the workspace's
        // existing shell-tokeniser) and dispatch through `run_argv` so
        // we keep the same exec path.
        let argv = match shlex::split(command) {
            Some(v) if !v.is_empty() => v,
            Some(_) => {
                return Err(ShellRunError::ArgvParse(
                    "shell-string parsed to empty argv".into(),
                ))
            }
            None => {
                return Err(ShellRunError::ArgvParse(format!(
                    "shlex could not parse: {command:?}"
                )))
            }
        };
        self.run_argv(&argv, cwd, timeout)
    }
}
