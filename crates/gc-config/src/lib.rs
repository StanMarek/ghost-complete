//! TOML configuration, keybinding definitions, and color themes.
//!
//! Reads from `~/.config/ghost-complete/config.toml` with serde deserialization
//! and sensible defaults for all fields.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{de, Deserialize, Deserializer, Serialize};

fn deserialize_saturating_u16<'de, D>(deserializer: D) -> std::result::Result<u16, D::Error>
where
    D: Deserializer<'de>,
{
    let value = i64::deserialize(deserializer)?;
    if value < 0 {
        return Err(de::Error::invalid_value(
            de::Unexpected::Signed(value),
            &"a nonnegative integer",
        ));
    }
    if value > i64::from(u16::MAX) {
        // The post-clamp value would otherwise show up in normalize()'s warning
        // (always 65535), losing the user's original magnitude. Surface the
        // raw value here so the operator can spot the typo.
        tracing::warn!(
            "config value {} exceeds u16::MAX ({}); saturating before normalization",
            value,
            u16::MAX,
        );
    }

    Ok(value.min(i64::from(u16::MAX)) as u16)
}

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExperimentalConfig {
    pub multi_terminal: bool,
    pub aws_sdk_provider: bool,
    pub aws_sdk_fallback_to_cli: bool,
    /// Cap on the number of formulae returned by `brew search ""`. Brew's
    /// full index has tens of thousands of entries; the engine ranks
    /// fuzz-matches over this list and rendering past ~1k swamps the popup.
    /// Defaults to 1000; raise for unfiltered exploration, lower for slower
    /// machines.
    pub brew_search_cap: usize,
}

impl Default for ExperimentalConfig {
    fn default() -> Self {
        Self {
            multi_terminal: false,
            aws_sdk_provider: false,
            aws_sdk_fallback_to_cli: true,
            brew_search_cap: 1_000,
        }
    }
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
    /// Empty/Error feedback dismiss delay (ms); 0 disables. Clamped to [0, 10000]. Default 1200.
    #[serde(deserialize_with = "deserialize_saturating_u16")]
    pub feedback_dismiss_ms: u16,
    /// Animate Loading feedback with a spinner; narrow popups fall back to ellipsis. Default true.
    pub spinner: bool,
    /// Show provider names in error feedback; default false to avoid leaking on shared screens.
    pub show_provider_errors: bool,
    /// Maximum time (ms) the popup will block waiting for a higher-priority
    /// async generator before painting whatever sync results we have. Set
    /// to `0` to disable blocking entirely (paint immediately, merge async
    /// later). Clamped to `[0, 300]` during normalization. Default: 80 ms,
    /// chosen to stay below the human perception threshold for "instant".
    #[serde(deserialize_with = "deserialize_saturating_u16")]
    pub render_block_ms: u16,
    /// Minimum popup width in display columns. Clamped to `[10, 500]`
    /// during normalization. Default 20.
    #[serde(deserialize_with = "deserialize_saturating_u16")]
    pub min_width: u16,
    /// Maximum popup width in display columns. Clamped to `[min_width, 500]`
    /// (or to `screen_cols` at render time, whichever is smaller).
    /// Increase this on wide terminals to give descriptions more room before
    /// the truncation ellipsis kicks in. Default 60.
    #[serde(deserialize_with = "deserialize_saturating_u16")]
    pub max_width: u16,
    /// Description box mode. When `"side"`, an adjacent box is rendered next
    /// to the main popup with the selected suggestion's full description,
    /// wrapped to multiple lines. `"off"` keeps the legacy inline-truncated
    /// behavior. Default `"off"`.
    ///
    /// The runtime stores the sibling tuning fields
    /// (`description_box_max_width`, `description_box_lines`,
    /// `description_box_debounce_ms`) regardless of mode and gates actual
    /// rendering on `description_box == Side`.
    pub description_box: DescriptionBoxMode,
    /// Maximum width (display columns) for the description box. Clamped to
    /// `[20, 200]` during normalization. The actual rendered width is
    /// `min(this, remaining columns next to main popup)`. Default 60.
    #[serde(deserialize_with = "deserialize_saturating_u16")]
    pub description_box_max_width: u16,
    /// Maximum number of wrapped lines in the description box. Long
    /// descriptions are hard-truncated with an ellipsis on the final line.
    /// `0` resets to default 5; values above 20 clamp to 20. Default 5.
    #[serde(deserialize_with = "deserialize_saturating_u16")]
    pub description_box_lines: u16,
    /// Debounce window (ms) for description-box updates on selection change.
    /// Holding arrow keys causes the box to update at most once per window,
    /// avoiding flicker. `0` disables debounce. Clamped to `[0, 500]`.
    /// Default 80, matching `render_block_ms`.
    #[serde(deserialize_with = "deserialize_saturating_u16")]
    pub description_box_debounce_ms: u16,
}

impl Default for PopupConfig {
    fn default() -> Self {
        Self {
            max_visible: 10,
            borders: false,
            feedback_dismiss_ms: 1200,
            spinner: true,
            show_provider_errors: false,
            render_block_ms: 80,
            min_width: 20,
            max_width: 60,
            description_box: DescriptionBoxMode::Off,
            description_box_max_width: 60,
            description_box_lines: 5,
            description_box_debounce_ms: 80,
        }
    }
}

/// Behavior for the optional adjacent description box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DescriptionBoxMode {
    /// Legacy inline-truncated description in the main popup row only.
    #[default]
    Off,
    /// Adjacent box rendered to the side of (or below) the main popup, with
    /// wrapped multi-line description for the selected suggestion.
    Side,
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
    pub spec_cache: SpecCacheConfig,
}

impl Default for SuggestConfig {
    fn default() -> Self {
        Self {
            max_results: 50,
            max_history_results: 5,
            generator_timeout_ms: 5000,
            providers: ProvidersConfig::default(),
            spec_cache: SpecCacheConfig::default(),
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
    /// Global kill switch for the QuickJS evaluator that backs
    /// `requires_js` generators. Default `true` so JS-backed
    /// completions work out of the box; users can disable locally
    /// with `[suggest.providers] js_runtime = false` in their
    /// `config.toml`.
    ///
    /// When `false`, the suggestion engine skips every JS-backed
    /// `requires_js` generator whose `js_runtime.kind` is populated
    /// (`post_process`, `script_function`, or `custom`). Static spec
    /// completions remain available.
    pub js_runtime: bool,
}

impl Default for ProvidersConfig {
    fn default() -> Self {
        Self {
            commands: true,
            filesystem: true,
            specs: true,
            git: true,
            js_runtime: true,
        }
    }
}

/// Cache eviction policy for parsed completion specs. Eviction is opt-in:
/// `idle_ttl_secs = 0` (default) preserves the lazy-loading layer's
/// "parse once, hold forever" behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SpecCacheConfig {
    /// Seconds after last access before a successfully-parsed spec is
    /// evicted from the in-memory cache. `0` (default) disables eviction.
    pub idle_ttl_secs: u64,
    /// How often the background sweep wakes to scan for idle entries.
    /// Default 60s. Ignored when `idle_ttl_secs == 0`.
    pub sweep_interval_secs: u64,
    /// Spec aliases (filename stems or `CompletionSpec.name` values) that
    /// must never be evicted. Shell aliases are NOT walked; pin specs by
    /// their registered name. Default empty.
    pub keep_warm: Vec<String>,
    /// LRU backstop: after TTL eviction, if total estimated resident heap
    /// exceeds this cap, evict more entries oldest-access-first until under
    /// cap. `keep_warm` entries are exempt. `0` (default) disables.
    pub max_resident_mb: u64,
}

impl Default for SpecCacheConfig {
    fn default() -> Self {
        Self {
            idle_ttl_secs: 0,
            sweep_interval_secs: 60,
            keep_warm: Vec::new(),
            max_resident_mb: 0,
        }
    }
}

impl SpecCacheConfig {
    /// Convert `max_resident_mb` to a byte cap. `None` when disabled.
    /// Saturates at `u64::MAX` for pathological user input instead of
    /// wrapping to a too-small cap.
    pub fn max_resident_bytes(&self) -> Option<u64> {
        if self.max_resident_mb == 0 {
            None
        } else {
            Some(self.max_resident_mb.saturating_mul(1024 * 1024))
        }
    }

    /// True when eviction is enabled (`idle_ttl_secs > 0`).
    pub fn enabled(&self) -> bool {
        self.idle_ttl_secs > 0
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback_loading: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback_empty: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback_error: Option<String>,
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
    pub feedback_loading: String,
    pub feedback_empty: String,
    pub feedback_error: String,
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
        validate_opt_style("theme.feedback_loading", self.feedback_loading.as_deref())?;
        validate_opt_style("theme.feedback_empty", self.feedback_empty.as_deref())?;
        validate_opt_style("theme.feedback_error", self.feedback_error.as_deref())?;
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
            feedback_loading: self
                .feedback_loading
                .clone()
                .unwrap_or(base.feedback_loading),
            feedback_empty: self.feedback_empty.clone().unwrap_or(base.feedback_empty),
            feedback_error: self.feedback_error.clone().unwrap_or(base.feedback_error),
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
            feedback_loading: "dim".into(),
            feedback_empty: "dim".into(),
            feedback_error: "dim fg:#f38ba8".into(),
        },
        "light" => ResolvedTheme {
            selected: "fg:#1e1e2e bg:#dce0e8 bold".into(),
            description: "fg:#6c6f85".into(),
            match_highlight: "fg:#d20f39 bold".into(),
            item_text: String::new(),
            scrollbar: "fg:#9ca0b0".into(),
            border: "fg:#9ca0b0".into(),
            feedback_loading: "fg:#6c6f85".into(),
            feedback_empty: "fg:#6c6f85".into(),
            feedback_error: "dim fg:#d20f39".into(),
        },
        "catppuccin" => ResolvedTheme {
            selected: "fg:#cdd6f4 bg:#585b70 bold".into(),
            description: "fg:#6c7086".into(),
            match_highlight: "fg:#f9e2af bold".into(),
            item_text: String::new(),
            scrollbar: "fg:#585b70".into(),
            border: "fg:#585b70".into(),
            feedback_loading: "fg:#6c7086".into(),
            feedback_empty: "fg:#6c7086".into(),
            feedback_error: "dim fg:#f38ba8".into(),
        },
        "material-darker" => ResolvedTheme {
            selected: "fg:#eeffff bg:#424242 bold".into(),
            description: "fg:#616161".into(),
            match_highlight: "fg:#ffcb6b bold".into(),
            item_text: String::new(),
            scrollbar: "fg:#424242".into(),
            border: "fg:#424242".into(),
            feedback_loading: "fg:#616161".into(),
            feedback_empty: "fg:#616161".into(),
            feedback_error: "dim fg:#ff5370".into(),
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
const FEEDBACK_DISMISS_MS_UPPER: u16 = 10_000;
const POPUP_MIN_WIDTH_FLOOR: u16 = 10;
const POPUP_MAX_WIDTH_CEILING: u16 = 500;
const DESC_BOX_MAX_WIDTH_FLOOR: u16 = 20;
const DESC_BOX_MAX_WIDTH_CEILING: u16 = 200;
const DESC_BOX_LINES_CEILING: u16 = 20;
const DESC_BOX_DEBOUNCE_CEILING: u16 = 500;

/// Returns every leaf field path in dotted form, e.g. `popup.render_block_ms`.
/// Used by drift tests in `ghost-complete` to verify the install template,
/// TUI editor metadata, and `docs/CONFIGURATION.md` hot-reload table stay
/// in sync with the schema.
///
/// New schema fields MUST appear here. Adding via copy-paste from
/// `GhostConfig` is acceptable; the cost of forgetting is a failing
/// drift test, not a runtime bug.
pub fn all_field_paths() -> Vec<&'static str> {
    vec![
        // [trigger]
        "trigger.auto_chars",
        "trigger.delay_ms",
        "trigger.auto_trigger",
        // [popup]
        "popup.max_visible",
        "popup.borders",
        "popup.feedback_dismiss_ms",
        "popup.spinner",
        "popup.show_provider_errors",
        "popup.render_block_ms",
        "popup.min_width",
        "popup.max_width",
        "popup.description_box",
        "popup.description_box_max_width",
        "popup.description_box_lines",
        "popup.description_box_debounce_ms",
        // [suggest]
        "suggest.max_results",
        "suggest.max_history_results",
        "suggest.generator_timeout_ms",
        // [suggest.providers]
        "suggest.providers.commands",
        "suggest.providers.filesystem",
        "suggest.providers.specs",
        "suggest.providers.git",
        "suggest.providers.js_runtime",
        // [suggest.spec_cache]
        "suggest.spec_cache.idle_ttl_secs",
        "suggest.spec_cache.sweep_interval_secs",
        "suggest.spec_cache.keep_warm",
        "suggest.spec_cache.max_resident_mb",
        // [paths]
        "paths.spec_dirs",
        // [keybindings] — 6 fields
        "keybindings.accept",
        "keybindings.accept_and_enter",
        "keybindings.dismiss",
        "keybindings.navigate_up",
        "keybindings.navigate_down",
        "keybindings.trigger",
        // [theme] — 10 fields
        "theme.preset",
        "theme.selected",
        "theme.description",
        "theme.match_highlight",
        "theme.item_text",
        "theme.scrollbar",
        "theme.border",
        "theme.feedback_loading",
        "theme.feedback_empty",
        "theme.feedback_error",
        // [experimental]
        "experimental.multi_terminal",
        "experimental.aws_sdk_provider",
        "experimental.aws_sdk_fallback_to_cli",
        "experimental.brew_search_cap",
    ]
}

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
        if self.popup.feedback_dismiss_ms > FEEDBACK_DISMISS_MS_UPPER {
            tracing::warn!(
                "popup.feedback_dismiss_ms={} exceeds maximum {}, clamping",
                self.popup.feedback_dismiss_ms,
                FEEDBACK_DISMISS_MS_UPPER,
            );
            self.popup.feedback_dismiss_ms = FEEDBACK_DISMISS_MS_UPPER;
        }
        // Popup width sanity. min_width is clamped first so the max_width
        // clamp can rely on a valid lower bound.
        if self.popup.min_width < POPUP_MIN_WIDTH_FLOOR {
            tracing::warn!(
                "popup.min_width={} below floor {}, clamping",
                self.popup.min_width,
                POPUP_MIN_WIDTH_FLOOR,
            );
            self.popup.min_width = POPUP_MIN_WIDTH_FLOOR;
        }
        if self.popup.min_width > POPUP_MAX_WIDTH_CEILING {
            tracing::warn!(
                "popup.min_width={} exceeds ceiling {}, clamping",
                self.popup.min_width,
                POPUP_MAX_WIDTH_CEILING,
            );
            self.popup.min_width = POPUP_MAX_WIDTH_CEILING;
        }
        if self.popup.max_width > POPUP_MAX_WIDTH_CEILING {
            tracing::warn!(
                "popup.max_width={} exceeds ceiling {}, clamping",
                self.popup.max_width,
                POPUP_MAX_WIDTH_CEILING,
            );
            self.popup.max_width = POPUP_MAX_WIDTH_CEILING;
        }
        if self.popup.max_width < self.popup.min_width {
            tracing::warn!(
                "popup.max_width={} < popup.min_width={}, raising max to min",
                self.popup.max_width,
                self.popup.min_width,
            );
            self.popup.max_width = self.popup.min_width;
        }
        // Description box knobs.
        if self.popup.description_box_max_width < DESC_BOX_MAX_WIDTH_FLOOR {
            tracing::warn!(
                "popup.description_box_max_width={} below floor {}, clamping",
                self.popup.description_box_max_width,
                DESC_BOX_MAX_WIDTH_FLOOR,
            );
            self.popup.description_box_max_width = DESC_BOX_MAX_WIDTH_FLOOR;
        }
        if self.popup.description_box_max_width > DESC_BOX_MAX_WIDTH_CEILING {
            tracing::warn!(
                "popup.description_box_max_width={} exceeds ceiling {}, clamping",
                self.popup.description_box_max_width,
                DESC_BOX_MAX_WIDTH_CEILING,
            );
            self.popup.description_box_max_width = DESC_BOX_MAX_WIDTH_CEILING;
        }
        if self.popup.description_box_lines == 0 {
            tracing::warn!(
                "popup.description_box_lines=0 is invalid (would render an empty box), \
                 clamping to default 5",
            );
            self.popup.description_box_lines = 5;
        }
        if self.popup.description_box_lines > DESC_BOX_LINES_CEILING {
            tracing::warn!(
                "popup.description_box_lines={} exceeds ceiling {}, clamping",
                self.popup.description_box_lines,
                DESC_BOX_LINES_CEILING,
            );
            self.popup.description_box_lines = DESC_BOX_LINES_CEILING;
        }
        if self.popup.description_box_debounce_ms > DESC_BOX_DEBOUNCE_CEILING {
            tracing::warn!(
                "popup.description_box_debounce_ms={} exceeds ceiling {}, clamping",
                self.popup.description_box_debounce_ms,
                DESC_BOX_DEBOUNCE_CEILING,
            );
            self.popup.description_box_debounce_ms = DESC_BOX_DEBOUNCE_CEILING;
        }
        // Spec cache sanity. Only apply when eviction is enabled —
        // a config with idle_ttl_secs=0 means the user opted out and
        // the other fields are unused.
        if self.suggest.spec_cache.idle_ttl_secs > 0 {
            if self.suggest.spec_cache.sweep_interval_secs == 0 {
                tracing::warn!(
                    "suggest.spec_cache.sweep_interval_secs=0 with eviction enabled \
                     is invalid; clamping to 60"
                );
                self.suggest.spec_cache.sweep_interval_secs = 60;
            }
            if self.suggest.spec_cache.sweep_interval_secs >= self.suggest.spec_cache.idle_ttl_secs
            {
                tracing::warn!(
                    sweep_interval = self.suggest.spec_cache.sweep_interval_secs,
                    idle_ttl = self.suggest.spec_cache.idle_ttl_secs,
                    "sweep_interval_secs >= idle_ttl_secs — eviction will lag the \
                     configured TTL"
                );
            }
        }
    }

    pub fn load(path: Option<&Path>) -> Result<Self> {
        let config_path = match path {
            Some(p) => p.to_path_buf(),
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
    fn all_field_paths_covers_every_section() {
        let paths = all_field_paths();
        let expected_sections = &[
            "trigger.",
            "popup.",
            "suggest.",
            "suggest.providers.",
            "suggest.spec_cache.",
            "paths.",
            "keybindings.",
            "theme.",
            "experimental.",
        ];
        for prefix in expected_sections {
            assert!(
                paths.iter().any(|p| p.starts_with(prefix)),
                "missing section: {}",
                prefix,
            );
        }
    }

    #[test]
    fn all_field_paths_includes_render_block_ms() {
        let paths = all_field_paths();
        assert!(paths.contains(&"popup.render_block_ms"));
        assert!(paths.contains(&"suggest.providers.js_runtime"));
        assert!(paths.contains(&"experimental.brew_search_cap"));
        assert!(paths.contains(&"suggest.spec_cache.idle_ttl_secs"));
    }

    #[test]
    fn test_default_config_matches_hardcoded() {
        let config = GhostConfig::default();
        assert_eq!(config.trigger.auto_chars, vec![' ', '/', '-', '.']);
        assert_eq!(config.trigger.delay_ms, 150);
        assert!(config.trigger.auto_trigger);
        assert_eq!(config.popup.max_visible, 10);
        assert_eq!(config.popup.feedback_dismiss_ms, 1200);
        assert!(config.popup.spinner);
        assert!(!config.popup.show_provider_errors);
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
        assert_eq!(config.theme.feedback_loading, None);
        assert_eq!(config.theme.feedback_empty, None);
        assert_eq!(config.theme.feedback_error, None);
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
    fn spec_cache_config_defaults() {
        let config = SpecCacheConfig::default();
        assert_eq!(config.idle_ttl_secs, 0, "eviction must be opt-in (TTL=0)");
        assert_eq!(config.sweep_interval_secs, 60);
        assert!(config.keep_warm.is_empty());
        assert_eq!(config.max_resident_mb, 0);
        assert_eq!(config.max_resident_bytes(), None);
    }

    #[test]
    fn spec_cache_config_max_resident_bytes_translates_mb() {
        let config = SpecCacheConfig {
            max_resident_mb: 100,
            ..Default::default()
        };
        assert_eq!(config.max_resident_bytes(), Some(100 * 1024 * 1024));
    }

    #[test]
    fn spec_cache_config_max_resident_bytes_saturates_on_overflow() {
        let config = SpecCacheConfig {
            max_resident_mb: u64::MAX,
            ..Default::default()
        };
        assert_eq!(config.max_resident_bytes(), Some(u64::MAX));
    }

    #[test]
    fn suggest_config_includes_spec_cache_with_defaults() {
        let config = SuggestConfig::default();
        assert_eq!(config.spec_cache.idle_ttl_secs, 0);
    }

    #[test]
    fn spec_cache_deserializes_from_toml() {
        let toml = r#"
[suggest.spec_cache]
idle_ttl_secs = 300
sweep_interval_secs = 30
keep_warm = ["git", "cd"]
max_resident_mb = 100
"#;
        let parsed: GhostConfig = toml::from_str(toml).unwrap();
        assert_eq!(parsed.suggest.spec_cache.idle_ttl_secs, 300);
        assert_eq!(parsed.suggest.spec_cache.sweep_interval_secs, 30);
        assert_eq!(parsed.suggest.spec_cache.keep_warm, vec!["git", "cd"]);
        assert_eq!(parsed.suggest.spec_cache.max_resident_mb, 100);
    }

    #[test]
    fn spec_cache_normalize_clamps_zero_sweep_interval_when_eviction_enabled() {
        let mut config = GhostConfig {
            suggest: SuggestConfig {
                spec_cache: SpecCacheConfig {
                    idle_ttl_secs: 300,
                    sweep_interval_secs: 0,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        };
        config.normalize();
        assert_eq!(
            config.suggest.spec_cache.sweep_interval_secs, 60,
            "sweep_interval_secs=0 with eviction enabled must clamp to 60"
        );
    }

    #[test]
    fn spec_cache_normalize_does_not_touch_disabled_eviction() {
        let mut config = GhostConfig::default();
        // idle_ttl_secs=0 (eviction disabled) — no normalization triggered.
        config.suggest.spec_cache.sweep_interval_secs = 0;
        config.normalize();
        assert_eq!(
            config.suggest.spec_cache.sweep_interval_secs, 0,
            "disabled eviction should not auto-fix sweep_interval"
        );
    }

    #[test]
    fn test_missing_file_returns_default() {
        let config = GhostConfig::load(Some(Path::new("/nonexistent/path/config.toml"))).unwrap();
        assert_eq!(config.popup.max_visible, 10);
    }

    #[test]
    fn test_malformed_toml_returns_error() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "this is not [valid toml = {{}}").unwrap();
        let result = GhostConfig::load(Some(tmp.path()));
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
feedback_loading = "bold fg:#89b4fa"
feedback_empty = "dim fg:244"
feedback_error = "dim fg:#d20f39"
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.theme.selected.as_deref(), Some("fg:255 bg:236"));
        assert_eq!(config.theme.description.as_deref(), Some("dim underline"));
        assert_eq!(
            config.theme.feedback_loading.as_deref(),
            Some("bold fg:#89b4fa")
        );
        assert_eq!(config.theme.feedback_empty.as_deref(), Some("dim fg:244"));
        assert_eq!(
            config.theme.feedback_error.as_deref(),
            Some("dim fg:#d20f39")
        );
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
        assert_eq!(resolved.feedback_loading, "dim");
        assert_eq!(resolved.feedback_empty, "dim");
        assert_eq!(resolved.feedback_error, "dim fg:#f38ba8");
    }

    #[test]
    fn test_feedback_theme_overrides_resolve_and_validate() {
        let config = ThemeConfig {
            feedback_loading: Some("bold fg:#89b4fa".into()),
            feedback_empty: Some("dim".into()),
            feedback_error: Some("fg:196".into()),
            ..ThemeConfig::default()
        };
        config.validate().unwrap();
        let resolved = config.resolve().unwrap();
        assert_eq!(resolved.feedback_loading, "bold fg:#89b4fa");
        assert_eq!(resolved.feedback_empty, "dim");
        assert_eq!(resolved.feedback_error, "fg:196");
    }

    #[test]
    fn test_feedback_theme_preset_defaults() {
        let presets = [
            ("dark", "dim fg:#f38ba8"),
            ("light", "dim fg:#d20f39"),
            ("catppuccin", "dim fg:#f38ba8"),
            ("material-darker", "dim fg:#ff5370"),
        ];
        for (preset, error_style) in presets {
            let config = ThemeConfig {
                preset: preset.into(),
                ..ThemeConfig::default()
            };
            let resolved = config.resolve().unwrap();
            assert_eq!(resolved.feedback_loading, resolved.description);
            assert_eq!(resolved.feedback_empty, resolved.description);
            assert_eq!(resolved.feedback_error, error_style);
        }
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
    fn test_popup_width_fields_parse() {
        let toml_str = r#"
[popup]
max_visible = 10
min_width = 25
max_width = 80
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.popup.max_visible, 10);
        assert_eq!(config.popup.min_width, 25);
        assert_eq!(config.popup.max_width, 80);
    }

    #[test]
    fn test_popup_width_defaults() {
        let cfg = PopupConfig::default();
        assert_eq!(cfg.min_width, 20);
        assert_eq!(cfg.max_width, 60);
    }

    #[test]
    fn test_popup_max_width_clamps_ceiling() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "[popup]\nmax_width = 1000").unwrap();
        let config = GhostConfig::load(Some(tmp.path())).unwrap();
        assert_eq!(config.popup.max_width, 500);
    }

    #[test]
    fn test_popup_max_width_above_u16_still_clamps_ceiling() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "[popup]\nmax_width = 100000").unwrap();
        let config = GhostConfig::load(Some(tmp.path())).unwrap();
        assert_eq!(config.popup.max_width, 500);
    }

    #[test]
    fn test_popup_min_width_clamps_floor() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "[popup]\nmin_width = 1").unwrap();
        let config = GhostConfig::load(Some(tmp.path())).unwrap();
        assert_eq!(config.popup.min_width, 10);
    }

    #[test]
    fn test_popup_min_width_clamps_ceiling() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "[popup]\nmin_width = 600").unwrap();
        let config = GhostConfig::load(Some(tmp.path())).unwrap();
        assert_eq!(config.popup.min_width, 500);
        assert_eq!(config.popup.max_width, 500);
    }

    #[test]
    fn test_popup_min_width_above_u16_still_clamps_ceiling() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "[popup]\nmin_width = 100000").unwrap();
        let config = GhostConfig::load(Some(tmp.path())).unwrap();
        assert_eq!(config.popup.min_width, 500);
        assert_eq!(config.popup.max_width, 500);
    }

    #[test]
    fn test_popup_max_below_min_raised_to_min() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "[popup]\nmin_width = 50\nmax_width = 30").unwrap();
        let config = GhostConfig::load(Some(tmp.path())).unwrap();
        assert_eq!(config.popup.min_width, 50);
        assert_eq!(config.popup.max_width, 50);
    }

    #[test]
    fn test_description_box_defaults_off() {
        let cfg = PopupConfig::default();
        assert_eq!(cfg.description_box, DescriptionBoxMode::Off);
        assert_eq!(cfg.description_box_max_width, 60);
        assert_eq!(cfg.description_box_lines, 5);
        assert_eq!(cfg.description_box_debounce_ms, 80);
    }

    #[test]
    fn test_description_box_mode_parses_lowercase() {
        let toml_str = r#"
[popup]
description_box = "side"
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.popup.description_box, DescriptionBoxMode::Side);
    }

    #[test]
    fn test_description_box_lines_zero_clamps_to_default() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "[popup]\ndescription_box_lines = 0").unwrap();
        let config = GhostConfig::load(Some(tmp.path())).unwrap();
        assert_eq!(config.popup.description_box_lines, 5);
    }

    #[test]
    fn test_description_box_lines_clamps_ceiling() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "[popup]\ndescription_box_lines = 999").unwrap();
        let config = GhostConfig::load(Some(tmp.path())).unwrap();
        assert_eq!(config.popup.description_box_lines, 20);
    }

    #[test]
    fn test_description_box_lines_above_u16_still_clamps_ceiling() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "[popup]\ndescription_box_lines = 100000").unwrap();
        let config = GhostConfig::load(Some(tmp.path())).unwrap();
        assert_eq!(config.popup.description_box_lines, 20);
    }

    #[test]
    fn test_description_box_max_width_clamps_floor() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "[popup]\ndescription_box_max_width = 5").unwrap();
        let config = GhostConfig::load(Some(tmp.path())).unwrap();
        assert_eq!(config.popup.description_box_max_width, 20);
    }

    #[test]
    fn test_description_box_max_width_zero_clamps_to_floor() {
        // Pin the documented contract: 0 must clamp up to DESC_BOX_MAX_WIDTH_FLOOR (20),
        // not pass through. Guards against a regression that swapped `<` for
        // `> 0 && <`, which would let zero leak through and render a degenerate box.
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "[popup]\ndescription_box_max_width = 0").unwrap();
        let config = GhostConfig::load(Some(tmp.path())).unwrap();
        assert_eq!(config.popup.description_box_max_width, 20);
    }

    #[test]
    fn test_description_box_max_width_clamps_ceiling() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "[popup]\ndescription_box_max_width = 9999").unwrap();
        let config = GhostConfig::load(Some(tmp.path())).unwrap();
        assert_eq!(config.popup.description_box_max_width, 200);
    }

    #[test]
    fn test_description_box_max_width_above_u16_still_clamps_ceiling() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "[popup]\ndescription_box_max_width = 100000").unwrap();
        let config = GhostConfig::load(Some(tmp.path())).unwrap();
        assert_eq!(config.popup.description_box_max_width, 200);
    }

    #[test]
    fn test_description_box_debounce_ms_clamps_ceiling() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "[popup]\ndescription_box_debounce_ms = 9999").unwrap();
        let config = GhostConfig::load(Some(tmp.path())).unwrap();
        assert_eq!(config.popup.description_box_debounce_ms, 500);
    }

    #[test]
    fn test_description_box_debounce_ms_above_u16_still_clamps_ceiling() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "[popup]\ndescription_box_debounce_ms = 100000").unwrap();
        let config = GhostConfig::load(Some(tmp.path())).unwrap();
        assert_eq!(config.popup.description_box_debounce_ms, 500);
    }

    #[test]
    fn test_popup_negative_min_width_rejected() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "[popup]\nmin_width = -10").unwrap();
        let result = GhostConfig::load(Some(tmp.path()));
        let err = result.expect_err("negative min_width must be rejected");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("nonnegative integer"),
            "error must mention the expected shape, got: {msg}",
        );
    }

    #[test]
    fn test_popup_negative_description_box_lines_rejected() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "[popup]\ndescription_box_lines = -1").unwrap();
        let result = GhostConfig::load(Some(tmp.path()));
        let err = result.expect_err("negative description_box_lines must be rejected");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("nonnegative integer"),
            "error must mention the expected shape, got: {msg}",
        );
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
    fn experimental_aws_sdk_defaults_are_conservative() {
        let config = GhostConfig::default();
        assert!(!config.experimental.aws_sdk_provider);
        assert!(config.experimental.aws_sdk_fallback_to_cli);
    }

    #[test]
    fn experimental_aws_sdk_flags_parse_from_toml() {
        let toml_str = r#"
[experimental]
aws_sdk_provider = true
aws_sdk_fallback_to_cli = false
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        assert!(config.experimental.aws_sdk_provider);
        assert!(!config.experimental.aws_sdk_fallback_to_cli);
    }

    #[test]
    fn experimental_brew_search_cap_defaults_and_parses() {
        let default_config = GhostConfig::default();
        assert_eq!(default_config.experimental.brew_search_cap, 1_000);

        let toml_str = r#"
[experimental]
brew_search_cap = 250
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.experimental.brew_search_cap, 250);
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
        let config = GhostConfig::load(Some(tmp.path())).unwrap();
        assert_eq!(config.popup.max_visible, MAX_VISIBLE_UPPER);
    }

    #[test]
    fn test_clamp_max_results_over_limit() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "[suggest]\nmax_results = 999999").unwrap();
        let config = GhostConfig::load(Some(tmp.path())).unwrap();
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
        let config = GhostConfig::load(Some(tmp.path())).unwrap();
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
        let config = GhostConfig::load(Some(tmp.path())).unwrap();
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
        let config = GhostConfig::load(Some(tmp.path())).unwrap();
        assert_eq!(config.suggest.max_results, MAX_RESULTS_DEFAULT);
    }

    #[test]
    fn test_clamp_max_visible_zero_to_default() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "[popup]\nmax_visible = 0").unwrap();
        let config = GhostConfig::load(Some(tmp.path())).unwrap();
        assert_eq!(config.popup.max_visible, 10);
    }

    #[test]
    fn test_popup_feedback_knobs_parse_and_clamp() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            tmp,
            "[popup]\nfeedback_dismiss_ms = 20000\nspinner = false\nshow_provider_errors = true"
        )
        .unwrap();
        let config = GhostConfig::load(Some(tmp.path())).unwrap();
        assert_eq!(config.popup.feedback_dismiss_ms, 10000);
        assert!(!config.popup.spinner);
        assert!(config.popup.show_provider_errors);
    }

    #[test]
    fn test_delay_ms_zero_is_allowed() {
        // delay_ms=0 disables the typing-pause debounce — still a valid
        // choice, so it must pass through untouched.
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "[trigger]\ndelay_ms = 0").unwrap();
        let config = GhostConfig::load(Some(tmp.path())).unwrap();
        assert_eq!(config.trigger.delay_ms, 0);
    }

    #[test]
    fn test_feedback_dismiss_ms_zero_is_allowed() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "[popup]\nfeedback_dismiss_ms = 0").unwrap();
        let config = GhostConfig::load(Some(tmp.path())).unwrap();
        assert_eq!(config.popup.feedback_dismiss_ms, 0);
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
        let result = GhostConfig::load(Some(tmp.path()));
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
        let config = GhostConfig::load(Some(tmp.path())).unwrap();
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
        let config = GhostConfig::load(Some(tmp.path())).unwrap();
        // Known fields still applied correctly.
        assert_eq!(config.trigger.delay_ms, 200);
        assert_eq!(config.suggest.max_results, 75);
    }

    #[test]
    fn test_missing_file_returns_default_via_notfound() {
        // Verifies the TOCTOU-safe path: read_to_string NotFound → default
        let config =
            GhostConfig::load(Some(Path::new("/tmp/definitely_not_a_real_config_42.toml")))
                .unwrap();
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

#[cfg(test)]
mod docs_drift_tests {
    use super::all_field_paths;

    /// Read docs/CONFIGURATION.md via the workspace root computed from
    /// CARGO_MANIFEST_DIR (gc-config is at `<root>/crates/gc-config`,
    /// so we go up two levels).
    fn configuration_md() -> String {
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let root = manifest
            .parent()
            .expect("crates/")
            .parent()
            .expect("repo root");
        let path = root.join("docs/CONFIGURATION.md");
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {}", path.display(), e))
    }

    /// Decide whether a single field key is "documented" in CONFIGURATION.md.
    ///
    /// Accept any of three forms — all are how the docs reference fields:
    ///   1. Markdown code span:        `key`
    ///   2. TOML assignment with ws:   key = ...
    ///   3. TOML assignment no ws:     key=...
    ///
    /// Bare prose matches don't count — many leaf keys (`background`,
    /// `description`, `accept`) are common English and would false-positive
    /// against unrelated docs text.
    fn is_documented(doc: &str, key: &str) -> bool {
        let backtick = format!("`{key}`");
        let toml_eq_ws = format!("{key} =");
        let toml_eq_tight = format!("{key}=");
        doc.contains(&backtick) || doc.contains(&toml_eq_ws) || doc.contains(&toml_eq_tight)
    }

    /// Every schema field must be referenced in CONFIGURATION.md. This is
    /// the actual drift guard — a section-only check would silently allow
    /// a field to be removed from the docs as long as the section header
    /// stayed.
    #[test]
    fn configuration_md_lists_every_field() {
        let doc = configuration_md();
        let mut missing: Vec<&str> = Vec::new();
        for path in all_field_paths() {
            let (_section, key) = path.rsplit_once('.').expect("dotted path");
            if !is_documented(&doc, key) {
                missing.push(path);
            }
        }
        assert!(
            missing.is_empty(),
            "CONFIGURATION.md is missing these schema fields: {:#?}",
            missing,
        );
    }

    /// Section headers are a cheaper smoke check — useful if the field
    /// test fails wholesale (entire section gone) to surface a clearer
    /// failure first.
    #[test]
    fn configuration_md_mentions_every_section() {
        let doc = configuration_md();
        let sections = [
            "[trigger]",
            "[popup]",
            "[suggest]",
            "[suggest.providers]",
            "[suggest.spec_cache]",
            "[paths]",
            "[keybindings]",
            "[theme]",
            "[experimental]",
        ];
        for s in sections {
            assert!(doc.contains(s), "CONFIGURATION.md missing section {}", s);
        }
    }
}
