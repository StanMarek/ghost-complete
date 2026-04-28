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
//! released.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// Cache file name (inside the state directory).
const ALIAS_CACHE_FILE: &str = "aliases-cache.json";

/// On-disk schema version; bump on incompatible CachedAliases changes.
const CURRENT_ALIAS_CACHE_VERSION: u32 = 2;

/// Source dotfiles whose mtimes invalidate the alias cache. If any exists
/// with an mtime newer than the cache file, we regenerate. Includes every
/// file the load paths in [`load_shell_aliases`] actually consult:
///
///   * shell rc files (run on every shell start, may `alias` directly)
///   * `*.local` per-host overrides that our own users often add
///   * dedicated alias files the fast path reads directly
///   * `~/.bash_profile` (login-shell counterpart to `.bashrc`)
///
/// Missing the file-read fast-path files here is the bug that let a user
/// edit `~/.zsh_aliases` and see a stale cache forever.
const ALIAS_SOURCE_FILES: &[&str] = &[
    ".zshrc",
    ".zshrc.local",
    ".zshenv",
    ".zshenv.local",
    ".zprofile",
    ".bashrc",
    ".bash_profile",
    ".zsh_aliases",
    ".aliases",
    ".bash_aliases",
];

/// Directories whose mtime (i.e. child add/remove/rename) invalidates the
/// alias cache. We use directory mtime rather than recursing: it flips on
/// any entry change, which catches the user adding/removing a custom-alias
/// drop-in in oh-my-zsh without us having to scan every file on every launch.
const ALIAS_SOURCE_DIRS: &[&str] = &[".oh-my-zsh/custom", ".config/fish/functions"];

/// Fingerprint of a watched source file. We pair mtime-seconds with
/// subsecond precision AND the file length so rapid in-place edits inside
/// the same wall-clock second still invalidate the cache. Dropping nanos
/// (the original shape here) let a user edit `.zshrc` twice within one
/// second and miss the second edit — the history/ssh caches in this crate
/// already use `(mtime, len)` for exactly this reason.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone, Copy)]
struct SourceFingerprint {
    secs: u64,
    #[serde(default)]
    nanos: u32,
    #[serde(default)]
    len: u64,
}

#[derive(Serialize, Deserialize)]
struct CachedAliases {
    /// Bump on incompatible CachedAliases shape changes; mismatch forces regeneration.
    #[serde(default)]
    format_version: u32,
    /// Maps each watched source file (by basename) to its fingerprint at
    /// the time of capture. On load, we compare the current fingerprint
    /// against the stored value: any difference invalidates.
    source_mtimes: HashMap<String, SourceFingerprint>,
    aliases: HashMap<String, Vec<String>>,
}

/// Resolve the state directory path for `aliases-cache.json`. Mirrors the
/// frecency resolution: prefers `$XDG_STATE_HOME/ghost-complete`, falls
/// back to `~/.local/state/ghost-complete`.
fn alias_cache_path() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
        if !xdg.is_empty() {
            return Some(
                PathBuf::from(xdg)
                    .join("ghost-complete")
                    .join(ALIAS_CACHE_FILE),
            );
        }
    }
    dirs::home_dir().map(|h| {
        h.join(".local")
            .join("state")
            .join("ghost-complete")
            .join(ALIAS_CACHE_FILE)
    })
}

fn file_fingerprint(path: &Path) -> Option<SourceFingerprint> {
    let meta = std::fs::metadata(path).ok()?;
    let mt = meta.modified().ok()?;
    let d = mt.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    Some(SourceFingerprint {
        secs: d.as_secs(),
        nanos: d.subsec_nanos(),
        len: meta.len(),
    })
}

/// Fingerprint a directory tree using the same `(secs, nanos, len)` shape
/// we use for files. Walks children under the same budget as the original
/// original walker and keeps the largest (`secs`, then `nanos`, then
/// `len`) tuple, so any edit inside the tree advances the fingerprint.
fn dir_tree_fingerprint(root: &Path) -> Option<SourceFingerprint> {
    let mut best = file_fingerprint(root)?;
    let mut stack: Vec<(PathBuf, u32)> = vec![(root.to_path_buf(), 0)];
    let mut files_seen: u32 = 0;

    while let Some((dir, depth)) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            if files_seen >= DIR_WALK_MAX_FILES {
                return Some(best);
            }
            files_seen += 1;

            let path = entry.path();
            if let Some(fp) = file_fingerprint(&path) {
                if (fp.secs, fp.nanos, fp.len) > (best.secs, best.nanos, best.len) {
                    best = fp;
                }
            }

            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            if is_dir && depth + 1 < DIR_WALK_MAX_DEPTH {
                stack.push((path, depth + 1));
            }
        }
    }

    Some(best)
}

/// Bounds for [`dir_tree_fingerprint`]. Covers a ~normal oh-my-zsh
/// installation (custom drop-ins + a few plugin subdirs) without letting a
/// pathological layout turn startup into a deep FS walk.
const DIR_WALK_MAX_DEPTH: u32 = 3;
const DIR_WALK_MAX_FILES: u32 = 500;

fn collect_source_mtimes(home: &Path) -> HashMap<String, SourceFingerprint> {
    let mut out = HashMap::new();
    for name in ALIAS_SOURCE_FILES {
        let p = home.join(name);
        if let Some(fp) = file_fingerprint(&p) {
            out.insert((*name).to_string(), fp);
        }
    }
    for name in ALIAS_SOURCE_DIRS {
        let p = home.join(name);
        // Recursive max-fingerprint: the directory's own mtime only flips
        // on add/remove/rename, so it would miss a user editing an
        // existing drop-in like `~/.oh-my-zsh/custom/aliases.zsh` in
        // place.
        if let Some(fp) = dir_tree_fingerprint(&p) {
            out.insert((*name).to_string(), fp);
        }
    }
    out
}

/// Attempt to load the alias map from the on-disk cache. Returns `None`
/// when the cache is missing, unreadable, malformed, stale w.r.t. any
/// watched source file, or written by a different schema version.
fn load_alias_cache(home: &Path, cache_path: &Path) -> Option<HashMap<String, Vec<String>>> {
    let contents = std::fs::read_to_string(cache_path).ok()?;
    let cached: CachedAliases = serde_json::from_str(&contents).ok()?;
    if cached.format_version != CURRENT_ALIAS_CACHE_VERSION {
        return None;
    }
    let current = collect_source_mtimes(home);
    // Any watched file newer than the cache record means regenerate.
    // Also regenerate if a file disappeared or appeared since we cached.
    if current != cached.source_mtimes {
        return None;
    }
    Some(cached.aliases)
}

/// Write the alias map plus the current source mtimes to the cache file.
/// Uses atomic write (tmp + rename). Best-effort: any failure is logged
/// at debug and ignored.
fn save_alias_cache(home: &Path, cache_path: &Path, aliases: &HashMap<String, Vec<String>>) {
    if aliases.is_empty() {
        // Don't cache empty results — next startup retries from scratch.
        return;
    }
    let parent = match cache_path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    if let Err(e) = std::fs::create_dir_all(&parent) {
        tracing::warn!("alias cache dir creation failed: {e}");
        return;
    }
    let payload = CachedAliases {
        format_version: CURRENT_ALIAS_CACHE_VERSION,
        source_mtimes: collect_source_mtimes(home),
        aliases: aliases.clone(),
    };
    let json = match serde_json::to_string(&payload) {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!("alias cache serialize error: {e}");
            return;
        }
    };
    // A unique per-process temp file in the destination's parent dir keeps
    // `persist()` a single atomic same-FS rename. A shared-name tmp path
    // (e.g. `aliases-cache.json.tmp`) would let a pre-seeded symlink at
    // that location redirect the write to an arbitrary target, and two
    // concurrent launches would race each other.
    let mut tmp = match tempfile::NamedTempFile::new_in(&parent) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("alias cache tmp create failed: {e}");
            return;
        }
    };
    if let Err(e) = std::io::Write::write_all(tmp.as_file_mut(), json.as_bytes()) {
        tracing::warn!("alias cache write failed at {}: {e}", cache_path.display());
        return;
    }
    if let Err(e) = tmp.persist(cache_path) {
        tracing::warn!(
            "alias cache persist failed at {}: {e}",
            cache_path.display()
        );
    }
}

/// Lazy alias map populated by a background loader.
///
/// Reads (`get`) take a non-blocking [`RwLock`] read guard so concurrent
/// suggestion lookups never serialize against each other. The single
/// background loader thread takes the write lock briefly, just long enough
/// to swap in the populated map.
#[derive(Clone, Default)]
pub struct AliasStore {
    inner: Arc<RwLock<HashMap<String, Vec<String>>>>,
}

impl AliasStore {
    /// Construct a store and immediately spawn a background thread to run
    /// [`load_shell_aliases`]. The store is observable as empty until the
    /// thread completes — this is a deliberate trade-off so startup never
    /// blocks on a slow shell probe.
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

    /// Returns the full token vector for `name`, or None if absent or loader still pending.
    pub fn get(&self, name: &str) -> Option<Vec<String>> {
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
    pub(crate) fn populate(&self, map: HashMap<String, Vec<String>>) {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        *guard = map;
    }

    #[doc(hidden)]
    pub fn install(&self, map: HashMap<String, Vec<String>>) {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        *guard = map;
    }
}

/// Parse zsh/bash `alias` output into name -> token-vector pairs; full tokens preserved via shlex.
pub fn parse_aliases(output: &str) -> HashMap<String, Vec<String>> {
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

        let tokens = match shlex::split(value) {
            Some(toks) if !toks.is_empty() => toks,
            Some(_) => continue, // shlex parsed but produced nothing — empty value
            None => {
                // Malformed shlex parse: keep raw whitespace tokens so the alias still surfaces partially.
                tracing::debug!("shlex failed to parse alias value for {alias_name:?}: {value:?}");
                let fallback: Vec<String> = value.split_whitespace().map(String::from).collect();
                if fallback.is_empty() {
                    continue;
                }
                fallback
            }
        };

        map.insert(alias_name.to_string(), tokens);
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
pub fn load_shell_aliases() -> HashMap<String, Vec<String>> {
    let home = dirs::home_dir();
    let cache_path = alias_cache_path();

    // Fastest path: on-disk cache keyed by rc-file mtimes. The subprocess
    // spawn (`zsh -c alias`) is 300–500ms on oh-my-zsh setups, which blows
    // the <100ms startup budget by 3–5×. A cache invalidated on rc-file
    // edits makes startup nearly free on every subsequent run.
    if let (Some(h), Some(cp)) = (home.as_ref(), cache_path.as_ref()) {
        if let Some(cached) = load_alias_cache(h, cp) {
            tracing::debug!("loaded {} aliases from disk cache", cached.len());
            return cached;
        }
    }

    // Fast path: read alias dotfiles directly (no subprocess)
    if let Some(ref home) = home {
        for file in &[".zsh_aliases", ".aliases", ".bash_aliases"] {
            let path = home.join(file);
            if let Ok(contents) = std::fs::read_to_string(&path) {
                let aliases = parse_aliases(&contents);
                if !aliases.is_empty() {
                    tracing::debug!("loaded {} aliases from {}", aliases.len(), path.display());
                    if let Some(ref cp) = cache_path {
                        save_alias_cache(home, cp, &aliases);
                    }
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
                            if let (Some(h), Some(cp)) = (home.as_ref(), cache_path.as_ref()) {
                                save_alias_cache(h, cp, &aliases);
                            }
                            return aliases;
                        }
                    }
                }
            } else {
                tracing::debug!("{shell} alias command failed: {s}");
            }
        }
    }

    // "No aliases loaded" is a legitimate empty state — a user may simply
    // have no aliases defined. Keep this at debug level so it doesn't
    // pollute normal logs.
    tracing::debug!("no aliases loaded from any source");
    HashMap::new()
}

#[cfg(test)]
fn token_vec(tokens: &[&str]) -> Vec<String> {
    tokens.iter().map(|s| (*s).to_string()).collect()
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
        assert_eq!(aliases.get("g"), Some(&token_vec(&["git"])));
        assert_eq!(aliases.get("k"), Some(&token_vec(&["kubectl"])));
        assert_eq!(aliases.get("ll"), Some(&token_vec(&["ls", "-la"])));
    }

    #[test]
    fn test_parse_bash_aliases() {
        let output = "\
alias g='git'
alias k='kubectl'
alias ll='ls -la'
";
        let aliases = parse_aliases(output);
        assert_eq!(aliases.get("g"), Some(&token_vec(&["git"])));
        assert_eq!(aliases.get("k"), Some(&token_vec(&["kubectl"])));
        assert_eq!(aliases.get("ll"), Some(&token_vec(&["ls", "-la"])));
    }

    #[test]
    fn test_parse_double_quoted() {
        let output = "alias g=\"git\"\n";
        let aliases = parse_aliases(output);
        assert_eq!(aliases.get("g"), Some(&token_vec(&["git"])));
    }

    #[test]
    fn test_parse_empty_value_skipped() {
        let output = "empty=\n";
        let aliases = parse_aliases(output);
        assert!(!aliases.contains_key("empty"));
    }

    #[test]
    fn test_parse_empty_quoted_value_skipped() {
        let output = "alias x=''\nalias y=\"\"\nalias z=' '\n";
        let aliases = parse_aliases(output);
        assert!(!aliases.contains_key("x"));
        assert!(!aliases.contains_key("y"));
        assert!(!aliases.contains_key("z"));
    }

    #[test]
    fn test_parse_quoted_value_with_padding_trimmed() {
        let output = "alias k=' kubectl '\n";
        let aliases = parse_aliases(output);
        assert_eq!(aliases.get("k"), Some(&token_vec(&["kubectl"])));
    }

    #[test]
    fn test_parse_keeps_dollar_var_as_literal_token() {
        let output = "k='kubectl --context $CTX'\n";
        let aliases = parse_aliases(output);
        assert_eq!(
            aliases.get("k"),
            Some(&token_vec(&["kubectl", "--context", "$CTX"]))
        );
    }

    #[test]
    fn test_parse_empty_output() {
        let aliases = parse_aliases("");
        assert!(aliases.is_empty());
    }

    #[test]
    fn test_parse_complex_value_keeps_full_tokens() {
        let output = "glog='git log --oneline --graph'\n";
        let aliases = parse_aliases(output);
        assert_eq!(
            aliases.get("glog"),
            Some(&token_vec(&["git", "log", "--oneline", "--graph"]))
        );
    }

    #[test]
    fn test_parse_double_quoted_with_inner_spaces() {
        let output = "commit='git commit -m \"wip commit\"'\n";
        let aliases = parse_aliases(output);
        assert_eq!(
            aliases.get("commit"),
            Some(&token_vec(&["git", "commit", "-m", "wip commit"]))
        );
    }

    #[test]
    fn test_parse_escaped_space() {
        let output = "gx='git foo\\ bar'\n";
        let aliases = parse_aliases(output);
        assert_eq!(aliases.get("gx"), Some(&token_vec(&["git", "foo bar"])));
    }

    #[test]
    fn test_parse_falls_back_on_unbalanced_quote() {
        let output = "broken=git \"open\nok=ls\n";
        let aliases = parse_aliases(output);
        assert_eq!(
            aliases.get("broken"),
            Some(&token_vec(&["git", "\"open"])),
            "fallback must preserve every token, not just the first"
        );
        assert_eq!(
            aliases.get("ok"),
            Some(&token_vec(&["ls"])),
            "a single corrupt alias must not drop later entries"
        );
    }

    #[test]
    fn test_parse_single_word_unchanged() {
        let output = "ll=ls\n";
        let aliases = parse_aliases(output);
        assert_eq!(aliases.get("ll"), Some(&token_vec(&["ls"])));
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
        map.insert("gco".to_string(), token_vec(&["git", "checkout"]));
        map.insert("k".to_string(), token_vec(&["kubectl"]));
        store.populate(map);

        assert_eq!(store.len(), 2);
        assert_eq!(store.get("gco"), Some(token_vec(&["git", "checkout"])));
        assert_eq!(store.get("k"), Some(token_vec(&["kubectl"])));
        assert_eq!(store.get("not-an-alias"), None);
    }

    #[test]
    fn alias_store_clones_share_storage() {
        // Cloning the store must alias the same backing map — otherwise
        // two callers (e.g. the engine and a future cache invalidator)
        // would observe divergent state once the loader fills.
        let store = AliasStore::empty();
        let store2 = store.clone();
        store.populate(HashMap::from([("g".to_string(), token_vec(&["git"]))]));
        assert_eq!(store2.get("g"), Some(token_vec(&["git"])));
    }

    #[test]
    fn alias_cache_roundtrip_and_invalidation() {
        // Fake $HOME with a .zshrc; save cache; reload cache; assert hit.
        // Then bump .zshrc mtime forward and assert the cache is rejected.
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(".zshrc"), b"# empty\n").unwrap();

        let cache_path = home.path().join("aliases-cache.json");

        let mut aliases = HashMap::new();
        aliases.insert("g".to_string(), token_vec(&["git"]));
        aliases.insert("k".to_string(), token_vec(&["kubectl"]));
        save_alias_cache(home.path(), &cache_path, &aliases);
        assert!(cache_path.exists(), "cache file must be written");

        let loaded = load_alias_cache(home.path(), &cache_path).expect("cache should load cleanly");
        assert_eq!(loaded, aliases, "loaded cache must match saved");

        // Bump the source file's mtime forward — cache should reject.
        let future = SystemTime::now() + std::time::Duration::from_secs(60);
        filetime::set_file_mtime(
            home.path().join(".zshrc"),
            filetime::FileTime::from_system_time(future),
        )
        .unwrap();

        assert!(
            load_alias_cache(home.path(), &cache_path).is_none(),
            "cache must be rejected after source file changes"
        );
    }

    #[test]
    fn alias_cache_skips_empty_result() {
        // Empty alias maps must not be cached — otherwise an early
        // subprocess failure would persist as "no aliases" forever.
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(".zshrc"), b"# empty\n").unwrap();
        let cache_path = home.path().join("aliases-cache.json");

        save_alias_cache(home.path(), &cache_path, &HashMap::new());
        assert!(
            !cache_path.exists(),
            "empty alias results must not be cached"
        );
    }

    #[test]
    fn alias_cache_invalidates_when_new_source_appears() {
        // Cache is written with no .zshenv. Later, .zshenv is created —
        // even if .zshrc mtime is unchanged, the appearance of a new
        // source file must invalidate the cache.
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(".zshrc"), b"# empty\n").unwrap();
        let cache_path = home.path().join("aliases-cache.json");

        let mut aliases = HashMap::new();
        aliases.insert("g".to_string(), token_vec(&["git"]));
        save_alias_cache(home.path(), &cache_path, &aliases);
        assert!(load_alias_cache(home.path(), &cache_path).is_some());

        // Create .zshenv after the cache was saved.
        std::fs::write(home.path().join(".zshenv"), b"# new file\n").unwrap();
        assert!(
            load_alias_cache(home.path(), &cache_path).is_none(),
            "new source file must invalidate cache"
        );
    }

    #[test]
    fn alias_cache_tracks_every_file_the_fast_path_reads() {
        // Regression guard: the file-read fast path in `load_shell_aliases`
        // reads `.zsh_aliases`, `.aliases`, and `.bash_aliases`. If those
        // filenames are missing from ALIAS_SOURCE_FILES, a user can edit
        // them and never see the cache invalidate.
        for fast_path_file in [".zsh_aliases", ".aliases", ".bash_aliases"] {
            assert!(
                ALIAS_SOURCE_FILES.contains(&fast_path_file),
                "ALIAS_SOURCE_FILES must include {fast_path_file} (read by the file-based fast path)"
            );
        }
    }

    #[test]
    fn alias_cache_invalidates_when_zsh_aliases_edited() {
        // The fast path at the top of load_shell_aliases reads .zsh_aliases
        // directly and caches the result. Editing .zsh_aliases must be
        // reflected by a cache miss on the next load.
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join(".zsh_aliases"),
            b"alias g='git'\nalias k='kubectl'\n",
        )
        .unwrap();
        let cache_path = home.path().join("aliases-cache.json");

        let mut aliases = HashMap::new();
        aliases.insert("g".to_string(), token_vec(&["git"]));
        save_alias_cache(home.path(), &cache_path, &aliases);
        assert!(load_alias_cache(home.path(), &cache_path).is_some());

        let future = SystemTime::now() + std::time::Duration::from_secs(60);
        filetime::set_file_mtime(
            home.path().join(".zsh_aliases"),
            filetime::FileTime::from_system_time(future),
        )
        .unwrap();

        assert!(
            load_alias_cache(home.path(), &cache_path).is_none(),
            "editing .zsh_aliases must invalidate the cache"
        );
    }

    #[test]
    fn alias_cache_invalidates_when_existing_omz_dropin_is_edited() {
        // Editing an existing file inside a tracked directory does NOT bump
        // the directory's own mtime. The recursive max-fingerprint walk in
        // `dir_tree_fingerprint` must catch this — otherwise edits to
        // `~/.oh-my-zsh/custom/aliases.zsh` slip through.
        let home = tempfile::tempdir().unwrap();
        let custom = home.path().join(".oh-my-zsh/custom");
        std::fs::create_dir_all(&custom).unwrap();
        let dropin = custom.join("aliases.zsh");
        std::fs::write(&dropin, b"alias g=git\n").unwrap();

        // Force the directory's own mtime into the past so we can prove
        // later that the cache miss comes from the file edit, not from
        // the directory metadata changing.
        let past = SystemTime::now() - std::time::Duration::from_secs(3600);
        filetime::set_file_mtime(&custom, filetime::FileTime::from_system_time(past)).unwrap();
        filetime::set_file_mtime(&dropin, filetime::FileTime::from_system_time(past)).unwrap();

        let cache_path = home.path().join("aliases-cache.json");
        let mut aliases = HashMap::new();
        aliases.insert("g".to_string(), token_vec(&["git"]));
        save_alias_cache(home.path(), &cache_path, &aliases);
        assert!(load_alias_cache(home.path(), &cache_path).is_some());

        // Edit the existing drop-in without touching the directory. Only
        // the file mtime advances; the containing directory mtime does
        // not (that's exactly the case the naive stat-the-dir approach
        // missed).
        let future = SystemTime::now() + std::time::Duration::from_secs(60);
        filetime::set_file_mtime(&dropin, filetime::FileTime::from_system_time(future)).unwrap();
        filetime::set_file_mtime(&custom, filetime::FileTime::from_system_time(past)).unwrap();

        assert!(
            load_alias_cache(home.path(), &cache_path).is_none(),
            "editing an existing drop-in inside a tracked dir must invalidate the cache"
        );
    }

    #[test]
    fn alias_cache_invalidates_on_same_second_subsecond_edit() {
        // Regression for the seconds-only-mtime bug: two edits to the
        // same file within one wall-clock second must still be noticed
        // because `SourceFingerprint` tracks nanos + length.
        let home = tempfile::tempdir().unwrap();
        let rc = home.path().join(".zshrc");
        std::fs::write(&rc, b"a").unwrap();
        let cache_path = home.path().join("aliases-cache.json");

        let mut aliases = HashMap::new();
        aliases.insert("g".to_string(), token_vec(&["git"]));
        save_alias_cache(home.path(), &cache_path, &aliases);
        assert!(load_alias_cache(home.path(), &cache_path).is_some());

        // Change length without advancing the mtime second — forces the
        // fingerprint to rely on `(nanos, len)` to detect the change.
        let fp = file_fingerprint(&rc).unwrap();
        std::fs::write(&rc, b"ab").unwrap();
        filetime::set_file_mtime(
            &rc,
            filetime::FileTime::from_unix_time(fp.secs as i64, fp.nanos),
        )
        .unwrap();

        assert!(
            load_alias_cache(home.path(), &cache_path).is_none(),
            "edit that preserves mtime-seconds but changes length must invalidate"
        );
    }

    #[test]
    fn dir_tree_fingerprint_walks_recursively_within_budget() {
        // Nested layout: custom/plugins/myplugin/myplugin.plugin.zsh.
        // The recursive walk must reach two levels down to surface an
        // edit there. Above DIR_WALK_MAX_DEPTH we deliberately stop.
        let tmp = tempfile::tempdir().unwrap();
        let leaf_dir = tmp.path().join("plugins/myplugin");
        std::fs::create_dir_all(&leaf_dir).unwrap();
        let leaf = leaf_dir.join("myplugin.plugin.zsh");
        std::fs::write(&leaf, b"alias x=y\n").unwrap();

        let before = dir_tree_fingerprint(tmp.path()).expect("walk must succeed");

        let future = SystemTime::now() + std::time::Duration::from_secs(120);
        filetime::set_file_mtime(&leaf, filetime::FileTime::from_system_time(future)).unwrap();

        let after = dir_tree_fingerprint(tmp.path()).expect("walk must succeed");
        assert!(
            (after.secs, after.nanos, after.len) > (before.secs, before.nanos, before.len),
            "fingerprint must advance after nested-file edit (before={before:?} after={after:?})"
        );
    }

    #[test]
    fn alias_cache_invalidates_when_omz_custom_dir_changes() {
        // oh-my-zsh users drop alias definitions into ~/.oh-my-zsh/custom/
        // as .zsh files. We don't recurse into each file, but we do watch
        // the directory mtime, which flips on any child add/remove/rename.
        let home = tempfile::tempdir().unwrap();
        let custom = home.path().join(".oh-my-zsh/custom");
        std::fs::create_dir_all(&custom).unwrap();
        std::fs::write(custom.join("aliases.zsh"), b"alias g=git\n").unwrap();

        let cache_path = home.path().join("aliases-cache.json");
        let mut aliases = HashMap::new();
        aliases.insert("g".to_string(), token_vec(&["git"]));
        save_alias_cache(home.path(), &cache_path, &aliases);
        assert!(load_alias_cache(home.path(), &cache_path).is_some());

        // Add a second drop-in; the dir mtime changes.
        std::fs::write(custom.join("work.zsh"), b"alias w=workflow\n").unwrap();
        let future = SystemTime::now() + std::time::Duration::from_secs(60);
        filetime::set_file_mtime(&custom, filetime::FileTime::from_system_time(future)).unwrap();

        assert!(
            load_alias_cache(home.path(), &cache_path).is_none(),
            "adding a drop-in to ~/.oh-my-zsh/custom must invalidate the cache"
        );
    }

    #[test]
    fn save_alias_cache_leaves_no_stale_tmp_files() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(".zshrc"), b"# empty\n").unwrap();
        let cache_path = home.path().join("aliases-cache.json");

        let mut aliases = HashMap::new();
        aliases.insert("g".to_string(), token_vec(&["git"]));
        save_alias_cache(home.path(), &cache_path, &aliases);

        assert!(cache_path.exists(), "cache file must be written");

        let leftover: Vec<_> = std::fs::read_dir(home.path())
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.ends_with(".tmp") || n.contains(".json.tmp"))
            .collect();
        assert!(
            leftover.is_empty(),
            "save_alias_cache must not leave stale temp files; found: {leftover:?}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn save_alias_cache_refuses_pre_seeded_shared_tmp_symlink() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(".zshrc"), b"# empty\n").unwrap();
        let cache_path = home.path().join("aliases-cache.json");

        let victim = home.path().join("victim.txt");
        std::fs::write(&victim, b"DO NOT CLOBBER").unwrap();

        let predictable = cache_path.with_extension("json.tmp");
        std::os::unix::fs::symlink(&victim, &predictable).unwrap();

        let mut aliases = HashMap::new();
        aliases.insert("g".to_string(), token_vec(&["git"]));
        save_alias_cache(home.path(), &cache_path, &aliases);

        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"DO NOT CLOBBER",
            "victim must be untouched by alias cache save"
        );
        assert!(
            cache_path.exists(),
            "cache must still be written to its real path"
        );
    }

    #[test]
    fn alias_cache_rejects_old_format_version() {
        // Pre-v2 cache (no format_version) must be rejected — silent deserialise would land empty Vecs.
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(".zshrc"), b"# empty\n").unwrap();
        let cache_path = home.path().join("aliases-cache.json");

        // Hand-craft a v1-shaped payload: no format_version, plain string values.
        let legacy = serde_json::json!({
            "source_mtimes": {
                ".zshrc": { "secs": 0, "nanos": 0, "len": 0 },
            },
            "aliases": { "g": "git" },
        });
        std::fs::write(&cache_path, legacy.to_string()).unwrap();

        assert!(
            load_alias_cache(home.path(), &cache_path).is_none(),
            "v1 cache (missing format_version) must be rejected on load"
        );

        // Sanity: an explicit older version is also rejected.
        let stale = serde_json::json!({
            "format_version": 1u32,
            "source_mtimes": {},
            "aliases": {},
        });
        std::fs::write(&cache_path, stale.to_string()).unwrap();
        assert!(
            load_alias_cache(home.path(), &cache_path).is_none(),
            "format_version != CURRENT_ALIAS_CACHE_VERSION must be rejected"
        );
    }

    #[test]
    fn concurrent_save_alias_cache_does_not_collide() {
        use std::sync::Arc;

        let home = Arc::new(tempfile::tempdir().unwrap());
        std::fs::write(home.path().join(".zshrc"), b"# empty\n").unwrap();
        let cache_path = home.path().join("aliases-cache.json");

        let mut handles = vec![];
        for t in 0..4 {
            let home = Arc::clone(&home);
            let cache_path = cache_path.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..20 {
                    let mut aliases = HashMap::new();
                    aliases.insert(format!("k_{t}_{i}"), token_vec(&["cmd"]));
                    save_alias_cache(home.path(), &cache_path, &aliases);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        assert!(cache_path.exists(), "final cache file must exist");
        let leftover: Vec<_> = std::fs::read_dir(home.path())
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(
            leftover.is_empty(),
            "concurrent saves must not leave temp files behind; found: {leftover:?}"
        );
    }
}
