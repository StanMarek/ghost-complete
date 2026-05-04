//! Per-context sandbox configuration.
//!
//! The runtime is reused across jobs for warm GC, but each job opens a
//! **fresh `Context`** so the JS executed for one spec cannot leak
//! state into the next. [`configure_context`] is therefore called
//! once per job, immediately after the new context is created.

use rquickjs::function::Rest;
use rquickjs::prelude::Func;
use rquickjs::{CatchResultExt, Ctx, Object, Value};

/// Globals we strip from the freshly-created context.
///
/// These names cover the Node/Deno/Bun extensions corpus authors might
/// reach for. QuickJS ships with none of them by default, but listing
/// them explicitly keeps the sandbox honest if a future feature flag
/// (or accidental linkage) starts including them.
const STRIPPED_GLOBALS: &[&str] = &[
    // Module / process control
    "require",
    "module",
    "exports",
    "process",
    "Deno",
    "Bun",
    "globalThis_browser",
    // Timers / scheduling
    "setTimeout",
    "setInterval",
    "setImmediate",
    "clearTimeout",
    "clearInterval",
    "clearImmediate",
    "queueMicrotask",
    // Networking
    "fetch",
    "XMLHttpRequest",
    "WebSocket",
    "EventSource",
    "Request",
    "Response",
    "Headers",
    "FormData",
    // Workers / IPC
    "Worker",
    "SharedWorker",
    "MessageChannel",
    "MessagePort",
    "BroadcastChannel",
    // Storage
    "localStorage",
    "sessionStorage",
    "indexedDB",
    // Node-style buffers and streams
    "Buffer",
    "ReadableStream",
    "WritableStream",
    "TransformStream",
    // Misc browser globals
    "alert",
    "confirm",
    "prompt",
    "navigator",
    "document",
    "window",
];

/// Names we replace with a thrower for defense in depth.
///
/// `eval` and `Function` aren't strictly dangerous — they're regular
/// QuickJS intrinsics — but the corpus is not expected to use them and
/// disabling them keeps the attack surface narrow if a malicious spec
/// ever lands in the embedded set.
const DISABLED_INTRINSICS: &[&str] = &["eval", "Function"];

/// Apply the sandbox configuration to a freshly-created [`Ctx`].
///
/// Strips Node-style globals and replaces `eval`/`Function` with
/// throwing stubs. Idempotent on the JS side: calling twice removes
/// already-removed names without error and re-shadows the throwers.
pub(crate) fn configure_context<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<()> {
    let globals: Object<'js> = ctx.globals();

    for name in STRIPPED_GLOBALS {
        let _ = globals.remove(*name);
    }

    for name in DISABLED_INTRINSICS {
        let disabled_name = *name;
        globals.set(
            *name,
            Func::new(
                move |ctx: Ctx<'js>, _args: Rest<Value<'js>>| -> rquickjs::Result<()> {
                    let msg = format!("{disabled_name} is disabled in gc-jsrt");
                    Err(ctx.throw(rquickjs::String::from_str(ctx.clone(), &msg)?.into()))
                },
            ),
        )?;
    }

    Ok(())
}

/// Convenience wrapper that runs `configure_context` and converts any
/// failure into a stringified internal error.
pub(crate) fn configure_or_internal<'js>(ctx: &Ctx<'js>) -> Result<(), String> {
    configure_context(ctx)
        .catch(ctx)
        .map_err(|e| format!("sandbox configuration failed: {e}"))
}
