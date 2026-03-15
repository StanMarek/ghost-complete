# History Full-Command Accept Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make history popup entries insert the full command on accept and match against the full buffer at any word position.

**Architecture:** Four-layer change: (1) add `is_first_segment` to `CommandContext`, (2) change history provider to emit full commands gated by segment, (3) add separate history fuzzy pass in engine with buffer param, (4) history-aware accept in handler that deletes the full buffer.

**Tech Stack:** Rust, nucleo (fuzzy matching), gc-buffer/gc-suggest/gc-pty crates

**Spec:** `docs/superpowers/specs/2026-03-15-history-full-command-accept-design.md`

---

## Chunk 1: CommandContext + History Provider

### Task 1: Add `is_first_segment` to `CommandContext`

**Files:**
- Modify: `crates/gc-buffer/src/context.rs:6-27` (struct definition)
- Modify: `crates/gc-buffer/src/context.rs:134-145` (struct construction in `parse_command_context`)
- Test: `crates/gc-buffer/src/context.rs:148-280` (existing tests)

- [ ] **Step 1: Write failing tests for `is_first_segment`**

Add these tests at the end of the `mod tests` block in `crates/gc-buffer/src/context.rs`:

```rust
#[test]
fn test_is_first_segment_simple_command() {
    let ctx = parse_command_context("git", 3);
    assert!(ctx.is_first_segment);
}

#[test]
fn test_is_first_segment_false_after_pipe() {
    let ctx = parse_command_context("cat f | grep ", 13);
    assert!(!ctx.is_first_segment);
}

#[test]
fn test_is_first_segment_false_after_semicolon() {
    let ctx = parse_command_context("cd /tmp; ls ", 12);
    assert!(!ctx.is_first_segment);
}

#[test]
fn test_is_first_segment_false_after_and() {
    let ctx = parse_command_context("make && ./run", 13);
    assert!(!ctx.is_first_segment);
}

#[test]
fn test_is_first_segment_true_empty() {
    let ctx = parse_command_context("", 0);
    assert!(ctx.is_first_segment);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p gc-buffer -- is_first_segment`
Expected: FAIL — `CommandContext` has no field `is_first_segment`

- [ ] **Step 3: Add the field and set it**

In `crates/gc-buffer/src/context.rs`, add to the `CommandContext` struct (after line 26, the `quote_state` field):

```rust
/// True when there are no preceding `|`, `&&`, `||`, or `;` operators.
/// Used to gate history suggestions — full commands only make sense as
/// standalone input, not as pipe/chain segments.
pub is_first_segment: bool,
```

In the `CommandContext` construction at line 134-145, add the field:

```rust
CommandContext {
    command,
    args,
    current_word: current_word.to_string(),
    word_index,
    is_flag,
    is_long_flag,
    preceding_flag,
    in_pipe: found_pipe,
    in_redirect,
    quote_state: result.quote_state,
    is_first_segment: segment_start == 0,
}
```

- [ ] **Step 4: Fix all direct `CommandContext` constructions across the workspace**

Every file that constructs `CommandContext` as a struct literal needs `is_first_segment: true` (or the appropriate test value). Add the field to each:

**`crates/gc-suggest/src/history.rs`** — 2 helpers (lines 128, 143):
```rust
// In cmd_position_ctx — add after quote_state line:
is_first_segment: true,

// In arg_position_ctx — add after quote_state line:
is_first_segment: true,
```

**`crates/gc-suggest/src/commands.rs`** — 2 helpers (lines 106, 121):
```rust
// In cmd_position_ctx — add after quote_state line:
is_first_segment: true,

// In arg_position_ctx — add after quote_state line:
is_first_segment: true,
```

**`crates/gc-suggest/src/filesystem.rs`** — 1 helper (line 142):
```rust
// In ctx_with_word — add after quote_state line:
is_first_segment: true,
```

**`crates/gc-suggest/src/engine.rs`** — `make_ctx` helper (line 439) + 3 inline constructions (lines 567, 595, 651):
```rust
// In make_ctx — add after quote_state line:
is_first_segment: true,

// In each inline CommandContext (pip, curl, test-deploy) — add after quote_state line:
is_first_segment: true,
```

**`crates/gc-suggest/src/specs.rs`** — 12 inline constructions (lines 504, 541, 562, 583, 602, 630, 663, 705, 743, 784, 822, 973):
```rust
// In each CommandContext — add after quote_state line:
is_first_segment: true,
```

**`crates/gc-suggest/benches/suggest_bench.rs`** — `make_ctx` helper (line 34):
```rust
// In make_ctx — add after quote_state line:
is_first_segment: true,
```

- [ ] **Step 5: Run full workspace tests**

Run: `cargo test`
Expected: ALL PASS (332+ tests). The new field is set everywhere, all existing behavior unchanged.

- [ ] **Step 6: Commit**

```bash
git add crates/gc-buffer/src/context.rs crates/gc-suggest/src/history.rs crates/gc-suggest/src/commands.rs crates/gc-suggest/src/filesystem.rs crates/gc-suggest/src/engine.rs crates/gc-suggest/src/specs.rs crates/gc-suggest/benches/suggest_bench.rs
git commit -m "feat: add is_first_segment to CommandContext for history gating"
```

---

### Task 2: Change history provider to emit full commands

**Files:**
- Modify: `crates/gc-suggest/src/history.rs:90-120` (provider impl)
- Test: `crates/gc-suggest/src/history.rs:122-186` (existing tests)

- [ ] **Step 1: Update existing tests for new behavior**

In `crates/gc-suggest/src/history.rs`, update the existing tests and add new ones:

Replace `test_history_only_at_command_position` (line 170-175):

```rust
#[test]
fn test_history_suppressed_in_pipe() {
    let provider = HistoryProvider::from_entries(vec!["git push".into(), "ls -la".into()]);
    let mut ctx = cmd_position_ctx("");
    ctx.in_pipe = true;
    ctx.is_first_segment = false;
    let results = provider.provide(&ctx, Path::new("/tmp")).unwrap();
    assert!(results.is_empty(), "history should be empty in pipe segment");
}
```

Replace `test_history_at_command_position` (line 178-185):

```rust
#[test]
fn test_history_returns_full_commands() {
    let provider = HistoryProvider::from_entries(vec!["git push".into(), "ls -la".into()]);
    let ctx = cmd_position_ctx("gi");
    let results = provider.provide(&ctx, Path::new("/tmp")).unwrap();
    assert_eq!(results.len(), 2);
    // text is now the FULL command, not just the first word
    assert!(results.iter().any(|s| s.text == "git push"));
    assert!(results.iter().any(|s| s.text == "ls -la"));
    // description is None
    assert!(results.iter().all(|s| s.description.is_none()));
}
```

Add a test for history at arg position (new behavior — history works at any word_index in first segment):

```rust
#[test]
fn test_history_available_at_arg_position_in_first_segment() {
    let provider = HistoryProvider::from_entries(vec!["git push origin main".into()]);
    let mut ctx = cmd_position_ctx("");
    ctx.command = Some("git".into());
    ctx.word_index = 1;
    ctx.is_first_segment = true;
    let results = provider.provide(&ctx, Path::new("/tmp")).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].text, "git push origin main");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p gc-suggest -- history`
Expected: FAIL — provider still gates on `word_index != 0` and uses first-word-only text

- [ ] **Step 3: Update the provider implementation**

In `crates/gc-suggest/src/history.rs`, replace the `provide` method (lines 91-115):

```rust
fn provide(&self, ctx: &CommandContext, _cwd: &Path) -> Result<Vec<Suggestion>> {
    // History only makes sense in the first segment — not after |, &&, ||, or ;
    if !ctx.is_first_segment {
        return Ok(Vec::new());
    }

    let suggestions = self
        .entries
        .iter()
        .map(|entry| {
            Suggestion {
                text: entry.clone(),
                description: None,
                kind: SuggestionKind::History,
                source: SuggestionSource::History,
                ..Default::default()
            }
        })
        .collect();

    Ok(suggestions)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p gc-suggest -- history`
Expected: ALL PASS

- [ ] **Step 5: Commit**

```bash
git add crates/gc-suggest/src/history.rs
git commit -m "feat: history provider emits full commands, gated by is_first_segment"
```

---

## Chunk 2: Engine + Handler

### Task 3: Add `buffer` parameter to `suggest_sync` with separate history pass

**Files:**
- Modify: `crates/gc-suggest/src/engine.rs:252-388` (suggest_sync method)
- Modify: `crates/gc-suggest/benches/suggest_bench.rs:186-197` (bench call sites)
- Test: `crates/gc-suggest/src/engine.rs:412-905` (existing tests)

- [ ] **Step 1: Write failing test for buffer-based history matching**

Add this test in the `mod tests` block of `crates/gc-suggest/src/engine.rs`:

```rust
#[test]
fn test_history_matches_full_buffer_at_arg_position() {
    let spec_store = SpecStore::load_from_dir(&spec_dir()).unwrap().store;
    let history = HistoryProvider::from_entries(vec![
        "git push origin main".into(),
        "git checkout -b feature".into(),
    ]);
    let commands = CommandsProvider::from_list(vec!["git".into()]);
    let engine = SuggestionEngine::with_providers(spec_store, history, commands);

    // User typed "git push" — history should match against full buffer
    let ctx = make_ctx(Some("git"), vec!["push"], "", 2);
    let results = engine.suggest_sync(&ctx, Path::new("/tmp"), "git push ").unwrap();
    let hist: Vec<_> = results
        .iter()
        .filter(|s| s.source == crate::types::SuggestionSource::History)
        .collect();
    assert!(
        hist.iter().any(|s| s.text == "git push origin main"),
        "expected full history entry in results: {hist:?}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p gc-suggest -- test_history_matches_full_buffer`
Expected: FAIL — `suggest_sync` doesn't accept a `buffer` parameter

- [ ] **Step 3: Add `buffer` parameter and separate history pass**

In `crates/gc-suggest/src/engine.rs`, change the `suggest_sync` signature (line 252):

```rust
pub fn suggest_sync(&self, ctx: &CommandContext, cwd: &Path, buffer: &str) -> Result<Vec<Suggestion>> {
```

Remove the history block from inside the `word_index == 0` branch (lines 263-268). The commands-only block becomes:

```rust
// Command position: commands only (history handled separately below)
if ctx.word_index == 0 {
    if self.providers_commands {
        match self.commands_provider.provide(ctx, cwd) {
            Ok(cmds) => candidates.extend(cmds),
            Err(e) => tracing::debug!("commands provider error: {e}"),
        }
    }
    return Ok(self.rank_with_history(ctx, cwd, buffer, candidates));
}
```

Replace every `return Ok(fuzzy::rank(&ctx.current_word, candidates, self.max_results));` with `return Ok(self.rank_with_history(ctx, cwd, buffer, candidates));` — there are 5 return sites (lines 269, 280, 364, 377, 387-388). The final non-return at line 387 also changes.

Add a private helper method to `SuggestionEngine`:

```rust
/// Rank main candidates with current_word, then separately rank history
/// candidates with the full buffer, and append history results at the end.
fn rank_with_history(
    &self,
    ctx: &CommandContext,
    cwd: &Path,
    buffer: &str,
    candidates: Vec<Suggestion>,
) -> Vec<Suggestion> {
    let mut results = fuzzy::rank(&ctx.current_word, candidates, self.max_results);

    if self.providers_history {
        match self.history_provider.provide(ctx, cwd) {
            Ok(hist) if !hist.is_empty() => {
                let hist_results = fuzzy::rank(buffer, hist, self.max_results);
                results.extend(hist_results);
            }
            Ok(_) => {}
            Err(e) => tracing::debug!("history provider error: {e}"),
        }
    }

    results
}
```

- [ ] **Step 4: Update all existing `suggest_sync` call sites in engine tests**

Every `engine.suggest_sync(&ctx, ...)` call needs a third `buffer` argument. Update all test calls in `crates/gc-suggest/src/engine.rs`:

For tests using `make_ctx`:
- `test_command_position_returns_commands_and_history` (line 457): `engine.suggest_sync(&ctx, Path::new("/tmp"), "gi")`
- `test_spec_subcommands` (line 466): `engine.suggest_sync(&ctx, Path::new("/tmp"), "git ch")`
- `test_spec_options` (lines 478, 490): `engine.suggest_sync(&ctx, ..., "git commit --")` and `"git commit -"`
- `test_redirect_gives_filesystem` (line 504): `engine.suggest_sync(&ctx, tmp.path(), "echo hello ")`
- `test_path_prefix_triggers_filesystem` (line 515): `engine.suggest_sync(&ctx, tmp.path(), "cat src/")`
- `test_unknown_command_falls_back_to_filesystem` (line 528): `engine.suggest_sync(&ctx, tmp.path(), "unknown_cmd_xyz ")`
- `test_empty_results_for_no_matches` (line 537): `engine.suggest_sync(&ctx, tmp.path(), "git zzzzzzz_no_match")`
- `test_cd_only_shows_directories` (line 548): `engine.suggest_sync(&ctx, tmp.path(), "cd ")`
- `test_option_arg_template_triggers_filesystem` (line 579): `engine.suggest_sync(&ctx, tmp.path(), "pip install -r ")`
- `test_curl_dash_o_shows_files_from_real_spec` (line 607): `engine.suggest_sync(&ctx, tmp.path(), "curl -o ")`
- `test_option_arg_folders_template_filters_files` (line 663): `engine.suggest_sync(&ctx, tmp.path(), "test-deploy install -t ")`
- `test_cd_first_suggestion_is_parent_dir` (line 681): `engine.suggest_sync(&ctx, tmp.path(), "cd ")`
- `test_cd_parent_dir_absent_at_root` (line 694): `engine.suggest_sync(&ctx, Path::new("/"), "cd ")`
- `test_cd_parent_dir_absent_at_home` (line 706): `engine.suggest_sync(&ctx, Path::new(&home), "cd ")`
- `test_cd_chaining_offers_double_parent` (line 721): `engine.suggest_sync(&ctx, &sub, "cd ../")`
- `test_cd_parent_dir_absent_with_query` (line 735): `engine.suggest_sync(&ctx, tmp.path(), "cd my")`
- `test_disabled_commands_provider` (line 751): `engine.suggest_sync(&ctx, Path::new("/tmp"), "gi")`

- [ ] **Step 5: Update benchmark call sites**

In `crates/gc-suggest/benches/suggest_bench.rs`, update the 3 `suggest_sync` calls (lines 187, 192, 197):

```rust
// line 187
b.iter(|| engine.suggest_sync(&cmd_ctx, tmp.path(), "gi").unwrap());

// line 192
b.iter(|| engine.suggest_sync(&sub_ctx, tmp.path(), "git ch").unwrap());

// line 197
b.iter(|| engine.suggest_sync(&fs_ctx, tmp.path(), "unknown_cmd_xyz ").unwrap());
```

- [ ] **Step 6: Run all tests**

Run: `cargo test -p gc-suggest`
Expected: ALL PASS

- [ ] **Step 7: Commit**

```bash
git add crates/gc-suggest/src/engine.rs crates/gc-suggest/benches/suggest_bench.rs
git commit -m "feat: suggest_sync takes buffer param, separate history ranking pass"
```

---

### Task 4: History-aware accept and handler integration

**Files:**
- Modify: `crates/gc-pty/src/handler.rs:538-562` (accept_suggestion method)
- Modify: `crates/gc-pty/src/handler.rs:263-331` (accept_with_chaining method)
- Modify: `crates/gc-pty/src/handler.rs:357-436` (trigger method — pass buffer to suggest_sync)
- Modify: `crates/gc-pty/src/handler.rs:315` (chaining suggest_sync call)

- [ ] **Step 1: Update `accept_suggestion` for history-aware buffer deletion**

In `crates/gc-pty/src/handler.rs`, replace the `accept_suggestion` method (lines 538-562):

```rust
fn accept_suggestion(&self, parser: &Arc<Mutex<TerminalParser>>) -> Vec<u8> {
    let selected_idx = match self.overlay.selected {
        Some(idx) if idx < self.suggestions.len() => idx,
        _ => return Vec::new(),
    };

    let selected = &self.suggestions[selected_idx];

    let (delete_chars, replacement) = {
        let p = parser.lock().unwrap();
        let state = p.state();
        let buffer = state.command_buffer().unwrap_or("");
        let cursor = state.buffer_cursor();

        if selected.kind == gc_suggest::SuggestionKind::History {
            // History: delete the entire buffer up to cursor, then type the full command
            (cursor, selected.text.clone())
        } else {
            // Non-history: delete current_word, type suggestion text
            let ctx = parse_command_context(buffer, cursor);
            (ctx.current_word.chars().count(), selected.text.clone())
        }
    };

    // One 0x7F (backspace) per CHARACTER — the shell deletes by character, not byte
    let mut bytes = vec![0x7F; delete_chars];
    bytes.extend_from_slice(replacement.as_bytes());

    bytes
}
```

- [ ] **Step 2: Update `accept_with_chaining` to skip chaining for history**

In `crates/gc-pty/src/handler.rs`, in the `accept_with_chaining` method, after getting `selected_text` and `is_dir` (around line 277), add a history check:

```rust
let selected_text = self.suggestions[selected_idx].text.clone();
let selected_kind = self.suggestions[selected_idx].kind;
let is_dir = selected_text.ends_with('/');
let forward = self.accept_suggestion(parser);

// History entries never chain — they're full commands, not directory paths
if selected_kind == gc_suggest::SuggestionKind::History {
    self.dismiss(stdout);
    return forward;
}

if is_dir {
```

Also update the `suggest_sync` call inside the chaining block (line 315). The buffer is the `predicted` string:

```rust
match self.engine.suggest_sync(&predicted_ctx, &cwd, &predicted) {
```

Note: the `predicted` variable is constructed at line 293. It's already a `String`, but it's consumed by `predict_command_buffer` at line 310. Clone it before that call:

```rust
let predicted_buffer = predicted.clone();
p.state_mut().predict_command_buffer(predicted, new_cursor);
// ...
match self.engine.suggest_sync(&predicted_ctx, &cwd, &predicted_buffer) {
```

- [ ] **Step 3: Update `trigger` method to pass buffer to `suggest_sync`**

In `crates/gc-pty/src/handler.rs`, in the `trigger` method, the buffer is already extracted at line 366. Update the `suggest_sync` call at line 394:

```rust
match self.engine.suggest_sync(&ctx, &cwd, &buffer) {
```

- [ ] **Step 4: Run all workspace tests**

Run: `cargo test`
Expected: ALL PASS

- [ ] **Step 5: Run clippy**

Run: `cargo clippy --all-targets`
Expected: No warnings

- [ ] **Step 6: Commit**

```bash
git add crates/gc-pty/src/handler.rs
git commit -m "feat: history-aware accept deletes full buffer, handler passes buffer to engine"
```

---

### Task 5: Final verification

- [ ] **Step 1: Run full test suite**

Run: `cargo test`
Expected: ALL PASS

- [ ] **Step 2: Run clippy and fmt**

Run: `cargo clippy --all-targets && cargo fmt --check`
Expected: Clean

- [ ] **Step 3: Build release**

Run: `cargo build --release`
Expected: Compiles successfully

- [ ] **Step 4: Verify benchmarks compile**

Run: `cargo bench -p gc-suggest -- --test`
Expected: Benchmarks compile and run (just the test pass, not full benchmark)
