use std::io::Write;

use anyhow::{Context, Result};
use gc_suggest::spec_dirs::resolve_spec_dirs;
use gc_suggest::specs::{ArgSpec, CompletionSpec, GeneratorSpec, OptionSpec, SubcommandSpec};
use gc_suggest::SpecStore;

use crate::sanitize::sanitize_for_terminal;

/// Check if a spec tree contains any generators with `requires_js: true`.
fn has_requires_js(spec: &CompletionSpec) -> bool {
    check_args_for_js(&spec.args)
        || check_options_for_js(&spec.options)
        || check_subcommands_for_js(&spec.subcommands)
}

fn check_generators_for_js(generators: &[GeneratorSpec]) -> bool {
    generators.iter().any(|g| g.requires_js)
}

fn check_arg_for_js(arg: &ArgSpec) -> bool {
    check_generators_for_js(&arg.generators)
}

fn check_args_for_js(args: &[ArgSpec]) -> bool {
    args.iter().any(check_arg_for_js)
}

fn check_options_for_js(options: &[OptionSpec]) -> bool {
    options
        .iter()
        .any(|o| o.args.as_ref().is_some_and(check_arg_for_js))
}

fn check_subcommands_for_js(subcommands: &[SubcommandSpec]) -> bool {
    subcommands.iter().any(|s| {
        check_args_for_js(&s.args)
            || check_options_for_js(&s.options)
            || check_subcommands_for_js(&s.subcommands)
    })
}

/// Outcome of a `status` run — surfaces the numbers the CLI entry point
/// uses to decide the process exit code in strict mode.
#[derive(Debug, Default, Clone, Copy)]
pub struct StatusOutcome {
    pub fs_specs: usize,
    pub embedded_count: usize,
    pub total_parse_errors: usize,
}

/// Inner implementation that writes its report to `out` instead of stdout,
/// so the sanitisation path can be tested without a real terminal.
fn run_status_inner(
    config_path: Option<&str>,
    out: &mut dyn std::io::Write,
) -> Result<StatusOutcome> {
    let config = gc_config::GhostConfig::load(config_path).context("failed to load config")?;
    let dirs = resolve_spec_dirs(&config.paths.spec_dirs);

    let embedded_count = crate::install::EMBEDDED_SPECS.len();

    // Scan filesystem spec directories for overrides / custom specs
    let mut fs_specs = 0usize;
    let mut fully_functional = 0usize;
    let mut partially_functional = 0usize;
    let mut js_commands: Vec<String> = Vec::new();
    let mut total_parse_errors = 0usize;

    for dir in &dirs {
        let result = SpecStore::load_from_dir(dir)?;
        let store = result.store;

        let mut specs: Vec<(&str, &CompletionSpec)> = store.iter().collect();
        specs.sort_by_key(|(name, _)| *name);

        for (name, spec) in &specs {
            fs_specs += 1;
            if has_requires_js(spec) {
                partially_functional += 1;
                js_commands.push((*name).to_string());
            } else {
                fully_functional += 1;
            }
        }

        if !result.errors.is_empty() {
            total_parse_errors += result.errors.len();
            writeln!(
                out,
                "\x1b[33m{} spec(s) failed to load:\x1b[0m",
                result.errors.len()
            )?;
            // Error strings embed `file_name` built from on-disk spec
            // filenames. A hostile filename (same-user local threat) can
            // smuggle CSI/OSC sequences here — sanitise at the print
            // boundary, matching `doctor::print_results` and
            // `validate::validate_dir`.
            for err in &result.errors {
                writeln!(out, "  \x1b[33m- {}\x1b[0m", sanitize_for_terminal(err))?;
            }
        }
    }

    js_commands.sort();

    writeln!(out, "Ghost Complete v{}\n", env!("CARGO_PKG_VERSION"))?;
    writeln!(out, "Completion specs:")?;
    writeln!(out, "  Embedded in binary:    {embedded_count}")?;
    if fs_specs > 0 {
        writeln!(out, "  Filesystem overrides:  {fs_specs}")?;
        writeln!(
            out,
            "  \x1b[32mFully functional:\x1b[0m      {fully_functional}"
        )?;
        writeln!(
            out,
            "  \x1b[33mPartially functional:\x1b[0m  {partially_functional} (has requires_js generators)"
        )?;
    } else {
        writeln!(
            out,
            "  Filesystem overrides:  0 (run `ghost-complete install` to deploy specs)"
        )?;
    }

    if !js_commands.is_empty() {
        writeln!(
            out,
            "\nCommands with requires_js generators ({}):",
            js_commands.len()
        )?;
        // Spec command names are normally sanitised at load time, but
        // defense-in-depth at the print boundary costs nothing.
        for cmd in &js_commands {
            writeln!(out, "  {}", sanitize_for_terminal(cmd))?;
        }
    }

    Ok(StatusOutcome {
        fs_specs,
        embedded_count,
        total_parse_errors,
    })
}

/// Render the status report. When `strict` is `true`, prints the full report
/// first and then exits with code 1 if spec health is degraded — meaning any
/// of:
///   - zero specs loaded across all configured spec dirs AND no embedded
///     specs available (nothing to complete against), or
///   - one or more spec files failed to parse (`SpecLoadResult::errors`
///     non-empty in at least one dir).
///
/// Non-strict mode preserves the prior behaviour: always returns `Ok(())`
/// regardless of spec health.
pub fn run_status_with_opts(config_path: Option<&str>, strict: bool) -> Result<()> {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let outcome = run_status_inner(config_path, &mut handle)?;

    // Strict-mode health check: print the report above first, then exit
    // non-zero if anything is wrong. "Wrong" = either there's literally
    // nothing to complete against (no filesystem specs AND no embedded
    // specs), or at least one spec file failed to parse.
    if strict {
        let no_specs_available = outcome.fs_specs == 0 && outcome.embedded_count == 0;
        if no_specs_available || outcome.total_parse_errors > 0 {
            writeln!(&mut handle)?;
            if no_specs_available {
                writeln!(
                    &mut handle,
                    "\x1b[31mstrict mode: no specs available (0 embedded, 0 filesystem).\x1b[0m"
                )?;
            }
            if outcome.total_parse_errors > 0 {
                writeln!(
                    &mut handle,
                    "\x1b[31mstrict mode: {} spec file(s) failed to parse.\x1b[0m",
                    outcome.total_parse_errors
                )?;
            }
            std::process::exit(1);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a config TOML pointing at a single spec dir, write it to
    /// `tmp/config.toml`, and return its path.
    fn write_config_for(spec_dir: &std::path::Path, tmp: &tempfile::TempDir) -> std::path::PathBuf {
        let cfg_path = tmp.path().join("config.toml");
        let body = format!(
            "[paths]\nspec_dirs = [\"{}\"]\n",
            spec_dir.display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(&cfg_path, body).unwrap();
        cfg_path
    }

    #[test]
    fn status_sanitizes_hostile_spec_filenames_in_errors() {
        // A hostile filename embedded in the on-disk spec dir must not
        // smuggle raw ESC bytes through `ghost-complete status` output.
        // The spec loader fails to parse this file (not valid JSON) and
        // the resulting error string embeds the filename verbatim — which
        // would otherwise reach stdout unsanitised.
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        let hostile = "\x1b[31mEVIL.json";
        std::fs::write(spec_dir.join(hostile), "not valid json").unwrap();
        let cfg = write_config_for(&spec_dir, &tmp);

        let mut out = Vec::new();
        run_status_inner(Some(cfg.to_str().unwrap()), &mut out).unwrap();
        let txt = String::from_utf8_lossy(&out);

        // The "failed to load" line is the only place user-supplied bytes
        // reach stdout in the status report. Pull it out and assert the
        // filename's raw ESC was stripped. The line still has `\x1b[33m`
        // color wrappers from our own formatter — those are fine; what
        // matters is that the *inner* error text carries no ESC.
        let err_line = txt
            .lines()
            .find(|l| l.contains("EVIL.json"))
            .unwrap_or_else(|| panic!("expected error line mentioning EVIL.json, got:\n{txt}"));
        let inner = err_line
            .trim_start_matches("  \x1b[33m- ")
            .trim_end_matches("\x1b[0m");
        assert!(
            !inner.contains('\x1b'),
            "error payload must not contain raw ESC bytes from filename, got:\n{inner:?}"
        );
        assert!(
            inner.contains("[31mEVIL.json"),
            "sanitized filename (ESC stripped, CSI params retained as literal text) \
             should appear in output:\n{inner}"
        );
    }
}
