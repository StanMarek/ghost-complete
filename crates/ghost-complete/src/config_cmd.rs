use anyhow::{Context, Result};

pub fn run_config(config_path: Option<&str>) -> Result<()> {
    let config = gc_config::GhostConfig::load(config_path).context("failed to load config")?;
    let toml_str = toml::to_string_pretty(&config).context("failed to serialize config")?;
    println!("{toml_str}");
    Ok(())
}
