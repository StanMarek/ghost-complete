use std::path::PathBuf;

/// Resolve spec directories using the same heuristics as the proxy:
/// explicit config paths first (with `~/` expansion), then
/// `~/.config/ghost-complete/specs` as the fallback.
pub(crate) fn resolve_spec_dirs(config: &gc_config::GhostConfig) -> Vec<PathBuf> {
    if !config.paths.spec_dirs.is_empty() {
        return config
            .paths
            .spec_dirs
            .iter()
            .map(|s| {
                if s.starts_with('~') {
                    if let Some(home) = dirs::home_dir() {
                        return home.join(s.strip_prefix("~/").unwrap_or(s));
                    }
                }
                PathBuf::from(s)
            })
            .collect();
    }

    let mut dirs = Vec::new();
    if let Some(config_dir) = gc_config::config_dir() {
        dirs.push(config_dir.join("specs"));
    }
    dirs
}
