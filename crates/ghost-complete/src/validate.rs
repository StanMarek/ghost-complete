use std::path::Path;

use anyhow::{Context, Result};

use crate::spec_dirs::resolve_spec_dirs;

fn count_spec_items(spec: &gc_suggest::CompletionSpec) -> (usize, usize) {
    fn count_subcommands(subs: &[gc_suggest::specs::SubcommandSpec]) -> usize {
        let mut n = subs.len();
        for sub in subs {
            n += count_subcommands(&sub.subcommands);
        }
        n
    }

    let subcommands = count_subcommands(&spec.subcommands);
    let options = spec.options.len();
    (subcommands, options)
}

fn validate_dir(dir: &Path) -> Result<(usize, usize)> {
    let mut valid = 0;
    let mut failed = 0;

    if !dir.exists() {
        println!("  Directory does not exist: {}\n", dir.display());
        return Ok((0, 0));
    }

    let mut entries = Vec::new();
    let mut unreadable = 0usize;
    for entry_result in std::fs::read_dir(dir)
        .with_context(|| format!("failed to read directory: {}", dir.display()))?
    {
        match entry_result {
            Ok(e) => {
                if e.path().extension().and_then(|ext| ext.to_str()) == Some("json") {
                    entries.push(e);
                }
            }
            Err(e) => {
                unreadable += 1;
                tracing::warn!("failed to read directory entry in {}: {e}", dir.display());
            }
        }
    }
    entries.sort_by_key(|e| e.file_name());

    if unreadable > 0 {
        println!("  \x1b[33m{unreadable} file(s) could not be read\x1b[0m");
        failed += unreadable;
    }

    for entry in entries {
        let path = entry.path();
        let file_name = path.file_name().unwrap_or_default().to_string_lossy();

        let contents = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                println!("  \x1b[31mFAIL\x1b[0m  {file_name}: {e}");
                failed += 1;
                continue;
            }
        };

        match serde_json::from_str::<gc_suggest::CompletionSpec>(&contents) {
            Ok(spec) => {
                let (subs, opts) = count_spec_items(&spec);
                println!("  \x1b[32m OK \x1b[0m  {file_name} ({subs} subcommands, {opts} options)");
                valid += 1;
            }
            Err(e) => {
                println!("  \x1b[31mFAIL\x1b[0m  {file_name}: {e}");
                failed += 1;
            }
        }
    }

    Ok((valid, failed))
}

pub fn run_validate_specs(config_path: Option<&str>) -> Result<()> {
    let config = gc_config::GhostConfig::load(config_path).context("failed to load config")?;

    let dirs = resolve_spec_dirs(&config);
    let mut total_valid = 0;
    let mut total_failed = 0;

    for dir in &dirs {
        println!("Validating specs in {}\n", dir.display());
        let (valid, failed) = validate_dir(dir)?;
        total_valid += valid;
        total_failed += failed;
    }

    if dirs.is_empty() {
        println!("No spec directories found.");
        return Ok(());
    }

    let total = total_valid + total_failed;
    println!();
    if total_failed == 0 {
        println!("{total}/{total} specs valid.");
    } else {
        println!("{total_valid}/{total} specs valid, {total_failed} failed.");
    }

    if total_failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}
