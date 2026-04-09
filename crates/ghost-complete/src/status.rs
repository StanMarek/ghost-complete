use anyhow::{Context, Result};
use gc_suggest::specs::{ArgSpec, CompletionSpec, GeneratorSpec, OptionSpec, SubcommandSpec};
use gc_suggest::SpecStore;

use crate::spec_dirs::resolve_spec_dirs;

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

pub fn run_status(config_path: Option<&str>) -> Result<()> {
    let config = gc_config::GhostConfig::load(config_path).context("failed to load config")?;
    let dirs = resolve_spec_dirs(&config);

    let embedded_count = crate::install::EMBEDDED_SPECS.len();

    // Scan filesystem spec directories for overrides / custom specs
    let mut fs_specs = 0usize;
    let mut fully_functional = 0usize;
    let mut partially_functional = 0usize;
    let mut js_commands: Vec<String> = Vec::new();

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
            println!(
                "\x1b[33m{} spec(s) failed to load\x1b[0m",
                result.errors.len()
            );
        }
    }

    js_commands.sort();

    println!("Ghost Complete v{}\n", env!("CARGO_PKG_VERSION"));
    println!("Completion specs:");
    println!("  Embedded in binary:    {embedded_count}");
    if fs_specs > 0 {
        println!("  Filesystem overrides:  {fs_specs}");
        println!("  \x1b[32mFully functional:\x1b[0m      {fully_functional}");
        println!(
            "  \x1b[33mPartially functional:\x1b[0m  {partially_functional} (has requires_js generators)"
        );
    } else {
        println!("  Filesystem overrides:  0 (run `ghost-complete install` to deploy specs)");
    }

    if !js_commands.is_empty() {
        println!(
            "\nCommands with requires_js generators ({}):",
            js_commands.len()
        );
        for cmd in &js_commands {
            println!("  {cmd}");
        }
    }

    Ok(())
}
