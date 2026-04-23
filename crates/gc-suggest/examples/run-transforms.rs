//! Oracle helper for the fig-converter: apply a transform pipeline and emit suggestions as JSON.
//!
//! Reads `{ "transforms": [...], "input": "..." }` from stdin, runs the
//! pipeline, and writes `{ "output": [...] }` to stdout on success or
//! `{ "error": "..." }` on failure. Exits 0 on success, 1 on any error.
//!
//! Invoked by the fig-converter oracle (`tools/fig-converter/src/oracle.js`)
//! via `spawnSync`. Kept intentionally minimal — the real logic lives in
//! `gc_suggest::pipeline`.
//!
//! Run manually:
//!
//! ```sh
//! echo '{"transforms":["split_lines","filter_empty"],"input":"a\nb\n\nc\n"}' \
//!     | cargo run -p gc-suggest --example run-transforms
//! ```

use std::io::{self, Read, Write};
use std::process::ExitCode;

use gc_suggest::transform::Transform;
use gc_suggest::try_run_pipeline;
use serde::Deserialize;

#[derive(Deserialize)]
struct Input {
    transforms: Vec<Transform>,
    input: String,
}

fn emit_error(msg: &str) {
    // Best-effort: if stdout is broken we can't do much useful anyway.
    let payload = serde_json::json!({ "error": msg });
    let _ = writeln!(io::stdout(), "{payload}");
}

fn main() -> ExitCode {
    let mut raw = String::new();
    if let Err(e) = io::stdin().read_to_string(&mut raw) {
        emit_error(&format!("failed to read stdin: {e}"));
        return ExitCode::from(1);
    }

    let parsed: Input = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            emit_error(&format!("transform deserialization failed: {e}"));
            return ExitCode::from(1);
        }
    };

    let suggestions = match try_run_pipeline(&parsed.transforms, &parsed.input) {
        Ok(v) => v,
        Err(e) => {
            emit_error(&format!("pipeline execution failed: {e}"));
            return ExitCode::from(1);
        }
    };

    let out = serde_json::json!({ "output": suggestions });
    if let Err(e) = writeln!(io::stdout(), "{out}") {
        eprintln!("run-transforms: failed to write stdout: {e}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
