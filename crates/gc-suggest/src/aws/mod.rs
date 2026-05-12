pub(crate) mod clients;
pub(crate) mod error;

use std::sync::atomic::{AtomicBool, Ordering};

/// `true` iff [`set_imds_disabled_env`] injected `AWS_EC2_METADATA_DISABLED`
/// on its own. Spawned child shells must strip the var in that case so the
/// proxy does not silently rewrite the user's environment.
static WE_INJECTED_IMDS_DISABLED: AtomicBool = AtomicBool::new(false);

/// The env var name we inject for AWS SDK credential resolution.
pub const IMDS_DISABLED_ENV: &str = "AWS_EC2_METADATA_DISABLED";

/// Set `AWS_EC2_METADATA_DISABLED=true` so that the AWS SDK never
/// attempts a 169.254.169.254 probe for completion suggestions. The
/// `aws_config` crate has no typed builder for this in 1.6.3, so we
/// fall back to the env var the SDK already consults.
///
/// If the user already exported the variable (any value), we leave
/// their value in place and record nothing — the spawned child shell
/// inherits whatever they configured. Otherwise we inject `true` and
/// flag the injection so [`imds_disabled_was_injected`] returns
/// `true`; the PTY spawn path uses that signal to strip the variable
/// from the child env, preventing the proxy from leaking its internal
/// SDK plumbing into the user's shell.
///
/// # Safety
///
/// **Must be called before the tokio runtime is built**, while the
/// process is still single-threaded. `std::env::set_var` is racy
/// against any concurrent reader or writer of the environment — but
/// the contract here is that this runs in `fn main` before any thread
/// is spawned. The function is `unsafe` to make that contract visible
/// at every call site.
pub unsafe fn set_imds_disabled_env() {
    if std::env::var_os(IMDS_DISABLED_ENV).is_some() {
        return;
    }
    // SAFETY: Forwarded — see function-level docs.
    unsafe {
        std::env::set_var(IMDS_DISABLED_ENV, "true");
    }
    WE_INJECTED_IMDS_DISABLED.store(true, Ordering::Release);
}

/// Returns `true` iff [`set_imds_disabled_env`] was the one that
/// inserted `AWS_EC2_METADATA_DISABLED` into the process environment.
/// Spawned child processes that inherit our env must drop the key
/// when this returns `true` so the proxy does not contaminate the
/// user's shell with an internal SDK knob.
pub fn imds_disabled_was_injected() -> bool {
    WE_INJECTED_IMDS_DISABLED.load(Ordering::Acquire)
}
