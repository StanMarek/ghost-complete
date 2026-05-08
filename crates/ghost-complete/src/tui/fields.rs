#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    Bool,
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
            field_type: FieldType::U64,
            default: "1200",
            reload: ReloadBehavior::Live,
            help: "Milliseconds before Empty/Error feedback auto-dismisses",
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
            key: "min_width",
            field_type: FieldType::Usize,
            default: "20",
            reload: ReloadBehavior::Live,
            help: "Minimum popup width in display columns (10-max_width)",
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
            help: "Adjacent description box mode: 'off' = inline truncation only, 'side' = wrapped multi-line box next to the popup",
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
}
