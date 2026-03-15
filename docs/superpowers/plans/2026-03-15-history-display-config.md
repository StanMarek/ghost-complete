# History Display Configuration Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `max_history_results` config field (default 5) that caps history entries in the popup, replacing the `providers.history` boolean toggle.

**Architecture:** Single config field flows from `gc-config` through `gc-pty` proxy to `gc-suggest` engine. The cap is applied in `rank_with_history()` after fuzzy scoring. Setting to 0 skips `$HISTFILE` loading entirely.

**Tech Stack:** Rust, serde/TOML config, existing fuzzy ranking in gc-suggest

**Spec:** `docs/superpowers/specs/2026-03-15-history-display-config-design.md`

---

### Task 1: Add `max_history_results` to config and remove `providers.history`

**Files:**
- Modify: `crates/gc-config/src/lib.rs:85-125` (SuggestConfig + ProvidersConfig)
- Modify: `crates/gc-config/src/lib.rs:246-385` (tests)

- [ ] **Step 1: Write failing test — `max_history_results` default is 5**

In `test_default_config_matches_hardcoded` (line 252), add:

```rust
assert_eq!(config.suggest.max_history_results, 5);
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p gc-config -- test_default_config_matches_hardcoded`
Expected: FAIL — `max_history_results` field does not exist on `SuggestConfig`

- [ ] **Step 3: Add `max_history_results` field to `SuggestConfig`, remove `history` from `ProvidersConfig`**

In `SuggestConfig` (line 87), add field after `max_history_entries`:

```rust
pub struct SuggestConfig {
    pub max_results: usize,
    pub max_history_results: usize,
    pub max_history_entries: usize,
    pub generator_timeout_ms: u64,
    pub providers: ProvidersConfig,
}
```

In `SuggestConfig::default()` (line 94), add default:

```rust
impl Default for SuggestConfig {
    fn default() -> Self {
        Self {
            max_results: 50,
            max_history_results: 5,
            max_history_entries: 10_000,
            generator_timeout_ms: 5000,
            providers: ProvidersConfig::default(),
        }
    }
}
```

In `ProvidersConfig` (line 107), remove `history` field:

```rust
pub struct ProvidersConfig {
    pub commands: bool,
    pub filesystem: bool,
    pub specs: bool,
    pub git: bool,
}
```

In `ProvidersConfig::default()` (line 116), remove `history`:

```rust
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
```

- [ ] **Step 4: Update remaining config tests**

In `test_default_config_matches_hardcoded` (line 263):
- Remove: `assert!(config.suggest.providers.history);` (the new assertion was added in Step 1)

In `test_full_config_parses` (line 329):
- Add to TOML string under `[suggest]`: `max_history_results = 3`
- Remove from TOML string under `[suggest.providers]`: `history = false`
- Add assertion: `assert_eq!(config.suggest.max_history_results, 3);`
- Remove assertion: `assert!(!config.suggest.providers.history);` (line 377)

- [ ] **Step 5: Write test — existing TOML with `providers.history` still parses (backwards compat)**

```rust
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
```

- [ ] **Step 6: Run all config tests**

Run: `cargo test -p gc-config`
Expected: ALL PASS

- [ ] **Step 7: Commit**

```bash
git add crates/gc-config/src/lib.rs
git commit -m "feat: add max_history_results config, remove providers.history"
```

---

### Task 2: Update engine to use `max_history_results` cap

**Files:**
- Modify: `crates/gc-suggest/src/engine.rs:24-104` (struct + constructors)
- Modify: `crates/gc-suggest/src/engine.rs:389-416` (rank_with_history)
- Modify: `crates/gc-suggest/src/engine.rs:440-977` (tests)

- [ ] **Step 1: Write failing test — history capped to N entries**

Add test after existing tests (~line 977):

```rust
#[test]
fn test_history_capped_to_max_history_results() {
    let spec_store = SpecStore::load_from_dir(&spec_dir()).unwrap().store;
    let history = HistoryProvider::from_entries(vec![
        "git push origin main".into(),
        "git pull origin main".into(),
        "git fetch --all".into(),
        "git status".into(),
        "git log --oneline".into(),
    ]);
    let commands = CommandsProvider::from_list(vec!["git".into()]);
    let engine = SuggestionEngine::with_providers(spec_store, history, commands)
        .with_suggest_config(50, 10_000, true, 3, true, true, true);

    let ctx = make_ctx(None, vec![], "git", 0);
    let results = engine
        .suggest_sync(&ctx, Path::new("/tmp"), "git")
        .unwrap();
    let hist_count = results
        .iter()
        .filter(|s| s.source == crate::types::SuggestionSource::History)
        .count();
    assert!(
        hist_count <= 3,
        "history should be capped at 3, got {hist_count}"
    );
}
```

- [ ] **Step 2: Write failing test — `max_history_results = 0` produces no history**

```rust
#[test]
fn test_history_disabled_when_max_zero() {
    let spec_store = SpecStore::load_from_dir(&spec_dir()).unwrap().store;
    let history = HistoryProvider::from_entries(vec![
        "git push origin main".into(),
        "cargo build".into(),
    ]);
    let commands = CommandsProvider::from_list(vec!["git".into(), "cargo".into()]);
    let engine = SuggestionEngine::with_providers(spec_store, history, commands)
        .with_suggest_config(50, 10_000, true, 0, true, true, true);

    let ctx = make_ctx(None, vec![], "git", 0);
    let results = engine
        .suggest_sync(&ctx, Path::new("/tmp"), "git")
        .unwrap();
    let hist_count = results
        .iter()
        .filter(|s| s.source == crate::types::SuggestionSource::History)
        .count();
    assert_eq!(hist_count, 0, "history should be disabled when max is 0");
}
```

- [ ] **Step 3: Run new tests to verify they fail**

Run: `cargo test -p gc-suggest -- test_history_capped test_history_disabled`
Expected: FAIL — `with_suggest_config` signature mismatch (bool vs usize)

- [ ] **Step 4: Update `SuggestionEngine` struct and constructors**

Replace `providers_history: bool` with `max_history_results: usize` in the struct (line 32):

```rust
pub struct SuggestionEngine {
    spec_store: SpecStore,
    filesystem_provider: FilesystemProvider,
    history_provider: HistoryProvider,
    commands_provider: CommandsProvider,
    generator_cache: Arc<GeneratorCache>,
    max_results: usize,
    max_history_results: usize,
    providers_commands: bool,
    providers_filesystem: bool,
    providers_specs: bool,
    providers_git: bool,
}
```

Update `new()` (line 48):

```rust
// Change: providers_history: true,
max_history_results: 5,
```

Update `with_suggest_config()` — replace `history: bool` param with `max_history_results: usize` (line 64):

```rust
#[allow(clippy::too_many_arguments)]
pub fn with_suggest_config(
    mut self,
    max_results: usize,
    max_history_entries: usize,
    commands: bool,
    max_history_results: usize,
    filesystem: bool,
    specs: bool,
    git: bool,
) -> Self {
    self.max_results = max_results;
    self.max_history_results = max_history_results;
    self.providers_commands = commands;
    self.providers_filesystem = filesystem;
    self.providers_specs = specs;
    self.providers_git = git;
    // Reload history only if enabled
    if max_history_results > 0 {
        self.history_provider = HistoryProvider::load(max_history_entries);
    } else {
        self.history_provider = HistoryProvider::from_entries(vec![]);
    }
    self
}
```

Update `with_providers()` (line 91):

```rust
// Change: providers_history: true,
max_history_results: 5,
```

- [ ] **Step 5: Update `rank_with_history()` to use the cap**

Replace lines 401-402 in `rank_with_history()`:

```rust
// Old:
if self.providers_history && !ctx.in_redirect {
    let remaining = self.max_results.saturating_sub(results.len());

// New:
if self.max_history_results > 0 && !ctx.in_redirect {
    let remaining = self.max_history_results.min(self.max_results.saturating_sub(results.len()));
```

- [ ] **Step 6: Update existing test `test_disabled_commands_provider` call site**

Line 796 — the `with_suggest_config` call needs the new param type:

```rust
// Old:
.with_suggest_config(50, 10_000, false, true, true, true, true);
// New:
.with_suggest_config(50, 10_000, false, 5, true, true, true);
```

- [ ] **Step 7: Run all suggest tests**

Run: `cargo test -p gc-suggest`
Expected: ALL PASS

- [ ] **Step 8: Commit**

```bash
git add crates/gc-suggest/src/engine.rs
git commit -m "feat: cap history results with max_history_results in engine"
```

---

### Task 3: Update handler and proxy wiring

**Files:**
- Modify: `crates/gc-pty/src/handler.rs:145-172` (InputHandler::with_suggest_config passthrough)
- Modify: `crates/gc-pty/src/proxy.rs:105-113` (config → handler call site)

- [ ] **Step 1: Update handler passthrough signature**

In `crates/gc-pty/src/handler.rs` (line 146), change `history: bool` to `max_history_results: usize`:

```rust
#[allow(clippy::too_many_arguments)]
pub fn with_suggest_config(
    self,
    max_results: usize,
    max_history_entries: usize,
    commands: bool,
    max_history_results: usize,
    filesystem: bool,
    specs: bool,
    git: bool,
) -> Self {
    let engine = Arc::try_unwrap(self.engine)
        .unwrap_or_else(|_| panic!("with_suggest_config called after engine was shared"))
        .with_suggest_config(
            max_results,
            max_history_entries,
            commands,
            max_history_results,
            filesystem,
            specs,
            git,
        );
    Self {
        engine: Arc::new(engine),
        ..self
    }
}
```

- [ ] **Step 2: Update proxy config passthrough**

In `crates/gc-pty/src/proxy.rs` (line 109), replace `config.suggest.providers.history` with `config.suggest.max_history_results`:

```rust
.with_suggest_config(
    config.suggest.max_results,
    config.suggest.max_history_entries,
    config.suggest.providers.commands,
    config.suggest.max_history_results,
    config.suggest.providers.filesystem,
    config.suggest.providers.specs,
    config.suggest.providers.git,
)
```

- [ ] **Step 3: Run full workspace build**

Run: `cargo build`
Expected: SUCCESS — no compilation errors

- [ ] **Step 4: Run full workspace tests**

Run: `cargo test`
Expected: ALL PASS

- [ ] **Step 5: Commit**

```bash
git add crates/gc-pty/src/handler.rs crates/gc-pty/src/proxy.rs
git commit -m "feat: wire max_history_results through handler and proxy"
```

---

### Task 4: Update default config template

**Files:**
- Modify: `crates/ghost-complete/src/install.rs:1184-1193`

- [ ] **Step 1: Update template**

Change the `[suggest]` section (around line 1184):

```toml
# [suggest]
# max_results = 50
# max_history_results = 5
# max_history_entries = 10000
```

Remove the `# history = true` line from the `[suggest.providers]` section (line 1190):

```toml
# [suggest.providers]
# commands = true
# filesystem = true
# specs = true
# git = true
```

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --all-targets`
Expected: No warnings

- [ ] **Step 3: Run fmt check**

Run: `cargo fmt --check`
Expected: No formatting issues

- [ ] **Step 4: Commit**

```bash
git add crates/ghost-complete/src/install.rs
git commit -m "feat: update default config template with max_history_results"
```

---

### Task 5: Final verification

- [ ] **Step 1: Run full test suite**

Run: `cargo test`
Expected: ALL PASS (332+ tests)

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --all-targets`
Expected: Clean

- [ ] **Step 3: Run fmt**

Run: `cargo fmt --check`
Expected: Clean

- [ ] **Step 4: Build release binary**

Run: `cargo build --release`
Expected: SUCCESS
