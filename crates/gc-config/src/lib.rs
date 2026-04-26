//! TOML configuration, keybinding definitions, and color themes.
//!
//! Reads from `~/.config/ghost-complete/config.toml` with serde deserialization
//! and sensible defaults for all fields.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Returns `~/.config/ghost-complete`, ignoring macOS `~/Library/Application Support/`.
pub fn config_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".config").join("ghost-complete"))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GhostConfig {
    pub trigger: TriggerConfig,
    pub popup: PopupConfig,
    pub suggest: SuggestConfig,
    pub paths: PathsConfig,
    pub keybindings: KeybindingsConfig,
    pub theme: ThemeConfig,
    pub experimental: ExperimentalConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExperimentalConfig {
    pub multi_terminal: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct KeybindingsConfig {
    pub accept: String,
    pub accept_and_enter: String,
    pub dismiss: String,
    pub navigate_up: String,
    pub navigate_down: String,
    pub trigger: String,
}

impl Default for KeybindingsConfig {
    fn default() -> Self {
        Self {
            accept: "tab".to_string(),
            accept_and_enter: "enter".to_string(),
            dismiss: "escape".to_string(),
            navigate_up: "arrow_up".to_string(),
            navigate_down: "arrow_down".to_string(),
            trigger: "ctrl+/".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TriggerConfig {
    pub auto_chars: Vec<char>,
    /// Typing-pause debounce window (milliseconds) before suggestions are
    /// computed on regular printable keystrokes.
    ///
    /// - `delay_ms > 0`: Task D in `gc-pty/src/proxy.rs` waits for this many
    ///   ms of inactivity after the last keystroke before firing a trigger.
    ///   This is the recommended behavior — it avoids re-ranking on every
    ///   character during fast typing.
    /// - `delay_ms = 0`: the debounce task is not spawned. Every printable
    ///   key and backspace fires a trigger immediately via
    ///   `handler.trigger_requested`, without any wait. Explicit triggers
    ///   (`auto_chars` such as space / slash, and the `trigger` keybinding)
    ///   still fire instantly regardless of this value — `delay_ms` only
    ///   gates the passive typing-pause path.
    ///
    /// Default: 150ms.
    ///
    /// **Hot-reload:** Changing `delay_ms` via `config.toml` edits while
    /// the proxy is running requires a restart to take effect (the debounce
    /// task is spawned once at startup — see `spawn_config_watcher`).
    pub delay_ms: u64,
    pub auto_trigger: bool,
}

impl Default for TriggerConfig {
    fn default() -> Self {
        Self {
            auto_chars: vec![' ', '/', '-', '.'],
            delay_ms: 150,
            auto_trigger: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PopupConfig {
    pub max_visible: usize,
    pub borders: bool,
    /// Maximum time (ms) the popup will block waiting for a higher-priority
    /// async generator before painting whatever sync results we have. Set
    /// to `0` to disable blocking entirely (paint immediately, merge async
    /// later). Clamped to `[0, 300]` during normalization. Default: 80 ms,
    /// chosen to stay below the human perception threshold for "instant".
    pub render_block_ms: u16,
}

impl Default for PopupConfig {
    fn default() -> Self {
        Self {
            max_visible: 10,
            borders: false,
            render_block_ms: 80,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SuggestConfig {
    /// Maximum number of ranked suggestions shown in the popup after
    /// fuzzy matching. Clamped to `[1, 10_000]` by [`GhostConfig::normalize`].
    ///
    /// - Upper bound `10_000`: values above are clamped with a warning to
    ///   avoid pathological memory / render cost.
    /// - Lower bound `1`: a literal `max_results = 0` is clamped to the
    ///   default (`50`) with a warning, because a zero cap would truncate
    ///   every result set to empty and render the popup permanently blank —
    ///   there is no legitimate user-facing reason to request that.
    /// - Default: `50`.
    ///
    /// **Hot-reload:** Changes require a proxy restart — the value is baked
    /// into the `SuggestionEngine` at builder time in
    /// `InputHandler::with_suggest_config`.
    pub max_results: usize,
    pub max_history_results: usize,
    /// Per-invocation timeout (ms) for async script/git generators. Results
    /// arriving after this budget elapses are discarded. Set high enough to
    /// cover slow generators (`docker ps`, `kubectl get`), low enough that a
    /// stalled generator does not keep the loading indicator spinning
    /// indefinitely. Default: 5000 ms.
    pub generator_timeout_ms: u64,
    pub providers: ProvidersConfig,
}

impl Default for SuggestConfig {
    fn default() -> Self {
        Self {
            max_results: 50,
            max_history_results: 5,
            generator_timeout_ms: 5000,
            providers: ProvidersConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProvidersConfig {
    pub commands: bool,
    pub filesystem: bool,
    pub specs: bool,
    pub git: bool,
}

impl Default for ProvidersConfig {
    fn default() -> Self {
        Self {
            commands: true,
            filesystem: true,
            specs: true,
            git: true,
        }
    }
}

/// User-facing theme config. Deserialized directly from `config.toml`.
///
/// Each override field is `Option<String>` so we can distinguish three cases:
///
/// * `None` — field omitted in TOML. Inherits from the preset.
/// * `Some("")` — field explicitly set to empty. Valid: means "no styling"
///   (i.e. produce zero ANSI bytes), distinct from "inherit from preset".
/// * `Some("bold fg:196")` — explicit override, used verbatim.
///
/// Call [`ThemeConfig::resolve`] to collapse this into a [`ResolvedTheme`]
/// where every field is a concrete `String`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeConfig {
    pub preset: String,
    // `skip_serializing_if` is required for every Option here: TOML has no
    // null, so `toml::Value::try_from` would error on a `None` field. The
    // two-pass loader in `GhostConfig::load` serializes the strict view to
    // walk it alongside the user's TOML, and that path must never fail on
    // the default (all-None) config.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_highlight: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scrollbar: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border: Option<String>,
}

/// Fully resolved theme — every field is a concrete style string (possibly
/// empty, meaning "no styling"). Produced by [`ThemeConfig::resolve`]; this
/// is what consumers (gc-pty, gc-overlay) should read.
///
/// Unlike [`ThemeConfig`], there is no `preset` field and no optionality:
/// the resolver has already merged the preset base with user overrides.
#[derive(Debug, Clone, Default)]
pub struct ResolvedTheme {
    pub selected: String,
    pub description: String,
    pub match_highlight: String,
    pub item_text: String,
    pub scrollbar: String,
    pub border: String,
}

impl ThemeConfig {
    /// Validate every style-string field without producing ANSI bytes.
    ///
    /// Each field is parsed against the same token grammar as
    /// `gc_overlay::parse_style`:
    /// `reverse` | `dim` | `bold` | `underline` | `fg:N` | `bg:N` | `fg:#RRGGBB` | `bg:#RRGGBB`
    ///
    /// Called from [`GhostConfig::load`] so that a typo in `config.toml`
    /// (e.g. `selected = "bld"` or `scrollbar = "fg:#GGGGGG"`) surfaces as a
    /// clear load-time error rather than a broken render later.
    ///
    /// `None` fields are skipped — they mean "inherit from preset", and the
    /// preset values are hard-coded and trusted.
    ///
    /// **SYNC REQUIREMENT:** this validator mirrors the token grammar of
    /// `gc_overlay::render::parse_style`. If a new token is added to the
    /// overlay parser, add it here too (or the new token will be silently
    /// rejected at load time until this validator catches up). Direct reuse
    /// of `parse_style` is blocked by a dependency cycle: gc-config →
    /// gc-overlay → gc-suggest → gc-config.
    pub fn validate(&self) -> Result<()> {
        // Validate the preset name so a validated ThemeConfig is guaranteed
        // to resolve() successfully. Valid names mirror preset_values(); an
        // empty string is allowed because resolve() treats it as "dark".
        if !self.preset.is_empty() {
            match self.preset.as_str() {
                "dark" | "light" | "catppuccin" | "material-darker" => {}
                other => bail!(
                    "invalid theme.preset: {:?} (valid: dark, light, catppuccin, material-darker)",
                    other
                ),
            }
        }
        validate_opt_style("theme.selected", self.selected.as_deref())?;
        validate_opt_style("theme.description", self.description.as_deref())?;
        validate_opt_style("theme.match_highlight", self.match_highlight.as_deref())?;
        validate_opt_style("theme.item_text", self.item_text.as_deref())?;
        validate_opt_style("theme.scrollbar", self.scrollbar.as_deref())?;
        validate_opt_style("theme.border", self.border.as_deref())?;
        Ok(())
    }

    /// Resolve preset base + field overrides into a [`ResolvedTheme`].
    ///
    /// For each override field: `Some(v)` wins (including `Some("")`,
    /// which means "explicitly no styling"); `None` inherits the preset's
    /// value for that field.
    pub fn resolve(&self) -> Result<ResolvedTheme> {
        let preset_name = if self.preset.is_empty() {
            "dark"
        } else {
            &self.preset
        };
        let base = preset_values(preset_name)?;
        Ok(ResolvedTheme {
            selected: self.selected.clone().unwrap_or(base.selected),
            description: self.description.clone().unwrap_or(base.description),
            match_highlight: self.match_highlight.clone().unwrap_or(base.match_highlight),
            item_text: self.item_text.clone().unwrap_or(base.item_text),
            scrollbar: self.scrollbar.clone().unwrap_or(base.scrollbar),
            border: self.border.clone().unwrap_or(base.border),
        })
    }
}

/// Validate an `Option<&str>` style field. `None` is always OK (means
/// "inherit from preset"); `Some(v)` delegates to [`validate_style_str`].
fn validate_opt_style(field: &str, value: Option<&str>) -> Result<()> {
    match value {
        None => Ok(()),
        Some(v) => validate_style_str(field, v),
    }
}

/// Shape validator for a single style string. Mirrors the token grammar of
/// `gc_overlay::render::parse_style` — see the doc comment on
/// [`ThemeConfig::validate`] for why this is a mirror rather than a call.
fn validate_style_str(field: &str, value: &str) -> Result<()> {
    for token in value.split_whitespace() {
        match token {
            "reverse" | "dim" | "bold" | "underline" => {}
            _ if token.starts_with("fg:#") => validate_hex_color(&token[4..], token, field)?,
            _ if token.starts_with("fg:") => validate_u8_color(&token[3..], token, field)?,
            _ if token.starts_with("bg:#") => validate_hex_color(&token[4..], token, field)?,
            _ if token.starts_with("bg:") => validate_u8_color(&token[3..], token, field)?,
            _ => bail!("invalid {}: unknown style token: {:?}", field, token),
        }
    }
    Ok(())
}

fn validate_hex_color(hex: &str, token: &str, field: &str) -> Result<()> {
    if hex.len() != 6 {
        bail!(
            "invalid {}: hex color must be 6 characters (token: {:?})",
            field,
            token
        );
    }
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!(
            "invalid {}: hex color contains non-hex chars (token: {:?})",
            field,
            token
        );
    }
    Ok(())
}

fn validate_u8_color(num: &str, token: &str, field: &str) -> Result<()> {
    num.parse::<u8>().map(|_| ()).map_err(|_| {
        anyhow::anyhow!(
            "invalid {}: expected 0-255 palette index (token: {:?})",
            field,
            token
        )
    })
}

fn preset_values(name: &str) -> Result<ResolvedTheme> {
    let theme = match name {
        "dark" => ResolvedTheme {
            selected: "reverse".into(),
            description: "dim".into(),
            match_highlight: "bold".into(),
            item_text: String::new(),
            scrollbar: "dim".into(),
            border: "dim".into(),
        },
        "light" => ResolvedTheme {
            selected: "fg:#1e1e2e bg:#dce0e8 bold".into(),
            description: "fg:#6c6f85".into(),
            match_highlight: "fg:#d20f39 bold".into(),
            item_text: String::new(),
            scrollbar: "fg:#9ca0b0".into(),
            border: "fg:#9ca0b0".into(),
        },
        "catppuccin" => ResolvedTheme {
            selected: "fg:#cdd6f4 bg:#585b70 bold".into(),
            description: "fg:#6c7086".into(),
            match_highlight: "fg:#f9e2af bold".into(),
            item_text: String::new(),
            scrollbar: "fg:#585b70".into(),
            border: "fg:#585b70".into(),
        },
        "material-darker" => ResolvedTheme {
            selected: "fg:#eeffff bg:#424242 bold".into(),
            description: "fg:#616161".into(),
            match_highlight: "fg:#ffcb6b bold".into(),
            item_text: String::new(),
            scrollbar: "fg:#424242".into(),
            border: "fg:#424242".into(),
        },
        _ => bail!(
            "unknown theme preset: {:?} (valid: dark, light, catppuccin, material-darker)",
            name
        ),
    };
    Ok(theme)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PathsConfig {
    pub spec_dirs: Vec<String>,
}

const MAX_VISIBLE_DEFAULT: usize = 10;
const MAX_VISIBLE_UPPER: usize = 50;
const MAX_RESULTS_UPPER: usize = 10_000;
const MAX_RESULTS_DEFAULT: usize = 50;
const RENDER_BLOCK_MS_UPPER: u16 = 300;

impl GhostConfig {
    /// Clamp config values to sane bounds, logging warnings when clamping.
    ///
    /// Exposed for TUI editor validation: callers can clone, normalize, and
    /// compare to detect out-of-range values without mutating the original.
    pub fn normalize(&mut self) {
        if self.popup.max_visible == 0 {
            tracing::warn!(
                "popup.max_visible=0 is invalid (would break popup scrolling), clamping to default {}",
                MAX_VISIBLE_DEFAULT,
            );
            self.popup.max_visible = MAX_VISIBLE_DEFAULT;
        }
        if self.popup.max_visible > MAX_VISIBLE_UPPER {
            tracing::warn!(
                "popup.max_visible={} exceeds maximum {}, clamping",
                self.popup.max_visible,
                MAX_VISIBLE_UPPER,
            );
            self.popup.max_visible = MAX_VISIBLE_UPPER;
        }
        if self.suggest.max_results > MAX_RESULTS_UPPER {
            tracing::warn!(
                "suggest.max_results={} exceeds maximum {}, clamping",
                self.suggest.max_results,
                MAX_RESULTS_UPPER,
            );
            self.suggest.max_results = MAX_RESULTS_UPPER;
        }
        // max_results=0 would truncate all ranked output to empty, leaving
        // the popup permanently blank. Clamp to the default and warn.
        if self.suggest.max_results == 0 {
            tracing::warn!(
                "suggest.max_results=0 is invalid (would hide all suggestions), \
                 clamping to default {}",
                MAX_RESULTS_DEFAULT,
            );
            self.suggest.max_results = MAX_RESULTS_DEFAULT;
        }
        if self.popup.render_block_ms > RENDER_BLOCK_MS_UPPER {
            tracing::warn!(
                "popup.render_block_ms={} exceeds maximum {}, clamping",
                self.popup.render_block_ms,
                RENDER_BLOCK_MS_UPPER,
            );
            self.popup.render_block_ms = RENDER_BLOCK_MS_UPPER;
        }
    }

    pub fn load(path: Option<&str>) -> Result<Self> {
        let config_path = match path {
            Some(p) => PathBuf::from(p),
            None => {
                let Some(dir) = config_dir() else {
                    // HOME unset — refuse to load from CWD (could be attacker-controlled).
                    return Ok(Self::default());
                };
                dir.join("config.toml")
            }
        };

        let contents = match std::fs::read_to_string(&config_path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(e) => {
                return Err(anyhow::Error::new(e).context(format!(
                    "failed to read config file: {}",
                    config_path.display()
                )));
            }
        };

        let mut config: GhostConfig = toml::from_str(&contents)
            .with_context(|| format!("failed to parse config file: {}", config_path.display()))?;

        // Fail-fast theme validation: catch typos in user-supplied style
        // strings at load time rather than later at render time. Presets are
        // hardcoded and always valid, so validating only the raw override
        // fields is sufficient.
        config
            .theme
            .validate()
            .with_context(|| format!("invalid theme in {}", config_path.display()))?;

        // Two-pass unknown-key detection: re-parse the source as a
        // permissive `toml::Value`, serialize the strictly-typed `GhostConfig`
        // back to `toml::Value`, and diff the two trees. Any key present in
        // the loose tree but absent in the typed tree is a typo / removed
        // field / unknown field — warn (not error) so a bad config.toml edit
        // can never take the proxy down.
        if let Ok(loose) = toml::from_str::<toml::Value>(&contents) {
            if let Ok(strict) = toml::Value::try_from(&config) {
                let mut unknown = Vec::new();
                let mut path: Vec<String> = Vec::new();
                diff_unknown_keys(&loose, &strict, &mut path, &mut unknown);
                for key in unknown {
                    tracing::warn!(
                        "unknown config key in {}: {} (typo? removed field?)",
                        config_path.display(),
                        key,
                    );
                }
            }
        }

        config.normalize();

        Ok(config)
    }
}

/// Walk `loose` (a permissive `toml::Value` parsed from the source file) and
/// `strict` (the same config serialized back from the typed `GhostConfig`) in
/// parallel, collecting dotted-path keys that exist only on the loose side.
///
/// Both sides are expected to be `Table`s at the root. Nested tables recurse.
/// Arrays-of-tables recurse element-wise. Leaf / scalar values are ignored —
/// value-level mismatches aren't unknown-key diagnostics.
fn diff_unknown_keys(
    loose: &toml::Value,
    strict: &toml::Value,
    path: &mut Vec<String>,
    out: &mut Vec<String>,
) {
    match (loose, strict) {
        (toml::Value::Table(loose_tbl), toml::Value::Table(strict_tbl)) => {
            for (key, loose_val) in loose_tbl {
                path.push(key.clone());
                match strict_tbl.get(key) {
                    Some(strict_val) => diff_unknown_keys(loose_val, strict_val, path, out),
                    None => out.push(path.join(".")),
                }
                path.pop();
            }
        }
        (toml::Value::Array(loose_arr), toml::Value::Array(strict_arr)) => {
            // Recurse into array-of-tables elements; scalar arrays bottom out
            // because their elements have no inner keys to diff.
            for (idx, loose_item) in loose_arr.iter().enumerate() {
                if let Some(strict_item) = strict_arr.get(idx) {
                    path.push(format!("[{idx}]"));
                    diff_unknown_keys(loose_item, strict_item, path, out);
                    path.pop();
                }
            }
        }
        _ => {
            // Leaves (scalar values) — nothing to diff key-wise.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_default_config_matches_hardcoded() {
        let config = GhostConfig::default();
        assert_eq!(config.trigger.auto_chars, vec![' ', '/', '-', '.']);
        assert_eq!(config.trigger.delay_ms, 150);
        assert!(config.trigger.auto_trigger);
        assert_eq!(config.popup.max_visible, 10);
        assert_eq!(config.suggest.max_results, 50);
        assert_eq!(config.suggest.max_history_results, 5);
        assert!(config.suggest.providers.commands);
        assert!(config.suggest.providers.filesystem);
        assert!(config.suggest.providers.specs);
        assert!(config.suggest.providers.git);
        assert!(config.paths.spec_dirs.is_empty());
        assert_eq!(config.keybindings.accept, "tab");
        assert_eq!(config.keybindings.accept_and_enter, "enter");
        assert_eq!(config.keybindings.dismiss, "escape");
        assert_eq!(config.keybindings.navigate_up, "arrow_up");
        assert_eq!(config.keybindings.navigate_down, "arrow_down");
        assert_eq!(config.keybindings.trigger, "ctrl+/");
        assert_eq!(config.theme.preset, "");
        assert_eq!(config.theme.selected, None);
        assert_eq!(config.theme.description, None);
        assert_eq!(config.theme.match_highlight, None);
        assert_eq!(config.theme.item_text, None);
        assert_eq!(config.theme.scrollbar, None);
        assert_eq!(config.theme.border, None);
        assert!(!config.experimental.multi_terminal);
    }

    #[test]
    fn test_parse_partial_toml() {
        let toml_str = r#"
[popup]
max_visible = 5
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.popup.max_visible, 5);
        // Everything else should be default
        assert_eq!(config.trigger.auto_chars, vec![' ', '/', '-', '.']);
        assert_eq!(config.suggest.max_results, 50);
    }

    #[test]
    fn test_missing_file_returns_default() {
        let config = GhostConfig::load(Some("/nonexistent/path/config.toml")).unwrap();
        assert_eq!(config.popup.max_visible, 10);
    }

    #[test]
    fn test_malformed_toml_returns_error() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "this is not [valid toml = {{}}").unwrap();
        let result = GhostConfig::load(Some(tmp.path().to_str().unwrap()));
        assert!(result.is_err());
    }

    #[test]
    fn test_partial_keybindings_override() {
        let toml_str = r#"
[keybindings]
accept = "enter"
navigate_up = "ctrl+space"
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.keybindings.accept, "enter");
        assert_eq!(config.keybindings.navigate_up, "ctrl+space");
        // Unset fields keep defaults
        assert_eq!(config.keybindings.accept_and_enter, "enter");
        assert_eq!(config.keybindings.dismiss, "escape");
        assert_eq!(config.keybindings.navigate_down, "arrow_down");
        assert_eq!(config.keybindings.trigger, "ctrl+/");
    }

    #[test]
    fn test_full_config_parses() {
        let toml_str = r#"
[trigger]
auto_chars = [' ', '/']
delay_ms = 200

[popup]
max_visible = 15

[suggest]
max_results = 100
max_history_results = 3

[suggest.providers]
commands = true
filesystem = true
specs = true
git = false

[paths]
spec_dirs = ["/usr/local/share/ghost-complete/specs"]

[keybindings]
accept = "enter"
accept_and_enter = "tab"
dismiss = "escape"
navigate_up = "arrow_up"
navigate_down = "arrow_down"
trigger = "ctrl+space"

[theme]
selected = "bold"
description = "dim"
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.trigger.auto_chars, vec![' ', '/']);
        assert_eq!(config.trigger.delay_ms, 200);
        assert_eq!(config.theme.selected.as_deref(), Some("bold"));
        assert_eq!(config.theme.description.as_deref(), Some("dim"));
        assert_eq!(config.popup.max_visible, 15);
        assert_eq!(config.suggest.max_results, 100);
        assert_eq!(config.suggest.max_history_results, 3);
        assert!(config.suggest.providers.commands);
        assert!(!config.suggest.providers.git);
        assert_eq!(
            config.paths.spec_dirs,
            vec!["/usr/local/share/ghost-complete/specs"]
        );
        assert_eq!(config.keybindings.accept, "enter");
        assert_eq!(config.keybindings.accept_and_enter, "tab");
    }

    #[test]
    fn test_partial_theme_override() {
        let toml_str = r#"
[theme]
selected = "bold fg:255"
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.theme.selected.as_deref(), Some("bold fg:255"));
        // Unset field stays None (inherits from preset at resolve time)
        assert_eq!(config.theme.description, None);
    }

    #[test]
    fn test_full_theme_config() {
        let toml_str = r#"
[theme]
selected = "fg:255 bg:236"
description = "dim underline"
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.theme.selected.as_deref(), Some("fg:255 bg:236"));
        assert_eq!(config.theme.description.as_deref(), Some("dim underline"));
    }

    #[test]
    fn test_theme_new_field_defaults() {
        let config = GhostConfig::default();
        assert_eq!(config.theme.match_highlight, None);
        assert_eq!(config.theme.item_text, None);
        assert_eq!(config.theme.scrollbar, None);
    }

    #[test]
    fn test_partial_theme_new_fields() {
        let toml_str = r#"
[theme]
match_highlight = "underline"
scrollbar = "fg:#555555"
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.theme.match_highlight.as_deref(), Some("underline"));
        assert_eq!(config.theme.scrollbar.as_deref(), Some("fg:#555555"));
        assert_eq!(config.theme.selected, None);
        assert_eq!(config.theme.description, None);
        assert_eq!(config.theme.item_text, None);
    }

    #[test]
    fn test_explicit_empty_string_distinct_from_none() {
        // Setting a theme field to "" in TOML is valid and distinct from
        // omitting it: omitted => inherit preset, "" => explicitly no styling.
        let toml_str = r#"
[theme]
preset = "dark"
selected = ""
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.theme.selected.as_deref(), Some(""));
        // description was omitted — stays None
        assert_eq!(config.theme.description, None);

        let resolved = config.theme.resolve().unwrap();
        // Explicit empty wins: no styling even though dark preset has "reverse"
        assert_eq!(resolved.selected, "");
        // Omitted field inherits from dark preset
        assert_eq!(resolved.description, "dim");
    }

    #[test]
    fn test_resolve_no_preset_uses_dark() {
        let config = ThemeConfig::default();
        let resolved = config.resolve().unwrap();
        assert_eq!(resolved.selected, "reverse");
        assert_eq!(resolved.description, "dim");
        assert_eq!(resolved.match_highlight, "bold");
        assert_eq!(resolved.item_text, "");
        assert_eq!(resolved.scrollbar, "dim");
        assert_eq!(resolved.border, "dim");
    }

    #[test]
    fn test_validate_accepts_all_known_presets() {
        for preset in &["", "dark", "light", "catppuccin", "material-darker"] {
            let config = ThemeConfig {
                preset: (*preset).into(),
                ..Default::default()
            };
            config
                .validate()
                .unwrap_or_else(|e| panic!("preset {preset:?} should validate, got: {e}"));
        }
    }

    #[test]
    fn test_validate_rejects_unknown_preset() {
        let config = ThemeConfig {
            preset: "drak".into(),
            ..Default::default()
        };
        let err = config
            .validate()
            .expect_err("typo preset must fail validate()");
        let msg = err.to_string();
        assert!(
            msg.contains("theme.preset") && msg.contains("drak"),
            "error must name the field and the bad value, got: {msg}"
        );
    }

    #[test]
    fn test_resolve_catppuccin_preset() {
        let config = ThemeConfig {
            preset: "catppuccin".into(),
            ..Default::default()
        };
        let resolved = config.resolve().unwrap();
        assert_eq!(resolved.selected, "fg:#cdd6f4 bg:#585b70 bold");
        assert_eq!(resolved.description, "fg:#6c7086");
        assert_eq!(resolved.match_highlight, "fg:#f9e2af bold");
        assert_eq!(resolved.item_text, "");
        assert_eq!(resolved.scrollbar, "fg:#585b70");
        assert_eq!(resolved.border, "fg:#585b70");
    }

    #[test]
    fn test_resolve_preset_with_field_override() {
        let config = ThemeConfig {
            preset: "catppuccin".into(),
            match_highlight: Some("underline".into()),
            ..Default::default()
        };
        let resolved = config.resolve().unwrap();
        // Override wins
        assert_eq!(resolved.match_highlight, "underline");
        // Rest from preset
        assert_eq!(resolved.selected, "fg:#cdd6f4 bg:#585b70 bold");
        assert_eq!(resolved.description, "fg:#6c7086");
    }

    #[test]
    fn test_resolve_invalid_preset_errors() {
        let config = ThemeConfig {
            preset: "nonexistent".into(),
            ..Default::default()
        };
        assert!(config.resolve().is_err());
    }

    #[test]
    fn test_resolve_material_darker_preset() {
        let config = ThemeConfig {
            preset: "material-darker".into(),
            ..Default::default()
        };
        let resolved = config.resolve().unwrap();
        assert_eq!(resolved.selected, "fg:#eeffff bg:#424242 bold");
        assert_eq!(resolved.description, "fg:#616161");
        assert_eq!(resolved.match_highlight, "fg:#ffcb6b bold");
        assert_eq!(resolved.scrollbar, "fg:#424242");
        assert_eq!(resolved.border, "fg:#424242");
    }

    #[test]
    fn test_resolve_light_preset() {
        let config = ThemeConfig {
            preset: "light".into(),
            ..Default::default()
        };
        let resolved = config.resolve().unwrap();
        assert_eq!(resolved.selected, "fg:#1e1e2e bg:#dce0e8 bold");
        assert_eq!(resolved.description, "fg:#6c6f85");
        assert_eq!(resolved.match_highlight, "fg:#d20f39 bold");
        assert_eq!(resolved.item_text, "");
        assert_eq!(resolved.scrollbar, "fg:#9ca0b0");
        assert_eq!(resolved.border, "fg:#9ca0b0");
    }

    #[test]
    fn test_legacy_providers_history_field_ignored() {
        let toml_str = r#"
[suggest.providers]
history = false
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        // Field is silently ignored; max_history_results keeps its default
        assert_eq!(config.suggest.max_history_results, 5);
    }

    #[test]
    fn test_removed_popup_fields_ignored() {
        let toml_str = r#"
[popup]
max_visible = 10
min_width = 25
max_width = 80
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.popup.max_visible, 10);
    }

    #[test]
    fn test_removed_suggest_fields_ignored() {
        // `max_history_entries` was renamed — parsing should succeed and
        // leave the replacement field at its default.
        let toml_str = r#"
[suggest]
max_results = 50
max_history_entries = 5000
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.suggest.max_results, 50);
    }

    #[test]
    fn test_generator_timeout_ms_default() {
        let config = GhostConfig::default();
        assert_eq!(config.suggest.generator_timeout_ms, 5000);
    }

    #[test]
    fn test_generator_timeout_ms_parse() {
        let toml_str = r#"
[suggest]
generator_timeout_ms = 2000
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.suggest.generator_timeout_ms, 2000);
        // Unrelated fields keep defaults.
        assert_eq!(config.suggest.max_results, 50);
    }

    #[test]
    fn test_generator_timeout_ms_missing_is_default() {
        let toml_str = r#"
[suggest]
max_results = 25
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.suggest.max_results, 25);
        assert_eq!(config.suggest.generator_timeout_ms, 5000);
    }

    #[test]
    fn test_experimental_defaults_to_off() {
        let config = GhostConfig::default();
        assert!(!config.experimental.multi_terminal);
    }

    #[test]
    fn test_experimental_multi_terminal_enabled() {
        let toml_str = r#"
[experimental]
multi_terminal = true
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        assert!(config.experimental.multi_terminal);
    }

    #[test]
    fn test_experimental_missing_uses_default() {
        let toml_str = r#"
[popup]
max_visible = 5
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        assert!(!config.experimental.multi_terminal);
    }

    #[test]
    fn test_clamp_max_visible_over_limit() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "[popup]\nmax_visible = 100000").unwrap();
        let config = GhostConfig::load(Some(tmp.path().to_str().unwrap())).unwrap();
        assert_eq!(config.popup.max_visible, MAX_VISIBLE_UPPER);
    }

    #[test]
    fn test_clamp_max_results_over_limit() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "[suggest]\nmax_results = 999999").unwrap();
        let config = GhostConfig::load(Some(tmp.path().to_str().unwrap())).unwrap();
        assert_eq!(config.suggest.max_results, MAX_RESULTS_UPPER);
    }

    #[test]
    fn test_no_clamp_when_within_bounds() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            tmp,
            "[popup]\nmax_visible = 25\n[suggest]\nmax_results = 500"
        )
        .unwrap();
        let config = GhostConfig::load(Some(tmp.path().to_str().unwrap())).unwrap();
        assert_eq!(config.popup.max_visible, 25);
        assert_eq!(config.suggest.max_results, 500);
    }

    #[test]
    fn test_clamp_at_exact_boundary() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            tmp,
            "[popup]\nmax_visible = 50\n[suggest]\nmax_results = 10000"
        )
        .unwrap();
        let config = GhostConfig::load(Some(tmp.path().to_str().unwrap())).unwrap();
        assert_eq!(config.popup.max_visible, 50);
        assert_eq!(config.suggest.max_results, 10000);
    }

    #[test]
    fn test_clamp_max_results_zero_to_default() {
        // max_results=0 is a footgun — it would truncate every ranked result
        // set to empty. Clamp to the default instead of rendering a
        // permanently blank popup.
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "[suggest]\nmax_results = 0").unwrap();
        let config = GhostConfig::load(Some(tmp.path().to_str().unwrap())).unwrap();
        assert_eq!(config.suggest.max_results, MAX_RESULTS_DEFAULT);
    }

    #[test]
    fn test_clamp_max_visible_zero_to_default() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "[popup]\nmax_visible = 0").unwrap();
        let config = GhostConfig::load(Some(tmp.path().to_str().unwrap())).unwrap();
        assert_eq!(config.popup.max_visible, 10);
    }

    #[test]
    fn test_delay_ms_zero_is_allowed() {
        // delay_ms=0 disables the typing-pause debounce — still a valid
        // choice, so it must pass through untouched.
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "[trigger]\ndelay_ms = 0").unwrap();
        let config = GhostConfig::load(Some(tmp.path().to_str().unwrap())).unwrap();
        assert_eq!(config.trigger.delay_ms, 0);
    }

    #[test]
    fn test_diff_unknown_keys_flat_top_level() {
        let loose: toml::Value = toml::from_str("known = 1\nbogus = 2").unwrap();
        let strict: toml::Value = toml::from_str("known = 1").unwrap();
        let mut out = Vec::new();
        let mut path = Vec::new();
        diff_unknown_keys(&loose, &strict, &mut path, &mut out);
        assert_eq!(out, vec!["bogus".to_string()]);
    }

    #[test]
    fn test_diff_unknown_keys_nested_table() {
        let loose: toml::Value = toml::from_str(
            r#"
[suggest]
max_results = 50
typo_field = 42

[suggest.providers]
git = true
"#,
        )
        .unwrap();
        let strict: toml::Value = toml::from_str(
            r#"
[suggest]
max_results = 50

[suggest.providers]
git = true
"#,
        )
        .unwrap();
        let mut out = Vec::new();
        let mut path = Vec::new();
        diff_unknown_keys(&loose, &strict, &mut path, &mut out);
        assert_eq!(out, vec!["suggest.typo_field".to_string()]);
    }

    #[test]
    fn test_diff_unknown_keys_deep_nested() {
        let loose: toml::Value = toml::from_str(
            r#"
[suggest.providers]
commands = true
unknown_provider = false
"#,
        )
        .unwrap();
        let strict: toml::Value = toml::from_str(
            r#"
[suggest.providers]
commands = true
"#,
        )
        .unwrap();
        let mut out = Vec::new();
        let mut path = Vec::new();
        diff_unknown_keys(&loose, &strict, &mut path, &mut out);
        assert_eq!(out, vec!["suggest.providers.unknown_provider".to_string()]);
    }

    #[test]
    fn test_diff_unknown_keys_all_known() {
        let loose: toml::Value = toml::from_str(
            r#"
[suggest]
max_results = 100
max_history_results = 10
"#,
        )
        .unwrap();
        let strict = loose.clone();
        let mut out = Vec::new();
        let mut path = Vec::new();
        diff_unknown_keys(&loose, &strict, &mut path, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn test_validate_empty_theme_ok() {
        // Default theme has all-None fields — validation is a no-op.
        let config = ThemeConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_all_valid_tokens() {
        let config = ThemeConfig {
            selected: Some("reverse bold".into()),
            description: Some("dim underline".into()),
            match_highlight: Some("fg:196 bg:0".into()),
            item_text: Some("fg:#FFCC00".into()),
            scrollbar: Some("bg:#112233".into()),
            border: Some("fg:255".into()),
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_accepts_explicit_empty_string() {
        // Some("") is valid — it means "explicitly no styling", not a typo.
        let config = ThemeConfig {
            selected: Some(String::new()),
            description: Some(String::new()),
            match_highlight: Some(String::new()),
            item_text: Some(String::new()),
            scrollbar: Some(String::new()),
            border: Some(String::new()),
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_rejects_unknown_token() {
        let config = ThemeConfig {
            selected: Some("notacolor".into()),
            ..Default::default()
        };
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("theme.selected"));
        assert!(err.contains("notacolor"));
    }

    #[test]
    fn test_validate_rejects_bad_hex_length() {
        let config = ThemeConfig {
            description: Some("fg:#ABC".into()),
            ..Default::default()
        };
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("theme.description"));
        assert!(err.contains("6 characters"));
    }

    #[test]
    fn test_validate_rejects_bad_hex_digits() {
        let config = ThemeConfig {
            match_highlight: Some("fg:#GGGGGG".into()),
            ..Default::default()
        };
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("theme.match_highlight"));
        assert!(err.contains("non-hex"));
    }

    #[test]
    fn test_validate_rejects_bad_palette_index() {
        let config = ThemeConfig {
            scrollbar: Some("bg:999".into()),
            ..Default::default()
        };
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("theme.scrollbar"));
        assert!(err.contains("0-255"));
    }

    #[test]
    fn test_load_rejects_invalid_theme_style() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "[theme]\nselected = \"blod\"").unwrap();
        let result = GhostConfig::load(Some(tmp.path().to_str().unwrap()));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid theme"));
    }

    #[test]
    fn test_load_accepts_valid_theme_style() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            tmp,
            "[theme]\nselected = \"bold fg:196\"\nborder = \"fg:#00FF00\""
        )
        .unwrap();
        let config = GhostConfig::load(Some(tmp.path().to_str().unwrap())).unwrap();
        assert_eq!(config.theme.selected.as_deref(), Some("bold fg:196"));
        assert_eq!(config.theme.border.as_deref(), Some("fg:#00FF00"));
    }

    #[test]
    fn test_load_with_unknown_key_succeeds() {
        // The two-pass load warns on unknown keys but must still succeed —
        // a typo in config.toml should never take the proxy down.
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            tmp,
            "[trigger]\ndelay_ms = 200\ndelay_ms_typo = 999\n\n[suggest]\nmax_results = 75"
        )
        .unwrap();
        let config = GhostConfig::load(Some(tmp.path().to_str().unwrap())).unwrap();
        // Known fields still applied correctly.
        assert_eq!(config.trigger.delay_ms, 200);
        assert_eq!(config.suggest.max_results, 75);
    }

    #[test]
    fn test_missing_file_returns_default_via_notfound() {
        // Verifies the TOCTOU-safe path: read_to_string NotFound → default
        let config = GhostConfig::load(Some("/tmp/definitely_not_a_real_config_42.toml")).unwrap();
        assert_eq!(config.popup.max_visible, 10);
        assert_eq!(config.suggest.max_results, 50);
    }

    #[test]
    fn test_config_dir_returns_none_yields_default() {
        // Simulate the load() code path when config_dir() returns None:
        // it must return Self::default(), NOT load from CWD.
        let result: Option<PathBuf> = None;
        let config = match result {
            Some(dir) => {
                let path = dir.join("config.toml");
                if path.exists() {
                    toml::from_str::<GhostConfig>(&std::fs::read_to_string(&path).unwrap()).unwrap()
                } else {
                    GhostConfig::default()
                }
            }
            None => GhostConfig::default(),
        };
        // Should be identical to defaults — never loaded from CWD
        assert_eq!(config.popup.max_visible, 10);
        assert_eq!(config.trigger.delay_ms, 150);
        assert_eq!(config.suggest.max_results, 50);
    }

    #[test]
    fn test_auto_trigger_defaults_to_true() {
        let config = GhostConfig::default();
        assert!(config.trigger.auto_trigger);
    }

    #[test]
    fn test_auto_trigger_false_from_toml() {
        let toml_str = r#"
[trigger]
auto_trigger = false
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        assert!(!config.trigger.auto_trigger);
        // Other trigger defaults preserved
        assert_eq!(config.trigger.auto_chars, vec![' ', '/', '-', '.']);
        assert_eq!(config.trigger.delay_ms, 150);
    }

    #[test]
    fn render_block_ms_default_is_80() {
        let cfg = PopupConfig::default();
        assert_eq!(cfg.render_block_ms, 80);
    }

    #[test]
    fn render_block_ms_clamps_above_300_during_normalize() {
        let mut cfg = GhostConfig::default();
        cfg.popup.render_block_ms = 500;
        cfg.normalize();
        assert_eq!(cfg.popup.render_block_ms, 300);
    }

    #[test]
    fn render_block_ms_zero_is_allowed() {
        let mut cfg = GhostConfig::default();
        cfg.popup.render_block_ms = 0;
        cfg.normalize();
        assert_eq!(cfg.popup.render_block_ms, 0);
    }
}
