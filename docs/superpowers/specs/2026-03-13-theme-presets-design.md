# Theme Presets

**Date:** 2026-03-13
**Scope:** gc-config, gc-pty, ghost-complete
**Follows:** Theme expansion (match highlighting, scrollbar, dimmed items, hex colors)

## Summary

Add 4 hardcoded theme presets (dark, light, catppuccin, material-darker) selectable via `theme.preset` in config. Individual field overrides compose on top of the preset. Zero new files, dependencies, or I/O — just a match arm per preset in gc-config.

## Config Surface

New optional field in `ThemeConfig`:

```toml
[theme]
preset = "catppuccin"
# Individual fields override the preset:
# selected = "reverse"
# description = "dim"
# match_highlight = "bold"
# item_text = ""
# scrollbar = "dim"
```

## Resolution Order

1. If `preset` is non-empty, load that preset's 5 field values as base
2. If `preset` is empty, load `"dark"` as base (backwards compatible)
3. For each of the 5 fields: if non-empty, use the explicit value; otherwise use the base from step 1/2

This means:
- Existing configs with no `[theme]` section → dark preset (identical to current behavior)
- Existing configs with explicit fields (e.g. `selected = "bold fg:255"`) → override preserved
- New `preset = "catppuccin"` with no field overrides → full catppuccin theme
- `preset = "catppuccin"` with `match_highlight = "underline"` → catppuccin but with underline highlights

## ThemeConfig Changes

**File:** `crates/gc-config/src/lib.rs`

```rust
pub struct ThemeConfig {
    pub preset: String,          // NEW — default ""
    pub selected: String,        // default "" (was "reverse")
    pub description: String,     // default "" (was "dim")
    pub match_highlight: String, // default "" (was "bold")
    pub item_text: String,       // default "" (already)
    pub scrollbar: String,       // default "" (was "dim")
}
```

All field defaults change to empty string. The actual default values move into the `"dark"` preset.

New method:

```rust
impl ThemeConfig {
    /// Resolve preset base + field overrides into a fully populated ThemeConfig.
    pub fn resolve(&self) -> Result<ThemeConfig> {
        let preset_name = if self.preset.is_empty() { "dark" } else { &self.preset };
        let base = preset_values(preset_name)?;
        Ok(ThemeConfig {
            preset: self.preset.clone(),
            selected: if self.selected.is_empty() { base.selected } else { self.selected.clone() },
            description: if self.description.is_empty() { base.description } else { self.description.clone() },
            match_highlight: if self.match_highlight.is_empty() { base.match_highlight } else { self.match_highlight.clone() },
            item_text: if self.item_text.is_empty() { base.item_text } else { self.item_text.clone() },
            scrollbar: if self.scrollbar.is_empty() { base.scrollbar } else { self.scrollbar.clone() },
        })
    }
}
```

New function:

```rust
fn preset_values(name: &str) -> Result<ThemeConfig> {
    match name {
        "dark" => Ok(ThemeConfig {
            preset: String::new(),
            selected: "reverse".into(),
            description: "dim".into(),
            match_highlight: "bold".into(),
            item_text: String::new(),
            scrollbar: "dim".into(),
        }),
        "light" => Ok(ThemeConfig {
            preset: String::new(),
            selected: "reverse".into(),
            description: "dim".into(),
            match_highlight: "bold".into(),
            item_text: String::new(),
            scrollbar: "dim".into(),
        }),
        "catppuccin" => Ok(ThemeConfig {
            preset: String::new(),
            selected: "fg:#cdd6f4 bg:#585b70 bold".into(),
            description: "fg:#6c7086".into(),
            match_highlight: "fg:#f9e2af bold".into(),
            item_text: String::new(),
            scrollbar: "fg:#585b70".into(),
        }),
        "material-darker" => Ok(ThemeConfig {
            preset: String::new(),
            selected: "fg:#eeffff bg:#424242 bold".into(),
            description: "fg:#616161".into(),
            match_highlight: "fg:#ffcb6b bold".into(),
            item_text: String::new(),
            scrollbar: "fg:#424242".into(),
        }),
        _ => bail!("unknown theme preset: {:?} (valid: dark, light, catppuccin, material-darker)", name),
    }
}
```

## Proxy Integration

**File:** `crates/gc-pty/src/proxy.rs`

Before the `parse_style()` calls, resolve the theme:

```rust
let resolved_theme = config.theme.resolve().context("invalid theme preset")?;
let theme = PopupTheme {
    selected_on: parse_style(&resolved_theme.selected).context("invalid theme.selected style")?,
    description_on: parse_style(&resolved_theme.description).context("invalid theme.description style")?,
    match_highlight_on: parse_style(&resolved_theme.match_highlight).context("invalid theme.match_highlight style")?,
    item_text_on: parse_style(&resolved_theme.item_text).context("invalid theme.item_text style")?,
    scrollbar_on: parse_style(&resolved_theme.scrollbar).context("invalid theme.scrollbar style")?,
};
```

## Doctor Integration

**File:** `crates/ghost-complete/src/doctor.rs`

`check_theme` should validate the resolved theme, not the raw config. Call `config.theme.resolve()` first, report preset errors. Then validate each resolved field through `parse_style()`.

## Default Config Template

**File:** `crates/ghost-complete/src/install.rs`

```toml
[theme]
# preset = "dark"
# selected = "reverse"
# description = "dim"
# match_highlight = "bold"
# item_text = ""
# scrollbar = "dim"
```

## Tests

- `resolve()` with no preset → dark values
- `resolve()` with `preset = "catppuccin"` → catppuccin values
- `resolve()` with preset + field override → override wins
- `resolve()` with invalid preset → error
- `resolve()` with empty field uses preset value, non-empty field overrides
- Existing `test_default_config_matches_hardcoded` updated for empty-string defaults
- All existing theme tests updated

## Files Modified

| File | Changes |
|------|---------|
| `crates/gc-config/src/lib.rs` | `ThemeConfig` preset field, empty defaults, `resolve()`, `preset_values()` |
| `crates/gc-pty/src/proxy.rs` | Call `resolve()` before `parse_style()` |
| `crates/ghost-complete/src/doctor.rs` | Validate resolved theme |
| `crates/ghost-complete/src/install.rs` | Default config template |

## Not In Scope

- File-based custom themes (`themes/` directory)
- Theme preview command
- Per-field "inherit from preset" sentinel other than empty string
