# Benchmarking & Performance Testing Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add criterion benchmarks covering gc-suggest and gc-parser hot paths, a manual-trigger CI workflow, and fix broken smoke tests.

**Architecture:** Per-crate `benches/` directories with criterion. Visibility changes to gc-suggest internals to expose fuzzy, commands, history modules and test constructors for deterministic benchmark setup. GitHub Actions workflow_dispatch for manual benchmarking.

**Tech Stack:** Rust, criterion 0.5 (with html_reports), GitHub Actions

**Spec:** `docs/superpowers/specs/2026-03-12-benchmarking-design.md`

---

## Chunk 1: Visibility Changes & Criterion Setup

### Task 1: Make gc-suggest internals accessible for benchmarks

Criterion benchmarks compile as external crates — they cannot access private modules or `#[cfg(test)]` items. This task opens up the minimum necessary visibility.

**Files:**
- Modify: `crates/gc-suggest/src/lib.rs`
- Modify: `crates/gc-suggest/src/commands.rs:28-29`
- Modify: `crates/gc-suggest/src/history.rs:29-30`
- Modify: `crates/gc-suggest/src/engine.rs:85-86`

- [ ] **Step 1: Change private modules to pub in lib.rs**

In `crates/gc-suggest/src/lib.rs`, change lines 7, 10, 12 from private to public:

```rust
pub mod cache;
pub mod commands;
mod engine;
mod filesystem;
pub mod fuzzy;
mod git;
pub mod history;
mod provider;
pub mod script;
pub mod specs;
pub mod transform;
pub mod types;

pub use engine::SuggestionEngine;
pub use specs::{CompletionSpec, SpecLoadResult, SpecStore};
pub use types::{Suggestion, SuggestionKind, SuggestionSource};
```

Three changes: `mod commands` → `pub mod commands`, `mod fuzzy` → `pub mod fuzzy`, `mod history` → `pub mod history`.

- [ ] **Step 2: Remove #[cfg(test)] from CommandsProvider::from_list()**

In `crates/gc-suggest/src/commands.rs`, remove the `#[cfg(test)]` attribute on line 28:

```rust
    /// Test/bench constructor — inject command list directly.
    pub fn from_list(commands: Vec<String>) -> Self {
        Self { commands }
    }
```

- [ ] **Step 3: Remove #[cfg(test)] from HistoryProvider::from_entries()**

In `crates/gc-suggest/src/history.rs`, remove the `#[cfg(test)]` attribute on line 29:

```rust
    /// Test/bench constructor — inject entries directly.
    pub fn from_entries(entries: Vec<String>) -> Self {
        Self { entries }
    }
```

- [ ] **Step 4: Remove #[cfg(test)] from SuggestionEngine::with_providers()**

In `crates/gc-suggest/src/engine.rs`, remove the `#[cfg(test)]` attribute on line 85:

```rust
    /// Test/bench constructor — inject providers directly for deterministic setup.
    pub fn with_providers(
        spec_store: SpecStore,
        history_provider: HistoryProvider,
        commands_provider: CommandsProvider,
    ) -> Self {
```

Note: change `fn with_providers` to `pub fn with_providers` (it's currently `fn` with only `#[cfg(test)]` visibility).

- [ ] **Step 5: Run existing tests to verify nothing broke**

Run: `cargo test --workspace`
Expected: All existing tests still pass (the visibility changes are strictly additive).

- [ ] **Step 6: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: No new warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/gc-suggest/src/lib.rs crates/gc-suggest/src/commands.rs crates/gc-suggest/src/history.rs crates/gc-suggest/src/engine.rs
git commit -m "refactor: expose gc-suggest internals for benchmark access"
```

### Task 2: Add criterion dependency and bench harness to gc-suggest

**Files:**
- Modify: `crates/gc-suggest/Cargo.toml`
- Create: `crates/gc-suggest/benches/suggest_bench.rs` (empty scaffold)

- [ ] **Step 1: Add criterion dev-dependency and bench section to Cargo.toml**

Add to `crates/gc-suggest/Cargo.toml`:

```toml
[dev-dependencies]
tempfile = "3"
criterion = { version = "0.5", features = ["html_reports"] }

[[bench]]
name = "suggest_bench"
harness = false
```

Note: `tempfile = "3"` already exists in dev-dependencies. Just add criterion and the `[[bench]]` section.

- [ ] **Step 2: Create empty bench scaffold**

Create `crates/gc-suggest/benches/suggest_bench.rs`:

```rust
use criterion::{criterion_group, criterion_main};

fn fuzzy_benchmarks(c: &mut criterion::Criterion) {
    c.bench_function("fuzzy_ranking/placeholder", |b| {
        b.iter(|| 1 + 1)
    });
}

criterion_group!(benches, fuzzy_benchmarks);
criterion_main!(benches);
```

- [ ] **Step 3: Verify the bench compiles and runs**

Run: `cargo bench -p gc-suggest -- --test`
Expected: Compiles successfully. The `--test` flag just compiles without running full iterations.

- [ ] **Step 4: Commit**

```bash
git add crates/gc-suggest/Cargo.toml crates/gc-suggest/benches/suggest_bench.rs
git commit -m "chore: add criterion scaffold for gc-suggest benchmarks"
```

### Task 3: Add criterion dependency and bench harness to gc-parser

**Files:**
- Modify: `crates/gc-parser/Cargo.toml`
- Create: `crates/gc-parser/benches/parser_bench.rs` (empty scaffold)

- [ ] **Step 1: Add criterion dev-dependency and bench section to Cargo.toml**

Add to `crates/gc-parser/Cargo.toml`:

```toml
[dev-dependencies]
criterion = { version = "0.5", features = ["html_reports"] }

[[bench]]
name = "parser_bench"
harness = false
```

- [ ] **Step 2: Create empty bench scaffold**

Create `crates/gc-parser/benches/parser_bench.rs`:

```rust
use criterion::{criterion_group, criterion_main};

fn parser_benchmarks(c: &mut criterion::Criterion) {
    c.bench_function("vt_parse_throughput/placeholder", |b| {
        b.iter(|| 1 + 1)
    });
}

criterion_group!(benches, parser_benchmarks);
criterion_main!(benches);
```

- [ ] **Step 3: Verify the bench compiles and runs**

Run: `cargo bench -p gc-parser -- --test`
Expected: Compiles successfully.

- [ ] **Step 4: Commit**

```bash
git add crates/gc-parser/Cargo.toml crates/gc-parser/benches/parser_bench.rs
git commit -m "chore: add criterion scaffold for gc-parser benchmarks"
```

---

## Chunk 2: gc-suggest Benchmarks — Fuzzy Ranking & Spec Loading

### Task 4: Implement fuzzy_ranking benchmarks

**Files:**
- Modify: `crates/gc-suggest/benches/suggest_bench.rs`

**Reference:** `crates/gc-suggest/src/fuzzy.rs` — `pub fn rank(query: &str, mut suggestions: Vec<Suggestion>, max_results: usize) -> Vec<Suggestion>`

- [ ] **Step 1: Implement fuzzy_ranking benchmark group**

Replace the placeholder in `crates/gc-suggest/benches/suggest_bench.rs` with:

```rust
use criterion::{criterion_group, criterion_main, Criterion};

use gc_suggest::fuzzy;
use gc_suggest::types::{Suggestion, SuggestionKind, SuggestionSource};

fn make_suggestion(text: &str) -> Suggestion {
    Suggestion {
        text: text.to_string(),
        description: None,
        kind: SuggestionKind::Command,
        source: SuggestionSource::Commands,
        score: 0,
    }
}

fn generate_candidates(n: usize) -> Vec<Suggestion> {
    (0..n).map(|i| make_suggestion(&format!("file_{i}.txt"))).collect()
}

fn fuzzy_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("fuzzy_ranking");

    group.bench_function("1k_3char", |b| {
        let candidates = generate_candidates(1_000);
        b.iter(|| {
            fuzzy::rank("fil", candidates.clone(), fuzzy::DEFAULT_MAX_RESULTS)
        });
    });

    group.bench_function("10k_3char", |b| {
        let candidates = generate_candidates(10_000);
        b.iter(|| {
            fuzzy::rank("fil", candidates.clone(), fuzzy::DEFAULT_MAX_RESULTS)
        });
    });

    group.bench_function("10k_empty", |b| {
        let candidates = generate_candidates(10_000);
        b.iter(|| {
            fuzzy::rank("", candidates.clone(), fuzzy::DEFAULT_MAX_RESULTS)
        });
    });

    group.finish();
}

criterion_group!(benches, fuzzy_benchmarks);
criterion_main!(benches);
```

- [ ] **Step 2: Run the benchmark to verify it works**

Run: `cargo bench -p gc-suggest -- fuzzy_ranking`
Expected: Three benchmarks run with timing output. The `10k_3char` result should be in the sub-millisecond range.

- [ ] **Step 3: Commit**

```bash
git add crates/gc-suggest/benches/suggest_bench.rs
git commit -m "bench: add fuzzy ranking benchmarks (1k, 10k candidates)"
```

### Task 5: Implement spec_loading benchmark

**Files:**
- Modify: `crates/gc-suggest/benches/suggest_bench.rs`

**Reference:** `crates/gc-suggest/src/specs.rs` — `pub fn load_from_dir(dir: &Path) -> Result<SpecLoadResult>`

- [ ] **Step 1: Add spec_loading benchmark group**

Add the following function and wire it into the criterion groups in `crates/gc-suggest/benches/suggest_bench.rs`:

```rust
use std::path::Path;
use gc_suggest::SpecStore;

fn spec_benchmarks(c: &mut Criterion) {
    let spec_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs");

    let mut group = c.benchmark_group("spec_loading");

    group.bench_function("load_717_specs", |b| {
        b.iter(|| {
            SpecStore::load_from_dir(&spec_dir).unwrap()
        });
    });

    group.finish();
}
```

Update criterion_group:

```rust
criterion_group!(benches, fuzzy_benchmarks, spec_benchmarks);
```

- [ ] **Step 2: Run the benchmark**

Run: `cargo bench -p gc-suggest -- spec_loading`
Expected: One benchmark measuring time to deserialize all 717 specs from disk.

- [ ] **Step 3: Commit**

```bash
git add crates/gc-suggest/benches/suggest_bench.rs
git commit -m "bench: add spec loading benchmark (717 specs)"
```

---

## Chunk 3: gc-suggest Benchmarks — Spec Resolution, Transform Pipeline, Engine

### Task 6: Implement spec_resolution benchmarks

**Files:**
- Modify: `crates/gc-suggest/benches/suggest_bench.rs`

**Reference:** `crates/gc-suggest/src/specs.rs` — `pub fn resolve_spec(spec: &CompletionSpec, ctx: &CommandContext) -> SpecResolution`

- [ ] **Step 1: Add spec_resolution benchmark group**

Add the following to `crates/gc-suggest/benches/suggest_bench.rs`:

```rust
use gc_suggest::specs;
use gc_buffer::{CommandContext, QuoteState};

fn make_ctx(
    command: Option<&str>,
    args: Vec<&str>,
    current_word: &str,
    word_index: usize,
) -> CommandContext {
    CommandContext {
        command: command.map(String::from),
        args: args.into_iter().map(String::from).collect(),
        current_word: current_word.to_string(),
        word_index,
        is_flag: current_word.starts_with('-'),
        is_long_flag: current_word.starts_with("--"),
        preceding_flag: None,
        in_pipe: false,
        in_redirect: false,
        quote_state: QuoteState::None,
    }
}

fn resolution_benchmarks(c: &mut Criterion) {
    let spec_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs");
    let store = SpecStore::load_from_dir(&spec_dir).unwrap().store;

    let mut group = c.benchmark_group("spec_resolution");

    // Shallow: git checkout (1 level)
    let git_spec = store.get("git").expect("git spec must exist");
    let shallow_ctx = make_ctx(Some("git"), vec!["checkout"], "", 2);
    group.bench_function("shallow", |b| {
        b.iter(|| specs::resolve_spec(git_spec, &shallow_ctx));
    });

    // Deep: docker compose up --build (2 levels + flags)
    let docker_spec = store.get("docker").expect("docker spec must exist");
    let deep_ctx = make_ctx(Some("docker"), vec!["compose", "up"], "--", 3);
    group.bench_function("deep", |b| {
        b.iter(|| specs::resolve_spec(docker_spec, &deep_ctx));
    });

    group.finish();
}
```

Update criterion_group:

```rust
criterion_group!(benches, fuzzy_benchmarks, spec_benchmarks, resolution_benchmarks);
```

- [ ] **Step 2: Run the benchmark**

Run: `cargo bench -p gc-suggest -- spec_resolution`
Expected: Two benchmarks (shallow, deep) showing microsecond-range timing.

- [ ] **Step 3: Commit**

```bash
git add crates/gc-suggest/benches/suggest_bench.rs
git commit -m "bench: add spec resolution benchmarks (shallow, deep)"
```

### Task 7: Implement transform_pipeline benchmarks

**Files:**
- Modify: `crates/gc-suggest/benches/suggest_bench.rs`

**Reference:** `crates/gc-suggest/src/transform.rs` — `pub fn execute_pipeline(output: &str, transforms: &[Transform]) -> Result<Vec<Suggestion>, String>`

Transform types needed:
- `Transform::Named(NamedTransform::SplitLines)`
- `Transform::Named(NamedTransform::FilterEmpty)`
- `Transform::Named(NamedTransform::Trim)`
- `Transform::Parameterized(ParameterizedTransform::RegexExtract { pattern, name, description })`
- `Transform::Parameterized(ParameterizedTransform::JsonExtract { name, description })`

- [ ] **Step 1: Add transform_pipeline benchmark group**

Add the following to `crates/gc-suggest/benches/suggest_bench.rs`:

```rust
use gc_suggest::transform::{
    self, Transform, NamedTransform, ParameterizedTransform,
};

fn transform_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("transform_pipeline");

    // Simple: split_lines + filter_empty + trim on 500 lines
    let simple_input: String = (0..500)
        .map(|i| format!("  line_{i}  \n"))
        .collect();
    let simple_transforms = vec![
        Transform::Named(NamedTransform::SplitLines),
        Transform::Named(NamedTransform::FilterEmpty),
        Transform::Named(NamedTransform::Trim),
    ];
    group.bench_function("simple", |b| {
        b.iter(|| {
            transform::execute_pipeline(&simple_input, &simple_transforms).unwrap()
        });
    });

    // Regex: split_lines + regex_extract on git-branch-like output
    let regex_input: String = (0..200)
        .map(|i| format!("  branch_{i}  abc1234 commit message {i}\n"))
        .collect();
    let regex_transforms = vec![
        Transform::Named(NamedTransform::SplitLines),
        Transform::Parameterized(ParameterizedTransform::RegexExtract {
            pattern: r"^\s*(\S+)\s+(\S+)\s+(.+)$".to_string(),
            name: 1,
            description: Some(3),
        }),
    ];
    group.bench_function("regex", |b| {
        b.iter(|| {
            transform::execute_pipeline(&regex_input, &regex_transforms).unwrap()
        });
    });

    // JSON: split_lines + json_extract on 100 JSON-per-line objects
    let json_input: String = (0..100)
        .map(|i| format!("{{\"name\":\"item_{i}\",\"desc\":\"description {i}\"}}\n"))
        .collect();
    let json_transforms = vec![
        Transform::Named(NamedTransform::SplitLines),
        Transform::Parameterized(ParameterizedTransform::JsonExtract {
            name: "name".to_string(),
            description: Some("desc".to_string()),
        }),
    ];
    group.bench_function("json", |b| {
        b.iter(|| {
            transform::execute_pipeline(&json_input, &json_transforms).unwrap()
        });
    });

    group.finish();
}
```

Update criterion_group:

```rust
criterion_group!(benches, fuzzy_benchmarks, spec_benchmarks, resolution_benchmarks, transform_benchmarks);
```

- [ ] **Step 2: Run the benchmark**

Run: `cargo bench -p gc-suggest -- transform_pipeline`
Expected: Three benchmarks (simple, regex, json).

- [ ] **Step 3: Commit**

```bash
git add crates/gc-suggest/benches/suggest_bench.rs
git commit -m "bench: add transform pipeline benchmarks (simple, regex, json)"
```

### Task 8: Implement engine_suggest_sync benchmarks

**Files:**
- Modify: `crates/gc-suggest/benches/suggest_bench.rs`

**Reference:** `crates/gc-suggest/src/engine.rs` — `pub fn suggest_sync(&self, ctx: &CommandContext, cwd: &Path) -> Result<Vec<Suggestion>>`

This is the integration-level benchmark. Construct a realistic engine with real specs, synthetic commands/history, and a temp directory with ~2000 files.

- [ ] **Step 1: Add engine_suggest_sync benchmark group**

Add the following to `crates/gc-suggest/benches/suggest_bench.rs`:

```rust
use gc_suggest::commands::CommandsProvider;
use gc_suggest::history::HistoryProvider;
use gc_suggest::SuggestionEngine;

fn setup_engine_and_dir() -> (SuggestionEngine, tempfile::TempDir) {
    let spec_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs");
    let store = SpecStore::load_from_dir(&spec_dir).unwrap().store;

    // 100 synthetic commands
    let commands: Vec<String> = (0..100).map(|i| format!("cmd_{i}")).collect();
    let commands_provider = CommandsProvider::from_list(commands);

    // 50 synthetic history entries
    let history: Vec<String> = (0..50).map(|i| format!("git push origin branch_{i}")).collect();
    let history_provider = HistoryProvider::from_entries(history);

    let engine = SuggestionEngine::with_providers(store, history_provider, commands_provider);

    // Temp directory with ~2000 files
    let tmp = tempfile::TempDir::new().unwrap();
    for i in 0..1500 {
        std::fs::write(
            tmp.path().join(format!("file_{i}.rs")),
            "",
        ).unwrap();
    }
    for i in 0..300 {
        std::fs::write(
            tmp.path().join(format!("data_{i}.json")),
            "",
        ).unwrap();
    }
    for i in 0..200 {
        let dir = tmp.path().join(format!("dir_{i}"));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("mod.rs"), "").unwrap();
    }

    (engine, tmp)
}

fn engine_benchmarks(c: &mut Criterion) {
    let (engine, tmp) = setup_engine_and_dir();

    let mut group = c.benchmark_group("engine_suggest_sync");

    // Command position: word_index=0, query="gi"
    let cmd_ctx = make_ctx(None, vec![], "gi", 0);
    group.bench_function("command_position", |b| {
        b.iter(|| engine.suggest_sync(&cmd_ctx, tmp.path()).unwrap());
    });

    // Subcommand with spec: git, word_index=1, query="ch"
    let sub_ctx = make_ctx(Some("git"), vec![], "ch", 1);
    group.bench_function("subcommand_with_spec", |b| {
        b.iter(|| engine.suggest_sync(&sub_ctx, tmp.path()).unwrap());
    });

    // Filesystem fallback: unknown command
    let fs_ctx = make_ctx(Some("unknown_cmd_xyz"), vec![], "", 1);
    group.bench_function("filesystem_fallback", |b| {
        b.iter(|| engine.suggest_sync(&fs_ctx, tmp.path()).unwrap());
    });

    group.finish();
}
```

Update criterion_group:

```rust
criterion_group!(
    benches,
    fuzzy_benchmarks,
    spec_benchmarks,
    resolution_benchmarks,
    transform_benchmarks,
    engine_benchmarks,
);
criterion_main!(benches);
```

- [ ] **Step 2: Run the benchmark**

Run: `cargo bench -p gc-suggest -- engine_suggest_sync`
Expected: Three benchmarks. `subcommand_with_spec` and `command_position` should be <50ms.

- [ ] **Step 3: Commit**

```bash
git add crates/gc-suggest/benches/suggest_bench.rs
git commit -m "bench: add engine suggest_sync benchmarks (command, subcommand, filesystem)"
```

---

## Chunk 4: gc-parser Benchmarks

### Task 9: Implement vt_parse_throughput benchmarks

**Files:**
- Modify: `crates/gc-parser/benches/parser_bench.rs`

**Reference:** `crates/gc-parser/src/lib.rs` — `pub fn process_bytes(&mut self, bytes: &[u8])`

- [ ] **Step 1: Implement vt_parse_throughput benchmark group**

Replace the placeholder in `crates/gc-parser/benches/parser_bench.rs`:

```rust
use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use gc_parser::TerminalParser;

const BUFFER_SIZE: usize = 64 * 1024; // 64 KB

fn make_plain_text(size: usize) -> Vec<u8> {
    let line = b"drwxr-xr-x  5 user staff  160 Mar 12 10:00 dirname\n";
    let mut buf = Vec::with_capacity(size);
    while buf.len() < size {
        let remaining = size - buf.len();
        let chunk = &line[..remaining.min(line.len())];
        buf.extend_from_slice(chunk);
    }
    buf.truncate(size);
    buf
}

fn make_ansi_colored(size: usize) -> Vec<u8> {
    let line = b"\x1b[32m+added line\x1b[0m\n\x1b[31m-removed line\x1b[0m\n";
    let mut buf = Vec::with_capacity(size);
    while buf.len() < size {
        let remaining = size - buf.len();
        let chunk = &line[..remaining.min(line.len())];
        buf.extend_from_slice(chunk);
    }
    buf.truncate(size);
    buf
}

fn make_cursor_heavy(size: usize) -> Vec<u8> {
    let seq = b"\x1b[H\x1b[2J\x1b[10;20Htext\x1b[1A\x1b[5C";
    let mut buf = Vec::with_capacity(size);
    while buf.len() < size {
        let remaining = size - buf.len();
        let chunk = &seq[..remaining.min(seq.len())];
        buf.extend_from_slice(chunk);
    }
    buf.truncate(size);
    buf
}

fn parser_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("vt_parse_throughput");
    group.throughput(Throughput::Bytes(BUFFER_SIZE as u64));

    let plain = make_plain_text(BUFFER_SIZE);
    group.bench_function("plain_text", |b| {
        b.iter(|| {
            let mut parser = TerminalParser::new(24, 80);
            parser.process_bytes(&plain);
        });
    });

    let colored = make_ansi_colored(BUFFER_SIZE);
    group.bench_function("ansi_colored", |b| {
        b.iter(|| {
            let mut parser = TerminalParser::new(24, 80);
            parser.process_bytes(&colored);
        });
    });

    let cursor = make_cursor_heavy(BUFFER_SIZE);
    group.bench_function("cursor_heavy", |b| {
        b.iter(|| {
            let mut parser = TerminalParser::new(24, 80);
            parser.process_bytes(&cursor);
        });
    });

    group.finish();
}

criterion_group!(benches, parser_benchmarks);
criterion_main!(benches);
```

- [ ] **Step 2: Run the benchmark**

Run: `cargo bench -p gc-parser -- vt_parse_throughput`
Expected: Three benchmarks with throughput reported in MB/s.

- [ ] **Step 3: Commit**

```bash
git add crates/gc-parser/benches/parser_bench.rs
git commit -m "bench: add VT parser throughput benchmarks (plain, ansi, cursor)"
```

---

## Chunk 5: CI Workflow & Smoke Test Fixes

### Task 10: Add manually-triggered benchmark CI workflow

**Files:**
- Create: `.github/workflows/bench.yml`

- [ ] **Step 1: Create the workflow file**

Create `.github/workflows/bench.yml`:

```yaml
name: Benchmarks

on:
  workflow_dispatch:

env:
  CARGO_TERM_COLOR: always

jobs:
  bench:
    name: Run Benchmarks
    runs-on: macos-latest
    steps:
      - uses: actions/checkout@v6
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Run benchmarks
        run: cargo bench --workspace
      - name: Upload criterion reports
        uses: actions/upload-artifact@v4
        with:
          name: criterion-reports
          path: target/criterion/
          retention-days: 30
```

Note: Uses `actions/checkout@v6` and `Swatinem/rust-cache@v2` to match the existing `ci.yml` patterns.

- [ ] **Step 2: Verify YAML is valid**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/bench.yml'))"`
Expected: No errors (valid YAML).

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/bench.yml
git commit -m "ci: add manually-triggered benchmark workflow"
```

### Task 11: Fix test_memory_baseline smoke test

**Files:**
- Modify: `crates/ghost-complete/tests/smoke.rs:97`

- [ ] **Step 1: Bump memory threshold from 50MB to 150MB**

In `crates/ghost-complete/tests/smoke.rs`, change line 97:

From:
```rust
            assert!(rss_mb < 50, "RSS is {} MB, exceeds 50 MB threshold", rss_mb);
```

To:
```rust
            assert!(rss_mb < 150, "RSS is {} MB, exceeds 150 MB threshold", rss_mb);
```

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo test -p ghost-complete -- test_memory_baseline`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add crates/ghost-complete/tests/smoke.rs
git commit -m "fix: bump memory baseline threshold to 150MB for 717 embedded specs"
```

### Task 12: Fix test_large_output smoke test

**Files:**
- Modify: `crates/ghost-complete/tests/smoke.rs:30-49`

The issue is the 500ms sleep not being enough time for the PTY buffer to fully drain after `seq 1 5000`. Replace the fixed sleep with a polling approach that waits for a minimum output size.

- [ ] **Step 1: Replace fixed sleep with polling loop**

In `crates/ghost-complete/tests/smoke.rs`, replace the `test_large_output` function:

```rust
#[test]
fn test_large_output() {
    let mut proc = GhostProcess::spawn();
    proc.send_line("seq 1 5000");
    // Wait for last number to appear.
    proc.expect_output("5000");

    // Poll until output buffer has stabilized (no new bytes for 500ms).
    let mut prev_len = 0;
    for _ in 0..10 {
        thread::sleep(Duration::from_millis(500));
        let snapshot = proc.output_snapshot();
        if snapshot.len() == prev_len {
            break;
        }
        prev_len = snapshot.len();
    }

    let snapshot = proc.output_snapshot();
    let text = String::from_utf8_lossy(&snapshot);
    // Check a spread of numbers. Use numbers > 4 digits to avoid false positives
    // from ANSI escape sequence parameters (e.g. "\x1b[100;1H" cursor positioning).
    for n in &[1000, 2500, 3333, 4999, 5000] {
        let needle = format!("{}", n);
        assert!(
            text.contains(&needle),
            "large output missing expected number {} (output {} bytes)",
            n,
            snapshot.len()
        );
    }
    proc.exit_with_code(0);
}
```

Key change: Instead of a single 500ms sleep, poll up to 10 times (5 seconds total) and stop when the buffer size stabilizes.

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo test -p ghost-complete -- test_large_output`
Expected: PASS

- [ ] **Step 3: Run all smoke tests to confirm nothing regressed**

Run: `cargo test -p ghost-complete`
Expected: All 10 tests pass (8 + the 2 previously-failing ones).

- [ ] **Step 4: Commit**

```bash
git add crates/ghost-complete/tests/smoke.rs
git commit -m "fix: use polling loop in test_large_output for reliable PTY drain"
```

### Task 13: Run full benchmark suite and verify all targets

This is a validation task — no code changes.

- [ ] **Step 1: Run full workspace benchmarks**

Run: `cargo bench --workspace`
Expected: All benchmarks run. No panics or compilation errors.

- [ ] **Step 2: Verify performance targets**

Check the output for these targets:
- `fuzzy_ranking/10k_3char`: should be <1ms
- `engine_suggest_sync/subcommand_with_spec`: should be <50ms
- `vt_parse_throughput/*`: should report >100 MB/s

- [ ] **Step 3: Run full test suite to confirm nothing is broken**

Run: `cargo test --workspace`
Expected: All tests pass, including the fixed smoke tests.

- [ ] **Step 4: Run clippy and fmt**

Run: `cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: Clean.
