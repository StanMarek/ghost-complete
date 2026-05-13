//! Synchronous adapter that lets the gc-jsrt worker thread run shell
//! commands by blocking on the engine's tokio runtime.
//!
//! # Why a separate file
//!
//! `gc-jsrt` defines the [`gc_jsrt::ShellRunner`] trait without taking
//! a tokio dependency, so it can be used in pure-test contexts. The
//! engine-side implementation is the only place that knows how to
//! reach `script::run_script_full`, so it lives here next to the rest
//! of the engine.
//!
//! # Threading model
//!
//! `executeShellCommand` is invoked synchronously from the JS worker
//! thread. That worker is a regular OS thread (spawned by
//! `JsWorker::spawn` inside `gc-jsrt`), NOT a tokio task. Calling
//! `tokio::runtime::Handle::block_on(...)` from a non-runtime thread
//! is supported by tokio and is the documented way to bridge sync ↔
//! async at a thread boundary.
//!
//! We intentionally do NOT call `block_on` from inside a tokio task —
//! tokio panics in that situation (`Cannot block the current thread
//! from within a runtime`). The JS worker thread is the only caller.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::runtime::Handle;

use gc_jsrt::{ShellRunError, ShellRunOutput, ShellRunner};

use crate::script::run_script_full_with_env;

/// Engine-side [`ShellRunner`] backed by `tokio::process::Command` via
/// the existing `script::run_script_full` helper.
pub struct EngineShellRunner {
    /// Tokio handle the worker thread uses to drive `run_script_full`.
    handle: Handle,
    env: Option<Arc<HashMap<String, String>>>,
}

impl EngineShellRunner {
    /// Construct a runner from the current tokio runtime handle. The
    /// caller MUST be inside a tokio runtime when this is called.
    pub fn from_current_handle() -> Self {
        Self {
            handle: Handle::current(),
            env: None,
        }
    }

    pub fn from_current_handle_with_env(env: Option<Arc<HashMap<String, String>>>) -> Self {
        Self {
            handle: Handle::current(),
            env,
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
        let env = self.env.clone();
        // `block_on` on a tokio Handle from outside the runtime works:
        // it parks the calling (worker) thread until the future
        // resolves on the runtime threadpool. We are NEVER on a tokio
        // task here — see the module-level safety note.
        self.handle.block_on(async move {
            let argv_refs: Vec<&str> = argv_owned.iter().map(|s| s.as_str()).collect();
            run_script_full_with_env(&argv_refs, &cwd_owned, timeout_ms, env.as_deref()).await
        })
    }

    fn run_string(
        &self,
        command: &str,
        cwd: &Path,
        timeout: Duration,
    ) -> Result<ShellRunOutput, ShellRunError> {
        // `allow_shell_command=false` is the spec default; this branch
        // fires only for explicitly-opted-in specs. Parse via
        // `shlex::split` (the workspace's existing shell-tokeniser) and
        // dispatch through `run_argv` so we keep the same exec path.
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

#[cfg(test)]
mod tests {
    use super::*;

    // The runner contract requires `block_on` from a non-runtime thread
    // (tokio panics if called from inside a tokio task). The helpers
    // build a multi-threaded runtime, hand its handle to a fresh OS
    // thread, and drive the runner there — mirroring the JS-worker
    // production caller exactly.
    fn drive_argv(argv: Vec<String>, timeout_ms: u64) -> Result<ShellRunOutput, ShellRunError> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("tokio runtime");
        let handle = rt.handle().clone();
        std::thread::spawn(move || {
            let runner = EngineShellRunner { handle, env: None };
            runner.run_argv(
                &argv,
                std::path::Path::new("/tmp"),
                Duration::from_millis(timeout_ms),
            )
        })
        .join()
        .expect("thread join")
    }

    fn drive_string(command: String, timeout_ms: u64) -> Result<ShellRunOutput, ShellRunError> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("tokio runtime");
        let handle = rt.handle().clone();
        std::thread::spawn(move || {
            let runner = EngineShellRunner { handle, env: None };
            runner.run_string(
                &command,
                std::path::Path::new("/tmp"),
                Duration::from_millis(timeout_ms),
            )
        })
        .join()
        .expect("thread join")
    }

    #[test]
    fn run_argv_timeout_classifies_as_timeout() {
        let err = drive_argv(vec!["/bin/sleep".into(), "5".into()], 50)
            .expect_err("sleep 5 with 50ms timeout must error");
        assert!(
            matches!(err, ShellRunError::Timeout),
            "expected Timeout, got: {err:?}"
        );
    }

    #[test]
    fn run_argv_nonzero_exit_classifies_as_nonzero_with_real_code() {
        let err = drive_argv(vec!["sh".into(), "-c".into(), "exit 1".into()], 5_000)
            .expect_err("exit 1 must surface as error");
        match err {
            ShellRunError::NonZeroExit { exit_code, .. } => {
                assert_eq!(exit_code, Some(1), "real exit code must surface");
            }
            other => panic!("expected NonZeroExit, got: {other:?}"),
        }
    }

    #[test]
    fn run_argv_nonzero_exit_carries_real_stderr() {
        let err = drive_argv(
            vec![
                "sh".into(),
                "-c".into(),
                "echo real-stderr-msg >&2; exit 7".into(),
            ],
            5_000,
        )
        .expect_err("script with stderr + non-zero exit must error");
        match err {
            ShellRunError::NonZeroExit {
                exit_code, stderr, ..
            } => {
                assert_eq!(exit_code, Some(7));
                assert!(
                    stderr.contains("real-stderr-msg"),
                    "stderr must reach JS verbatim, got: {stderr:?}"
                );
            }
            other => panic!("expected NonZeroExit, got: {other:?}"),
        }
    }

    #[test]
    fn run_argv_spawn_failure_classifies_as_spawn() {
        let err = drive_argv(
            vec!["/nonexistent/binary/that/should/never/exist".into()],
            5_000,
        )
        .expect_err("missing binary must surface as error");
        assert!(
            matches!(err, ShellRunError::Spawn(_)),
            "expected Spawn, got: {err:?}"
        );
    }

    #[test]
    fn run_string_shlex_failure_classifies_as_argv_parse() {
        let err = drive_string("echo \"unmatched".into(), 5_000)
            .expect_err("unmatched quote must surface as error");
        assert!(
            matches!(err, ShellRunError::ArgvParse(_)),
            "expected ArgvParse, got: {err:?}"
        );
    }
}
