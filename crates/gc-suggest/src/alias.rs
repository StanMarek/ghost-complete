//! Shell alias loading.
//!
//! Aliases let the engine resolve `g` → `git`, `gco` → `git checkout` so that
//! a user-customized command lands the right spec lookup. The slow-path probe
//! shells out (`zsh -c alias`) which can take 300–500ms with oh-my-zsh on a
//! cold start — well over the documented <100ms startup budget. To stay under
//! budget the [`AliasStore`] returned at startup is empty and a background
//! thread runs the probe; the first few keystrokes after launch may not see
//! alias expansion. Once the background thread completes, every subsequent
//! suggestion request will see the aliases as soon as the write lock is
//! released. See audit MED-24.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Lazy alias map populated by a background loader.
///
/// Reads (`get`) take a non-blocking [`RwLock`] read guard so concurrent
/// suggestion lookups never serialize against each other. The single
/// background loader thread takes the write lock briefly, just long enough
/// to swap in the populated map.
#[derive(Clone, Default)]
pub struct AliasStore {
    inner: Arc<RwLock<HashMap<String, String>>>,
}

impl AliasStore {
    /// Construct a store and immediately spawn a background thread to run
    /// [`load_shell_aliases`]. The store is observable as empty until the
    /// thread completes — this is a deliberate trade-off so startup never
    /// blocks on a slow shell probe (audit MED-24).
    pub fn load_async() -> Self {
        let store = Self::default();
        let inner = Arc::clone(&store.inner);
        std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let map = load_shell_aliases();
            let count = map.len();
            {
                let mut guard = inner.write().unwrap_or_else(|e| e.into_inner());
                *guard = map;
            }
            tracing::info!(
                "loaded {count} shell aliases in {}ms (background)",
                started.elapsed().as_millis()
            );
        });
        store
    }

    /// Build an empty store with no background load. Used by tests and the
    /// `with_providers` engine constructor.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Look up an alias and return the resolved command name. Returns `None`
    /// if the alias isn't known *or* if the background loader hasn't filled
    /// the store yet.
    pub fn get(&self, name: &str) -> Option<String> {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        guard.get(name).cloned()
    }

    /// Number of aliases currently in the store.
    pub fn len(&self) -> usize {
        self.inner.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Test/fixture helper — synchronously install a pre-built map. Mirrors
    /// what the background loader does on completion. Crate-private so
    /// production code keeps using `load_async`.
    #[cfg(test)]
    pub(crate) fn populate(&self, map: HashMap<String, String>) {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        *guard = map;
    }
}

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

    // Slow path: non-interactive subprocess with 2-second timeout.
    // Uses try_wait polling to avoid blocking indefinitely on a hanging .zshenv.
    for shell in &["zsh", "bash"] {
        tracing::debug!("spawning {shell} -c alias");
        let mut child = match std::process::Command::new(shell)
            .args(["-c", "alias"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("failed to spawn {shell}: {e}");
                continue;
            }
        };

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break Some(s),
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        tracing::debug!("{shell} alias timed out, killing");
                        let _ = child.kill();
                        let _ = child.wait();
                        break None;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => {
                    tracing::debug!("{shell} alias wait error: {e}");
                    break None;
                }
            }
        };

        if let Some(s) = status {
            if s.success() {
                if let Some(mut stdout) = child.stdout.take() {
                    use std::io::Read;
                    let mut text = String::new();
                    if stdout.read_to_string(&mut text).is_ok() {
                        let aliases = parse_aliases(&text);
                        if !aliases.is_empty() {
                            tracing::debug!("loaded {} aliases from {shell} -c", aliases.len());
                            return aliases;
                        }
                    }
                }
            } else {
                tracing::debug!("{shell} alias command failed: {s}");
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

    #[test]
    fn alias_store_starts_empty_then_fills() {
        // Models the production async-fill flow: at startup the store is
        // empty so suggestion lookups return None; once the background
        // loader populates it (`populate` here stands in for what the
        // background thread does), the same lookups must hit.
        let store = AliasStore::empty();
        assert!(store.is_empty(), "fresh store must be empty");
        assert_eq!(store.get("gco"), None);

        let mut map = HashMap::new();
        map.insert("gco".to_string(), "git".to_string());
        map.insert("k".to_string(), "kubectl".to_string());
        store.populate(map);

        assert_eq!(store.len(), 2);
        assert_eq!(store.get("gco"), Some("git".to_string()));
        assert_eq!(store.get("k"), Some("kubectl".to_string()));
        assert_eq!(store.get("not-an-alias"), None);
    }

    #[test]
    fn alias_store_clones_share_storage() {
        // Cloning the store must alias the same backing map — otherwise
        // two callers (e.g. the engine and a future cache invalidator)
        // would observe divergent state once the loader fills.
        let store = AliasStore::empty();
        let store2 = store.clone();
        store.populate(HashMap::from([("g".to_string(), "git".to_string())]));
        assert_eq!(store2.get("g"), Some("git".to_string()));
    }
}
