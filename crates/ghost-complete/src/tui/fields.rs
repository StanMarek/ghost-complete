#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    Bool,
    U16,
    U64,
    Usize,
    String,
    /// Fixed set of valid values.
    Enum(&'static [&'static str]),
    /// Style string like "bold fg:#FF0000".
    StyleString,
    /// Array of characters.
    CharArray,
    /// Array of strings.
    StringArray,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReloadBehavior {
    /// Change applies immediately.
    Live,
    /// Requires restarting the proxy.
    RequiresRestart,
}

#[derive(Debug, Clone)]
pub struct FieldMeta {
    pub section: &'static str,
    pub key: &'static str,
    pub field_type: FieldType,
    pub default: &'static str,
    pub reload: ReloadBehavior,
    pub help: &'static str,
}

pub const SECTIONS: &[&str] = &[
    "trigger",
    "popup",
    "suggest",
    "suggest.providers",
    "suggest.spec_cache",
    "keybindings",
    "theme",
    "paths",
    "experimental",
];

pub fn supports_inherit(field: &FieldMeta) -> bool {
    field.section == "theme" && field.key != "preset"
}

pub fn section_label(section: &str) -> &'static str {
    match section {
        "trigger" => "Trigger",
        "popup" => "Popup",
        "suggest" => "Suggest",
        "suggest.providers" => "Providers",
        "suggest.spec_cache" => "Spec Cache",
        "keybindings" => "Keybindings",
        "theme" => "Theme",
        "paths" => "Paths",
        "experimental" => "Experimental",
        _ => "Unknown",
    }
}

pub fn all_fields() -> Vec<FieldMeta> {
    vec![
        // trigger
        FieldMeta {
            section: "trigger",
            key: "auto_chars",
            field_type: FieldType::CharArray,
            default: "[' ', '/', '-', '.']",
            reload: ReloadBehavior::Live,
            help: "Characters that auto-trigger suggestions after typing",
        },
        FieldMeta {
            section: "trigger",
            key: "delay_ms",
            field_type: FieldType::U64,
            default: "150",
            reload: ReloadBehavior::RequiresRestart,
            help: "Milliseconds to wait after typing before showing suggestions",
        },
        FieldMeta {
            section: "trigger",
            key: "auto_trigger",
            field_type: FieldType::Bool,
            default: "true",
            reload: ReloadBehavior::Live,
            help: "Enable automatic trigger on typing (false = manual trigger only)",
        },
        // popup
        FieldMeta {
            section: "popup",
            key: "max_visible",
            field_type: FieldType::Usize,
            default: "10",
            reload: ReloadBehavior::Live,
            help: "Maximum number of suggestions visible at once (max 50)",
        },
        FieldMeta {
            section: "popup",
            key: "borders",
            field_type: FieldType::Bool,
            default: "false",
            reload: ReloadBehavior::Live,
            help: "Draw box-drawing borders around the popup",
        },
        FieldMeta {
            section: "popup",
            key: "feedback_dismiss_ms",
            field_type: FieldType::U16,
            default: "1200",
            reload: ReloadBehavior::Live,
            help: "Milliseconds before Empty/Error feedback auto-dismisses (0-10000)",
        },
        FieldMeta {
            section: "popup",
            key: "spinner",
            field_type: FieldType::Bool,
            default: "true",
            reload: ReloadBehavior::Live,
            help: "Animate async Loading feedback",
        },
        FieldMeta {
            section: "popup",
            key: "show_provider_errors",
            field_type: FieldType::Bool,
            default: "false",
            reload: ReloadBehavior::Live,
            help: "Show provider names in error feedback",
        },
        FieldMeta {
            section: "popup",
            key: "render_block_ms",
            field_type: FieldType::U16,
            default: "80",
            reload: ReloadBehavior::Live,
            help: "Maximum time (ms) the popup will block waiting for a higher-priority async generator before painting sync results (0-300)",
        },
        FieldMeta {
            section: "popup",
            key: "min_width",
            field_type: FieldType::Usize,
            default: "20",
            reload: ReloadBehavior::Live,
            help: "Minimum popup width in display columns. Clamped to [10, 500]; if max_width is lower after normalization, it is raised to min_width.",
        },
        FieldMeta {
            section: "popup",
            key: "max_width",
            field_type: FieldType::Usize,
            default: "60",
            reload: ReloadBehavior::Live,
            help: "Maximum popup width in display columns (min_width-500). Wider popups give descriptions more room before the ellipsis kicks in.",
        },
        FieldMeta {
            section: "popup",
            key: "description_box",
            field_type: FieldType::Enum(&["off", "side"]),
            default: "off",
            reload: ReloadBehavior::Live,
            help: "Adjacent description box: 'off' = inline truncation only, 'side' = wrapped box beside or below the popup when the inline description would be hidden or truncated; capped by description_box_lines and available rows",
        },
        FieldMeta {
            section: "popup",
            key: "description_box_max_width",
            field_type: FieldType::Usize,
            default: "60",
            reload: ReloadBehavior::Live,
            help: "Maximum width (cols) for the description box (20-200)",
        },
        FieldMeta {
            section: "popup",
            key: "description_box_lines",
            field_type: FieldType::Usize,
            default: "5",
            reload: ReloadBehavior::Live,
            help: "Maximum wrapped lines in the description box (1-20)",
        },
        FieldMeta {
            section: "popup",
            key: "description_box_debounce_ms",
            field_type: FieldType::U64,
            default: "80",
            reload: ReloadBehavior::Live,
            help: "Debounce window (ms) for description-box updates on selection change (0-500)",
        },
        // suggest
        FieldMeta {
            section: "suggest",
            key: "max_results",
            field_type: FieldType::Usize,
            default: "50",
            reload: ReloadBehavior::RequiresRestart,
            help: "Maximum total ranked suggestions (1-10000)",
        },
        FieldMeta {
            section: "suggest",
            key: "max_history_results",
            field_type: FieldType::Usize,
            default: "5",
            reload: ReloadBehavior::RequiresRestart,
            help: "Maximum history suggestions mixed into results",
        },
        FieldMeta {
            section: "suggest",
            key: "generator_timeout_ms",
            field_type: FieldType::U64,
            default: "5000",
            reload: ReloadBehavior::RequiresRestart,
            help: "Timeout in ms for async script generators",
        },
        FieldMeta {
            section: "suggest",
            key: "match_mode",
            field_type: FieldType::Enum(&["fuzzy", "substring"]),
            default: "fuzzy",
            reload: ReloadBehavior::RequiresRestart,
            help: "Query matching: fuzzy (subsequence) or substring (contiguous)",
        },
        // suggest.providers
        FieldMeta {
            section: "suggest.providers",
            key: "commands",
            field_type: FieldType::Bool,
            default: "true",
            reload: ReloadBehavior::RequiresRestart,
            help: "Enable $PATH command completions",
        },
        FieldMeta {
            section: "suggest.providers",
            key: "filesystem",
            field_type: FieldType::Bool,
            default: "true",
            reload: ReloadBehavior::RequiresRestart,
            help: "Enable filesystem path completions",
        },
        FieldMeta {
            section: "suggest.providers",
            key: "specs",
            field_type: FieldType::Bool,
            default: "true",
            reload: ReloadBehavior::RequiresRestart,
            help: "Enable completion spec-based suggestions",
        },
        FieldMeta {
            section: "suggest.providers",
            key: "git",
            field_type: FieldType::Bool,
            default: "true",
            reload: ReloadBehavior::RequiresRestart,
            help: "Enable git branch/tag/remote completions",
        },
        FieldMeta {
            section: "suggest.providers",
            key: "js_runtime",
            field_type: FieldType::Bool,
            default: "true",
            reload: ReloadBehavior::RequiresRestart,
            help: "Global kill switch for QuickJS evaluation of requires_js generators. Disable to skip all JS-backed completions.",
        },
        // suggest.spec_cache
        FieldMeta {
            section: "suggest.spec_cache",
            key: "idle_ttl_secs",
            field_type: FieldType::U64,
            default: "0",
            reload: ReloadBehavior::RequiresRestart,
            help: "Seconds idle before evicting parsed specs (0 disables eviction)",
        },
        FieldMeta {
            section: "suggest.spec_cache",
            key: "sweep_interval_secs",
            field_type: FieldType::U64,
            default: "60",
            reload: ReloadBehavior::RequiresRestart,
            help: "How often the eviction sweep runs (seconds, ignored when eviction disabled)",
        },
        FieldMeta {
            section: "suggest.spec_cache",
            key: "keep_warm",
            field_type: FieldType::StringArray,
            default: "[]",
            reload: ReloadBehavior::RequiresRestart,
            help: "Spec aliases (filename stems) that must never be evicted",
        },
        FieldMeta {
            section: "suggest.spec_cache",
            key: "max_resident_mb",
            field_type: FieldType::U64,
            default: "0",
            reload: ReloadBehavior::RequiresRestart,
            help: "LRU backstop cap in MiB after TTL eviction (0 disables)",
        },
        // keybindings
        FieldMeta {
            section: "keybindings",
            key: "accept",
            field_type: FieldType::String,
            default: "tab",
            reload: ReloadBehavior::Live,
            help: "Key to accept the selected suggestion",
        },
        FieldMeta {
            section: "keybindings",
            key: "accept_and_enter",
            field_type: FieldType::String,
            default: "enter",
            reload: ReloadBehavior::Live,
            help: "Key to accept and execute (insert + Enter)",
        },
        FieldMeta {
            section: "keybindings",
            key: "dismiss",
            field_type: FieldType::String,
            default: "escape",
            reload: ReloadBehavior::Live,
            help: "Key to dismiss the popup",
        },
        FieldMeta {
            section: "keybindings",
            key: "navigate_up",
            field_type: FieldType::String,
            default: "arrow_up",
            reload: ReloadBehavior::Live,
            help: "Key to move selection up",
        },
        FieldMeta {
            section: "keybindings",
            key: "navigate_down",
            field_type: FieldType::String,
            default: "arrow_down",
            reload: ReloadBehavior::Live,
            help: "Key to move selection down",
        },
        FieldMeta {
            section: "keybindings",
            key: "trigger",
            field_type: FieldType::String,
            default: "ctrl+/",
            reload: ReloadBehavior::Live,
            help: "Key to manually trigger suggestions",
        },
        // theme
        FieldMeta {
            section: "theme",
            key: "preset",
            field_type: FieldType::Enum(&["dark", "light", "catppuccin", "material-darker"]),
            default: "dark",
            reload: ReloadBehavior::Live,
            help: "Color theme preset",
        },
        FieldMeta {
            section: "theme",
            key: "selected",
            field_type: FieldType::StyleString,
            default: "",
            reload: ReloadBehavior::Live,
            help: "Style override for selected item",
        },
        FieldMeta {
            section: "theme",
            key: "description",
            field_type: FieldType::StyleString,
            default: "",
            reload: ReloadBehavior::Live,
            help: "Style override for description text",
        },
        FieldMeta {
            section: "theme",
            key: "match_highlight",
            field_type: FieldType::StyleString,
            default: "",
            reload: ReloadBehavior::Live,
            help: "Style for fuzzy-match highlighted chars",
        },
        FieldMeta {
            section: "theme",
            key: "item_text",
            field_type: FieldType::StyleString,
            default: "",
            reload: ReloadBehavior::Live,
            help: "Base text style for suggestion items",
        },
        FieldMeta {
            section: "theme",
            key: "scrollbar",
            field_type: FieldType::StyleString,
            default: "",
            reload: ReloadBehavior::Live,
            help: "Style for the scrollbar track",
        },
        FieldMeta {
            section: "theme",
            key: "border",
            field_type: FieldType::StyleString,
            default: "",
            reload: ReloadBehavior::Live,
            help: "Style for popup borders",
        },
        FieldMeta {
            section: "theme",
            key: "feedback_loading",
            field_type: FieldType::StyleString,
            default: "",
            reload: ReloadBehavior::Live,
            help: "Style for Loading feedback",
        },
        FieldMeta {
            section: "theme",
            key: "feedback_empty",
            field_type: FieldType::StyleString,
            default: "",
            reload: ReloadBehavior::Live,
            help: "Style for Empty feedback",
        },
        FieldMeta {
            section: "theme",
            key: "feedback_error",
            field_type: FieldType::StyleString,
            default: "",
            reload: ReloadBehavior::Live,
            help: "Style for provider Error feedback",
        },
        // paths
        FieldMeta {
            section: "paths",
            key: "spec_dirs",
            field_type: FieldType::StringArray,
            default: "[]",
            reload: ReloadBehavior::RequiresRestart,
            help: "Additional directories to search for completion specs",
        },
        // experimental
        FieldMeta {
            section: "experimental",
            key: "multi_terminal",
            field_type: FieldType::Bool,
            default: "false",
            reload: ReloadBehavior::RequiresRestart,
            help: "Enable proxy in unsupported terminals",
        },
        FieldMeta {
            section: "experimental",
            key: "aws_sdk_provider",
            field_type: FieldType::Bool,
            default: "false",
            reload: ReloadBehavior::RequiresRestart,
            help: "Use native AWS SDK provider for AWS CLI completions (vs. shelling to `aws`). Experimental — opt-in for one release before default.",
        },
        FieldMeta {
            section: "experimental",
            key: "aws_sdk_fallback_to_cli",
            field_type: FieldType::Bool,
            default: "true",
            reload: ReloadBehavior::RequiresRestart,
            help: "When aws_sdk_provider is on but credentials/profile are unavailable, fall back to shelling out to the AWS CLI.",
        },
        FieldMeta {
            section: "experimental",
            key: "brew_search_cap",
            field_type: FieldType::Usize,
            default: "1000",
            reload: ReloadBehavior::RequiresRestart,
            help: "Maximum number of formulae returned by the Homebrew search provider. Lower it on modest machines; raise it to scan more of the formula corpus.",
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_section_has_at_least_one_field() {
        let fields = all_fields();
        for section in SECTIONS {
            assert!(
                fields.iter().any(|f| f.section == *section),
                "section {section:?} declared in SECTIONS has no fields"
            );
        }
    }

    #[test]
    fn every_section_has_a_label() {
        for section in SECTIONS {
            assert_ne!(
                section_label(section),
                "Unknown",
                "section {section:?} is missing a label"
            );
        }
    }

    #[test]
    fn spec_cache_section_exposes_all_config_fields() {
        let fields = all_fields();
        let keys: Vec<&str> = fields
            .iter()
            .filter(|f| f.section == "suggest.spec_cache")
            .map(|f| f.key)
            .collect();
        for expected in [
            "idle_ttl_secs",
            "sweep_interval_secs",
            "keep_warm",
            "max_resident_mb",
        ] {
            assert!(
                keys.contains(&expected),
                "suggest.spec_cache.{expected} missing from config editor"
            );
        }
    }

    #[test]
    fn popup_section_exposes_description_box_fields() {
        let fields = all_fields();
        let keys: Vec<&str> = fields
            .iter()
            .filter(|f| f.section == "popup")
            .map(|f| f.key)
            .collect();
        for expected in [
            "min_width",
            "max_width",
            "description_box",
            "description_box_max_width",
            "description_box_lines",
            "description_box_debounce_ms",
        ] {
            assert!(
                keys.contains(&expected),
                "popup.{expected} missing from config editor"
            );
        }
    }

    fn find_field(section: &str, key: &str) -> FieldMeta {
        all_fields()
            .into_iter()
            .find(|f| f.section == section && f.key == key)
            .unwrap_or_else(|| panic!("{section}.{key} should be registered"))
    }

    #[test]
    fn new_fields_registered_with_expected_sections() {
        for (section, key) in [
            ("popup", "render_block_ms"),
            ("suggest.providers", "js_runtime"),
            ("experimental", "aws_sdk_provider"),
            ("experimental", "aws_sdk_fallback_to_cli"),
            ("experimental", "brew_search_cap"),
        ] {
            let field = find_field(section, key);
            assert_eq!(field.section, section);
            assert_eq!(field.key, key);
        }
    }

    /// Pulls a scalar value from a serialized config and renders it the way the
    /// editor would. Used to confirm `FieldMeta.default` strings match the
    /// schema's actual defaults — if `gc-config` changes a default the editor
    /// will fail this test until resynced.
    fn rendered_default(cfg_value: &toml::Value, section: &str, key: &str) -> String {
        let mut current = cfg_value;
        for part in section.split('.') {
            current = current
                .as_table()
                .and_then(|t| t.get(part))
                .unwrap_or_else(|| panic!("section {section} should exist"));
        }
        let v = current
            .as_table()
            .and_then(|t| t.get(key))
            .unwrap_or_else(|| panic!("{section}.{key} should exist"));
        match v {
            toml::Value::Boolean(b) => b.to_string(),
            toml::Value::Integer(i) => i.to_string(),
            toml::Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    }

    #[test]
    fn new_field_defaults_match_schema() {
        let cfg = gc_config::GhostConfig::default();
        let serialized = toml::Value::try_from(&cfg).expect("default config serializes");

        for (section, key) in [
            ("popup", "render_block_ms"),
            ("popup", "feedback_dismiss_ms"),
            ("suggest", "match_mode"),
            ("suggest.providers", "js_runtime"),
            ("experimental", "aws_sdk_provider"),
            ("experimental", "aws_sdk_fallback_to_cli"),
            ("experimental", "brew_search_cap"),
        ] {
            let field = find_field(section, key);
            let rendered = rendered_default(&serialized, section, key);
            assert_eq!(
                rendered, field.default,
                "{section}.{key}: editor default {:?} drifted from schema default {:?}",
                field.default, rendered
            );
        }
    }

    #[test]
    fn new_field_defaults_round_trip_through_toml() {
        // For each new field, build a minimal `[section] key = <default>` doc,
        // deserialize as `GhostConfig`, then reserialize and confirm the value
        // at the same path matches the editor's `default` string.
        for (section, key) in [
            ("popup", "render_block_ms"),
            ("popup", "feedback_dismiss_ms"),
            ("suggest", "match_mode"),
            ("suggest.providers", "js_runtime"),
            ("experimental", "aws_sdk_provider"),
            ("experimental", "aws_sdk_fallback_to_cli"),
            ("experimental", "brew_search_cap"),
        ] {
            let field = find_field(section, key);
            // Numeric / bool defaults are valid bare TOML scalars; string-valued
            // field types (enum variants, plain strings, style strings) must be
            // quoted to form a valid `key = "value"` document.
            let rhs = match field.field_type {
                FieldType::Enum(_) | FieldType::String | FieldType::StyleString => {
                    format!("\"{}\"", field.default)
                }
                _ => field.default.to_string(),
            };
            let doc = format!("[{section}]\n{key} = {rhs}\n");
            let cfg: gc_config::GhostConfig = toml::from_str(&doc)
                .unwrap_or_else(|e| panic!("{section}.{key} parse failed: {e}"));
            let round = toml::Value::try_from(&cfg).expect("config serializes");
            let rendered = rendered_default(&round, section, key);
            assert_eq!(
                rendered, field.default,
                "{section}.{key} did not survive serialize/deserialize round-trip"
            );
        }
    }

    #[test]
    fn match_mode_default_is_a_valid_enum_member() {
        // The editor seeds its selection from the index of `default` within the
        // `Enum` options slice; if the default ever drifts outside that slice the
        // start-index lookup would be wrong, so pin membership here.
        let field = find_field("suggest", "match_mode");
        let options = match field.field_type {
            FieldType::Enum(opts) => opts,
            other => panic!("suggest.match_mode should be an Enum field, got {other:?}"),
        };
        assert!(
            options.contains(&field.default),
            "match_mode default {:?} is not in its enum options {:?}",
            field.default,
            options
        );
    }

    #[test]
    fn feedback_dismiss_ms_is_u16_typed() {
        let field = find_field("popup", "feedback_dismiss_ms");
        assert_eq!(field.field_type, FieldType::U16);
    }

    #[test]
    fn feedback_dismiss_ms_above_u16_max_clamps_silently_in_schema() {
        // Sanity check on the saturating deserializer: values above u16::MAX
        // are silently capped at 65535, then `normalize()` further clamps into
        // operating range [0, 10_000]. The editor's `commit_toml_update` is
        // what surfaces this to the user as a "would be clamped" error.
        let doc = "[popup]\nfeedback_dismiss_ms = 99999\n";
        let cfg: gc_config::GhostConfig = toml::from_str(doc).unwrap();
        assert_eq!(cfg.popup.feedback_dismiss_ms, u16::MAX);

        let mut normalized = cfg;
        normalized.normalize();
        assert!(
            normalized.popup.feedback_dismiss_ms <= 10_000,
            "normalize should clamp feedback_dismiss_ms into operating range"
        );
    }
}

#[cfg(test)]
mod drift_tests {
    use super::all_fields;
    use gc_config::all_field_paths;

    #[test]
    fn tui_editor_metadata_contains_every_schema_field() {
        let editor_keys: Vec<String> = all_fields()
            .iter()
            .map(|f| format!("{}.{}", f.section, f.key))
            .collect();
        let mut missing = Vec::new();
        for path in all_field_paths() {
            if !editor_keys.iter().any(|k| k == path) {
                missing.push(path);
            }
        }
        assert!(
            missing.is_empty(),
            "TUI editor missing keys: {:#?}",
            missing,
        );
    }
}
