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
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// Cache file name (inside the state directory).
const ALIAS_CACHE_FILE: &str = "aliases-cache.json";

/// On-disk schema version; bump on incompatible CachedAliases changes.
const CURRENT_ALIAS_CACHE_VERSION: u32 = 2;

/// Source dotfiles whose mtimes invalidate the alias cache. This is a
/// cache-invalidation watch set keyed by mtime, so only MEMBERSHIP matters
/// here — order is irrelevant. Membership must mirror [`STATIC_ALIAS_FILES`]
/// exactly: files the static parser doesn't read can never produce alias
/// entries, so watching them only forces useless regenerations, and files
/// the parser DOES read but that aren't watched here would silently let
/// stale caches persist (the bug that originally let a user edit
/// `~/.zsh_aliases` and see a stale cache forever).
const ALIAS_SOURCE_FILES: &[&str] = &[
    ".zshenv",
    ".zprofile",
    ".zshrc",
    ".zshrc.local",
    ".bashrc",
    ".bash_profile",
    ".zsh_aliases",
    ".aliases",
    ".bash_aliases",
];

/// Directories whose mtime (i.e. child add/remove/rename) invalidates the
/// alias cache. Same shape as [`ALIAS_SOURCE_FILES`] but for trees: only
/// drop-in dirs the parser actually walks earn a slot.
const ALIAS_SOURCE_DIRS: &[&str] = &[".oh-my-zsh/custom"];

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
/// walker and keeps the largest (`secs`, then `nanos`, then `len`) tuple,
/// so any edit inside the tree advances the fingerprint.
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

/// Parse a zsh-flavored alias file (`.zshrc`, `.zshenv`, drop-ins, etc.).
///
/// Recognises `alias name=value`, `alias name='quoted value'`, and
/// `alias name="quoted value"`. Names may contain ASCII alphanumerics,
/// `_`, `.`, and `-` (so `..`, `...`, and hyphenated names like `git-st`
/// parse), but a leading `-` is rejected so `alias -g/-s/-L`
/// global/suffix declarations stay out. Ignores comments, function
/// definitions, and conditional blocks. A `#` inside a quoted value is
/// preserved (it is only a comment when it sits outside quotes). Multi-line
/// values (trailing `\`) are skipped in full — every physical line of a
/// backslash-continued statement is dropped, since a parser that can't see
/// the whole expansion shouldn't guess at the tail.
pub(crate) fn parse_alias_file(src: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    // Tracks whether the previous physical line ended in a trailing `\` (we
    // treat any trailing backslash as a line continuation and skip
    // conservatively), so the continuation line is dropped too rather than
    // parsed as a standalone statement (which would invent a phantom alias
    // from the tail and lose the real one).
    let mut skip_continuation = false;
    for raw in src.lines() {
        if skip_continuation {
            // Keep skipping until a line does NOT continue.
            skip_continuation = raw.trim_end().ends_with('\\');
            continue;
        }
        if raw.trim_end().ends_with('\\') {
            tracing::debug!("skipping backslash-continued statement: {raw:?}");
            skip_continuation = true;
            continue;
        }
        let line = raw.trim_start();
        // A real declaration starts with `alias ` after leading whitespace.
        // Comment/blank lines and everything else are skipped here; inline
        // `#` handling happens quote-aware once we reach the value.
        if !line.starts_with("alias ") {
            continue;
        }
        let rest = line[6..].trim_start();
        let Some((name, value)) = rest.split_once('=') else {
            tracing::debug!("alias line has no '=' separator: {raw:?}");
            continue;
        };
        let name = name.trim();
        if name.is_empty()
            || name.starts_with('-')
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
        {
            tracing::debug!("rejecting alias with non-local name {name:?}: {raw:?}");
            continue;
        }
        // Split the value into its (possibly quoted) payload and any
        // trailing inline comment. `#` is only a comment OUTSIDE quotes, so
        // `alias gp='git push #origin'` keeps `git push #origin` while
        // `alias x='a'  # note` drops the note.
        let Some(unquoted) = strip_alias_value(value.trim()) else {
            tracing::debug!("alias {name:?} has empty value: {raw:?}");
            continue;
        };
        out.insert(name.to_string(), unquoted);
    }
    out
}

/// Strip surrounding quotes and any unquoted trailing `#` comment from a
/// raw alias value. Returns `None` when nothing usable remains.
///
/// - A quoted value (`'…'` or `"…"`) takes everything up to the matching
///   closing quote; any trailing text (including a `# comment`) is dropped.
/// - An unquoted value is terminated at the first `#` that begins a word
///   (preceded by whitespace), which is how shells treat an inline comment;
///   a `#` glued to the value (e.g. `curl http://x#frag`) is kept.
fn strip_alias_value(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let first = *bytes.first()?;
    if first == b'\'' || first == b'"' {
        // Find the matching closing quote and ignore whatever follows it.
        if let Some(rel) = value[1..].find(first as char) {
            let inner = &value[1..1 + rel];
            if inner.is_empty() {
                return None;
            }
            return Some(inner.to_string());
        }
        // No closing quote: opening-quote-only value. Keep the raw text
        // (sans the leading quote) so the shlex fallback downstream still
        // surfaces the alias partially rather than dropping it.
        let inner = &value[1..];
        return if inner.is_empty() {
            None
        } else {
            Some(inner.to_string())
        };
    }
    // Unquoted: cut at an inline ` #...` comment (word-boundary `#`).
    let mut prev_ws = false;
    let mut end = value.len();
    for (idx, ch) in value.char_indices() {
        if ch == '#' && (idx == 0 || prev_ws) {
            end = idx;
            break;
        }
        prev_ws = ch.is_whitespace();
    }
    let trimmed = value[..end].trim_end();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
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

/// Tokenise an alias value, falling back to whitespace split when shlex
/// can't parse it. `None` when no usable tokens remain.
fn shlex_tokens(value: &str) -> Option<Vec<String>> {
    match shlex::split(value) {
        Some(toks) if !toks.is_empty() => Some(toks),
        Some(_) => None,
        None => {
            let fallback: Vec<String> = value.split_whitespace().map(String::from).collect();
            if fallback.is_empty() {
                None
            } else {
                Some(fallback)
            }
        }
    }
}

/// Source files (relative to `$HOME`) the static parser reads. Entries are
/// ingested in array order and a later entry overrides an earlier one for
/// the same alias name (`out.insert`), so this is ordered low→high
/// precedence. The zsh block follows zsh's real read order
/// (`.zshenv` → `.zprofile` → `.zshrc` → `.zshrc.local`) so a `.zshrc`
/// definition correctly wins over the same name in `.zshenv`. zsh and bash
/// never co-source the same session, so the cross-shell order is a
/// best-effort approximation, not a real shell load order: the bash files
/// follow the zsh ones, and the dedicated `*_aliases` drop-ins come last
/// because they are sourced from the rc files and hold the user's most
/// explicit alias intent. Must stay in sync with [`ALIAS_SOURCE_FILES`] —
/// anything missing here can never invalidate the cache.
const STATIC_ALIAS_FILES: &[&str] = &[
    ".zshenv",
    ".zprofile",
    ".zshrc",
    ".zshrc.local",
    ".bashrc",
    ".bash_profile",
    ".zsh_aliases",
    ".aliases",
    ".bash_aliases",
];

/// Static walk: parses alias declarations directly out of dotfiles and
/// oh-my-zsh custom drop-ins. Avoids the 300–500 ms `zsh -c alias`
/// startup cost on oh-my-zsh setups.
fn load_static_aliases_from(home: &Path) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    let mut ingest = |content: &str| {
        for (name, value) in parse_alias_file(content) {
            match shlex_tokens(&value) {
                Some(toks) => {
                    out.insert(name, toks);
                }
                None => {
                    tracing::debug!("dropping alias {name:?}: no usable tokens from {value:?}");
                }
            }
        }
    };
    for name in STATIC_ALIAS_FILES {
        match std::fs::read_to_string(home.join(name)) {
            Ok(content) => ingest(&content),
            // NotFound is the dominant legitimate case — most users lack
            // most dotfiles — so it stays silent. An existing-but-unreadable
            // file (PermissionDenied, non-UTF-8 InvalidData) is a real loss
            // and earns a breadcrumb.
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                tracing::debug!("alias source {name} unreadable: {e}");
            }
            Err(_) => {}
        }
    }
    let omz_custom = home.join(".oh-my-zsh/custom");
    match std::fs::read_dir(&omz_custom) {
        Ok(rd) => {
            // read_dir yields entries in filesystem/OS-dependent order, so
            // sort the matching `*.zsh` paths before ingesting — otherwise
            // two drop-ins defining the same alias pick a nondeterministic
            // last-write-wins winner across machines.
            let mut dropins: Vec<PathBuf> = rd
                .flatten()
                .map(|entry| entry.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("zsh"))
                .collect();
            dropins.sort();
            for p in dropins {
                if let Ok(content) = std::fs::read_to_string(&p) {
                    ingest(&content);
                }
            }
        }
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            tracing::debug!("oh-my-zsh custom dir unreadable: {e}");
        }
        Err(_) => {}
    }
    out
}

/// Spawn `zsh -c alias` (then `bash -c alias`) with a polling deadline.
/// Returns the first non-empty parse. Only used when the static walk
/// produced nothing.
fn load_aliases_via_subprocess(timeout: Duration) -> HashMap<String, Vec<String>> {
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

        let deadline = std::time::Instant::now() + timeout;
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
                    std::thread::sleep(Duration::from_millis(10));
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
    HashMap::new()
}

/// Resolve aliases for a specific `$HOME`. Tests use this to bypass the
/// real home and the on-disk cache. Production callers go through
/// [`load_shell_aliases`].
///
/// The subprocess (`zsh -c alias`) is a fallback for the *no-static-aliases*
/// case ONLY: it fires when the static walk parses zero entries, never to
/// supplement a non-empty static result. This is a deliberate trade-off —
/// layering subprocess output over static output would re-incur the
/// 300–500 ms oh-my-zsh spawn cost on every cold start and blow the <100 ms
/// startup budget. The cost of the trade-off is that an alias the static
/// parser legitimately cannot interpret (e.g. one defined inside a function
/// or a multi-line statement) is invisible whenever any other alias parsed
/// successfully. The correctness fixes in [`parse_alias_file`] (quote-aware
/// `#`, dotted/hyphenated names, full multi-line skip) keep that gap as
/// small as possible, and dropped lines are logged at debug.
pub(crate) fn load_shell_aliases_from(
    home: &Path,
    subprocess_timeout: Duration,
) -> HashMap<String, Vec<String>> {
    let aliases = load_static_aliases_from(home);
    if !aliases.is_empty() {
        tracing::debug!("loaded {} aliases via static parser", aliases.len());
        return aliases;
    }
    load_aliases_via_subprocess(subprocess_timeout)
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

    let aliases = match home.as_ref() {
        Some(h) => load_shell_aliases_from(h, Duration::from_secs(2)),
        None => load_aliases_via_subprocess(Duration::from_secs(2)),
    };

    if aliases.is_empty() {
        // "No aliases loaded" is a legitimate empty state — a user may
        // simply have no aliases defined. Keep at debug level so it
        // doesn't pollute normal logs.
        tracing::debug!("no aliases loaded from any source");
    } else if let (Some(h), Some(cp)) = (home.as_ref(), cache_path.as_ref()) {
        save_alias_cache(h, cp, &aliases);
    }
    aliases
}

#[cfg(test)]
fn token_vec(tokens: &[&str]) -> Vec<String> {
    tokens.iter().map(|s| (*s).to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_alias_file_handles_quoted_and_simple_forms() {
        let src = r#"
# top comment
alias gco='git checkout'
alias gcb="git checkout -b"
alias dev=ssh
alias=not_an_alias
function helper() { echo nope; }
alias 'broken-name'='whatever'   # invalid name; skip
alias g\=git                     # malformed; skip
alias multiline='ls \
  -la'                           # multiline; skip conservatively
"#;
        let map = parse_alias_file(src);
        assert_eq!(map.get("gco").map(|v| v.as_str()), Some("git checkout"));
        assert_eq!(map.get("gcb").map(|v| v.as_str()), Some("git checkout -b"));
        assert_eq!(map.get("dev").map(|v| v.as_str()), Some("ssh"));
        assert!(!map.contains_key("broken-name"));
        assert!(!map.contains_key("multiline"));
        assert!(!map.contains_key("helper"));
    }

    #[test]
    fn parse_alias_file_preserves_hash_inside_quotes_and_strips_trailing_comment() {
        // (a) trailing comment after a valid alias is dropped;
        // (b) a `#` inside a quoted value is preserved verbatim.
        let src = concat!(
            "alias gco='git checkout'   # my alias\n",
            "alias e='echo #1'\n",
            "alias gp='git push #origin'\n",
            "alias url='curl http://x#frag'\n",
            "alias dq=\"echo #hash\"  # trailing\n",
            "alias trail='a'  # trailing\n",
        );
        let map = parse_alias_file(src);
        assert_eq!(map.get("gco").map(String::as_str), Some("git checkout"));
        assert_eq!(map.get("e").map(String::as_str), Some("echo #1"));
        assert_eq!(map.get("gp").map(String::as_str), Some("git push #origin"));
        assert_eq!(
            map.get("url").map(String::as_str),
            Some("curl http://x#frag")
        );
        assert_eq!(map.get("dq").map(String::as_str), Some("echo #hash"));
        assert_eq!(map.get("trail").map(String::as_str), Some("a"));
    }

    #[test]
    fn parse_alias_file_accepts_dotted_and_hyphenated_names() {
        // The oh-my-zsh `..` family and hyphenated names are trivially
        // interpretable and must parse.
        let src = concat!(
            "alias ..='cd ..'\n",
            "alias ...='cd ../..'\n",
            "alias ....='cd ../../..'\n",
            "alias git-st='git status'\n",
        );
        let map = parse_alias_file(src);
        assert_eq!(map.get("..").map(String::as_str), Some("cd .."));
        assert_eq!(map.get("...").map(String::as_str), Some("cd ../.."));
        assert_eq!(map.get("....").map(String::as_str), Some("cd ../../.."));
        assert_eq!(map.get("git-st").map(String::as_str), Some("git status"));
    }

    #[test]
    fn parse_alias_file_rejects_global_suffix_and_punctuated_names() {
        // Leading-dash flag forms and names with disallowed chars must NOT
        // register; a plain `alias ok=ls` is the positive control.
        let src = concat!(
            "alias -g G='| grep'\n",
            "alias -s pdf=acroread\n",
            "alias -L l=ls\n",
            "alias 'has space'='x'\n",
            "alias na@me='y'\n",
            "alias ok=ls\n",
        );
        let map = parse_alias_file(src);
        assert!(!map.contains_key("G"), "global alias must be rejected");
        assert!(!map.contains_key("pdf"), "suffix alias must be rejected");
        assert!(!map.contains_key("l"), "-L form must be rejected");
        assert!(
            !map.contains_key("has space") && !map.contains_key("'has"),
            "name with a space must be rejected"
        );
        assert!(!map.contains_key("na@me"), "name with @ must be rejected");
        assert_eq!(map.get("ok").map(String::as_str), Some("ls"));
    }

    #[test]
    fn parse_alias_file_drops_entire_backslash_continued_statement() {
        // The continuation line must not be parsed as a standalone
        // statement: neither `first` nor a phantom `injected` may appear.
        let src = "alias first='foo \\\nalias injected=evil'\n";
        let map = parse_alias_file(src);
        assert!(
            !map.contains_key("first"),
            "the head of a multi-line alias must be dropped"
        );
        assert!(
            !map.contains_key("injected"),
            "the continuation line must not register a phantom alias"
        );
    }

    #[test]
    fn parse_alias_file_empty_and_partial_value_forms() {
        // Empty value, empty-after-unquote, and opening-quote-only.
        let src = concat!(
            "alias e=\n",     // empty value -> no key
            "alias q=''\n",   // empty after unquote -> no key
            "alias y='foo\n", // opening-quote-only -> kept as `foo`
        );
        let map = parse_alias_file(src);
        assert!(!map.contains_key("e"), "empty value must yield no key");
        assert!(
            !map.contains_key("q"),
            "empty-after-unquote must yield no key"
        );
        assert_eq!(
            map.get("y").map(String::as_str),
            Some("foo"),
            "opening-quote-only value keeps the remaining text"
        );
    }

    #[test]
    fn load_shell_aliases_uses_static_parser_for_zshrc_aliases() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        std::fs::write(
            home.join(".zshrc"),
            "alias gco='git checkout'\nalias zz='zsh -c'\n",
        )
        .unwrap();

        let map = load_shell_aliases_from(home, std::time::Duration::from_secs(2));
        assert_eq!(
            map.get("gco"),
            Some(&token_vec(&["git", "checkout"])),
            "static parser must populate gco from ~/.zshrc"
        );
        assert_eq!(
            map.get("zz"),
            Some(&token_vec(&["zsh", "-c"])),
            "static parser must populate zz from ~/.zshrc"
        );
    }

    #[test]
    fn load_static_aliases_reads_oh_my_zsh_custom_dropins() {
        // omz is the dominant target: custom/*.zsh files must be parsed,
        // and the `.zsh` extension filter must exclude everything else.
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        let custom = home.join(".oh-my-zsh/custom");
        std::fs::create_dir_all(&custom).unwrap();
        std::fs::write(custom.join("aliases.zsh"), "alias gco='git checkout'\n").unwrap();
        std::fs::write(custom.join("README.md"), "alias bad=should_not_load\n").unwrap();

        let map = load_shell_aliases_from(home, std::time::Duration::from_secs(2));
        assert_eq!(
            map.get("gco"),
            Some(&token_vec(&["git", "checkout"])),
            "omz custom/*.zsh drop-ins must be parsed"
        );
        assert!(
            !map.contains_key("bad"),
            "non-.zsh files in custom/ must be ignored (extension filter)"
        );
    }

    #[test]
    fn load_static_aliases_later_file_overrides_earlier() {
        // Override is last-definition-wins across STATIC_ALIAS_FILES.
        // `.zshenv` is earlier than `.zshrc` in the array, so `.zshrc`'s
        // definition of `g` must win; an alias defined only in the earlier
        // file must still survive (override is per-name, not whole-file).
        assert!(
            STATIC_ALIAS_FILES.iter().position(|f| *f == ".zshenv")
                < STATIC_ALIAS_FILES.iter().position(|f| *f == ".zshrc"),
            "test precondition: .zshenv must precede .zshrc in STATIC_ALIAS_FILES"
        );
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        std::fs::write(
            home.join(".zshenv"),
            "alias g='git --env'\nalias only_env='echo env'\n",
        )
        .unwrap();
        std::fs::write(home.join(".zshrc"), "alias g='git --rc'\n").unwrap();

        let map = load_shell_aliases_from(home, std::time::Duration::from_secs(2));
        assert_eq!(
            map.get("g"),
            Some(&token_vec(&["git", "--rc"])),
            "later file (.zshrc) must override earlier (.zshenv)"
        );
        assert_eq!(
            map.get("only_env"),
            Some(&token_vec(&["echo", "env"])),
            "an alias only in the earlier file must survive"
        );
    }

    #[test]
    fn load_static_aliases_keeps_shlex_fallback_tokens() {
        // A value that is unbalanced after outer-quote stripping makes
        // shlex::split fail; the whitespace fallback must preserve every
        // token rather than dropping the alias entirely.
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        std::fs::write(home.join(".zshrc"), "alias broken='git \"open'\n").unwrap();

        let map = load_shell_aliases_from(home, std::time::Duration::from_secs(2));
        assert_eq!(
            map.get("broken"),
            Some(&token_vec(&["git", "\"open"])),
            "shlex fallback must keep all whitespace-split tokens"
        );
    }

    #[test]
    fn load_static_aliases_from_empty_home_is_empty() {
        // Documents the precondition under which the subprocess fallback
        // fires: the static walk returns empty only when no dotfile yields
        // a usable alias. (We don't spawn a real shell here — that would be
        // non-deterministic.)
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(
            load_static_aliases_from(tmp.path()).is_empty(),
            "an empty home must yield zero static aliases"
        );
    }

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
    fn read_set_equals_watch_set() {
        use std::collections::BTreeSet;

        // The PR's central invariant: the parser's READ set must equal the
        // cache's WATCH set, BOTH ways. `STATIC_ALIAS_FILES` (read by
        // `load_static_aliases_from`) and `ALIAS_SOURCE_FILES` (the mtime
        // watch set) must be the same set — adding to one without the other
        // silently reintroduces the "edit ~/.zsh_aliases, see a stale cache
        // forever" bug this PR exists to fix. Membership-only (not order):
        // the watch set is order-irrelevant, while the read set is ordered
        // for precedence, so set-equality is the contract.
        assert_eq!(
            STATIC_ALIAS_FILES.iter().collect::<BTreeSet<_>>(),
            ALIAS_SOURCE_FILES.iter().collect::<BTreeSet<_>>(),
            "STATIC_ALIAS_FILES (parser reads) and ALIAS_SOURCE_FILES (cache watches) must be identical sets"
        );

        // The parser's hardcoded `.oh-my-zsh/custom` dir walk must be a
        // member of the watched dir set, or edits to drop-ins never
        // invalidate the cache.
        assert!(
            ALIAS_SOURCE_DIRS.contains(&".oh-my-zsh/custom"),
            "ALIAS_SOURCE_DIRS must watch the parser-walked .oh-my-zsh/custom dir"
        );
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
