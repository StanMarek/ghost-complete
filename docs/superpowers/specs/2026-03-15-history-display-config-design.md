# History Display Configuration

## Problem

History suggestions are shown in the popup on every trigger. While history is already deprioritized (sort priority 8, appended after main results), it can still fill a large number of slots when main suggestions are sparse. Not all users want history cluttering their popup — some find it noisy.

## Decision

Combine a config toggle with a display cap (options B + D from initial discussion):

- A single `max_history_results` config field replaces the existing `providers.history` boolean
- Setting it to `0` disables history entirely (including skipping `$HISTFILE` loading)
- Any positive value caps how many history entries appear in the popup
- Default: `5`

## Design

### Config Schema (`gc-config/src/lib.rs`)

**SuggestConfig:**
- Add `max_history_results: usize` with default `5`
- Keep `max_history_entries: usize` (default `10_000`) — controls how many lines to load from `$HISTFILE` (search pool depth, separate concern from display cap)
- Remove `ProvidersConfig.history: bool` — replaced by `max_history_results > 0`. The `ProvidersConfig` struct and `[suggest.providers]` TOML table remain; only the `history` field is removed.

The two fields serve distinct purposes:
- `max_history_entries` = search depth (how many history lines to load for fuzzy matching)
- `max_history_results` = display cap (how many to show in popup)

**Backwards compatibility:** No `deny_unknown_fields` in serde config. Existing `providers.history` in user configs will be silently ignored.

**Default config.toml template** (in `install.rs`):
```toml
# [suggest]
# max_results = 50
# max_history_results = 5
# max_history_entries = 10000
```

Remove `# history = true` from the `[suggest.providers]` section.

### Engine Changes (`gc-suggest/src/engine.rs`)

**SuggestEngine struct:**
- Replace `providers_history: bool` with `max_history_results: usize`

**`new()` constructor:**
- Sets `max_history_results: 5` (matching the config default), loads history as before

**`with_suggest_config()` constructor:**
- Replace `history: bool` param with `max_history_results: usize`
- Call `HistoryProvider::load()` only when `max_history_results > 0` (skip `$HISTFILE` read entirely when disabled)
- Note: `new()` loads history unconditionally (since its default is 5 > 0). When `with_suggest_config()` is chained with `max_history_results = 0`, it overwrites the provider with an empty one. The redundant load from `new()` is acceptable — this matches the existing builder pattern and the cost is negligible (one-time startup).

**`rank_with_history()` method:**
- Guard: `self.max_history_results > 0 && !ctx.in_redirect` (was `self.providers_history && ...`)
- Cap: `let remaining = self.max_history_results.min(self.max_results.saturating_sub(results.len()));` (was just `self.max_results.saturating_sub(results.len())`)
- History is still fuzzy-ranked against the full buffer, then capped — cap applies after scoring, not before

### Proxy Wiring (`gc-pty/src/proxy.rs`)

- Pass `config.suggest.max_history_results` instead of `config.suggest.providers.history` to engine constructor

### Tests

- Update existing tests referencing `providers_history` bool to use `max_history_results`
- New test: `max_history_results = 3` caps history output to 3 entries when more matches exist
- New test: `max_history_results = 0` produces zero history entries in results

## Approach

Cap at ranking time (Approach 1). The cap is applied in `rank_with_history()` after fuzzy scoring, not before. This ensures the best matches survive — truncating before scoring would lose good matches.

## Files Modified

| File | Change |
|------|--------|
| `crates/gc-config/src/lib.rs` | Add `max_history_results`, remove `ProvidersConfig.history`, update config tests (`test_full_config_parses`, `test_default_config_matches_hardcoded`) |
| `crates/gc-suggest/src/engine.rs` | Replace bool with usize, cap history in `rank_with_history()` |
| `crates/gc-pty/src/proxy.rs` | Update config passthrough |
| `crates/ghost-complete/src/install.rs` | Update default config template |
| `crates/gc-suggest/src/engine.rs` (tests) | Update existing, add cap/disable tests |

## Non-Goals

- No runtime toggle (hot-reload config). If you change the config, restart.
- No separate history-only trigger keybinding (possible future work, not this change).
- No changes to history sort priority (already lowest at 8).
- No changes to fuzzy ranking logic in `fuzzy.rs`.
- No changes to how sync and dynamic (async generator) results are merged in the proxy.
