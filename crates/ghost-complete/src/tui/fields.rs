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
    "keybindings",
    "theme",
    "paths",
    "experimental",
];

pub fn section_label(section: &str) -> &'static str {
    match section {
        "trigger" => "Trigger",
        "popup" => "Popup",
        "suggest" => "Suggest",
        "suggest.providers" => "Providers",
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
