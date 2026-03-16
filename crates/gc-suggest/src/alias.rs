use std::collections::HashMap;

/// Parse shell alias definitions into a map from alias name to resolved command.
///
/// Supports the output format of `alias` in zsh/bash:
/// - zsh: `name=value` or `name='value'`
/// - bash: `alias name='value'` or `alias name="value"`
///
/// Only extracts the first word of the resolved value (the command name).
pub fn parse_aliases(output: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Strip "alias " prefix (bash format)
        let line = line.strip_prefix("alias ").unwrap_or(line);

        // Find the = separator
        let eq_idx = match line.find('=') {
            Some(i) => i,
            None => continue,
        };

        let alias_name = line[..eq_idx].trim();
        if alias_name.is_empty() {
            continue;
        }

        let mut value = line[eq_idx + 1..].trim();

        // Strip surrounding quotes
        if (value.starts_with('\'') && value.ends_with('\''))
            || (value.starts_with('"') && value.ends_with('"'))
        {
            value = &value[1..value.len() - 1];
        }

        // Extract the first word (the command name)
        let command = value.split_whitespace().next().unwrap_or("");
        if command.is_empty() {
            continue;
        }

        map.insert(alias_name.to_string(), command.to_string());
    }

    map
}

/// Load aliases by reading common alias dotfiles, falling back to a
/// non-interactive shell subprocess.
///
/// Prefers file-based reads (instant) over subprocess spawning to stay
/// within the <100ms startup budget. Uses `zsh -c` (not `-ic`) to avoid
/// loading the full interactive config which can take 200-400ms with
/// oh-my-zsh/plugins.
pub fn load_shell_aliases() -> HashMap<String, String> {
    // Fast path: read alias dotfiles directly (no subprocess)
    if let Some(home) = dirs::home_dir() {
        for file in &[".zsh_aliases", ".aliases", ".bash_aliases"] {
            let path = home.join(file);
            if let Ok(contents) = std::fs::read_to_string(&path) {
                let aliases = parse_aliases(&contents);
                if !aliases.is_empty() {
                    tracing::debug!("loaded {} aliases from {}", aliases.len(), path.display());
                    return aliases;
                }
            }
        }
    }

    // Slow path: non-interactive subprocess (only gets .zshenv aliases)
    for shell in &["zsh", "bash"] {
        let result = std::process::Command::new(shell)
            .args(["-c", "alias"])
            .output();

        match result {
            Ok(output) if output.status.success() => {
                let text = String::from_utf8_lossy(&output.stdout);
                let aliases = parse_aliases(&text);
                if !aliases.is_empty() {
                    tracing::debug!("loaded {} aliases from {shell} -c", aliases.len());
                    return aliases;
                }
            }
            Ok(output) => {
                tracing::debug!("{shell} alias command failed: {:?}", output.status);
            }
            Err(e) => {
                tracing::debug!("failed to run {shell}: {e}");
            }
        }
    }

    tracing::debug!("no aliases loaded from any source");
    HashMap::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_zsh_aliases() {
        let output = "\
g=git
k=kubectl
ll='ls -la'
";
        let aliases = parse_aliases(output);
        assert_eq!(aliases.get("g"), Some(&"git".to_string()));
        assert_eq!(aliases.get("k"), Some(&"kubectl".to_string()));
        assert_eq!(aliases.get("ll"), Some(&"ls".to_string()));
    }

    #[test]
    fn test_parse_bash_aliases() {
        let output = "\
alias g='git'
alias k='kubectl'
alias ll='ls -la'
";
        let aliases = parse_aliases(output);
        assert_eq!(aliases.get("g"), Some(&"git".to_string()));
        assert_eq!(aliases.get("k"), Some(&"kubectl".to_string()));
        assert_eq!(aliases.get("ll"), Some(&"ls".to_string()));
    }

    #[test]
    fn test_parse_double_quoted() {
        let output = "alias g=\"git\"\n";
        let aliases = parse_aliases(output);
        assert_eq!(aliases.get("g"), Some(&"git".to_string()));
    }

    #[test]
    fn test_parse_empty_value_skipped() {
        let output = "empty=\n";
        let aliases = parse_aliases(output);
        assert!(!aliases.contains_key("empty"));
    }

    #[test]
    fn test_parse_empty_output() {
        let aliases = parse_aliases("");
        assert!(aliases.is_empty());
    }

    #[test]
    fn test_parse_complex_value_extracts_first_word() {
        let output = "glog='git log --oneline --graph'\n";
        let aliases = parse_aliases(output);
        assert_eq!(aliases.get("glog"), Some(&"git".to_string()));
    }

    #[test]
    fn test_parse_no_equals_skipped() {
        let output = "not an alias line\n";
        let aliases = parse_aliases(output);
        assert!(aliases.is_empty());
    }
}
