# Benchmarking & Performance Testing Design

## Goal

Add a criterion-based benchmark suite to Ghost Complete covering the critical hot paths in `gc-suggest` and `gc-parser`, with a manually-triggered CI workflow for regression detection and HTML report generation.

## Architecture

Per-crate `benches/` directories following Rust conventions. Each benchmarked crate (`gc-suggest`, `gc-parser`) gets its own criterion benchmark file. No shared fixture crate — each bench file constructs its own inputs. A manually-triggered GitHub Actions workflow runs `cargo bench` and uploads criterion's HTML reports as artifacts.

## Performance Targets

| Metric | Target | Benchmark |
|--------|--------|-----------|
| Fuzzy match 10k candidates | <1ms | `fuzzy_ranking/10k_3char` |
| Keystroke to suggestion (sync) | <50ms | `engine_suggest_sync/*` |
| Spec loading (717 specs) | Fast startup | `spec_loading` |
| VT parser throughput | >100 MB/s | `vt_parse_throughput/*` |

## Scope

### In scope
- `gc-suggest`: fuzzy ranking, spec loading, spec resolution, transform pipelines, engine suggest_sync
- `gc-parser`: VT parser throughput
- Manually-triggered CI workflow
- Fix broken smoke tests (`test_memory_baseline`, `test_large_output`)

### Out of scope
- `gc-overlay` (popup rendering) — microsecond-level, not worth benchmarking
- `gc-buffer` (tokenization) — microsecond-level, not worth benchmarking
- `gc-pty` (PTY proxy overhead) — requires end-to-end process spawning, better measured manually
- Automated CI baseline comparison — too noisy on shared runners

---

## File Structure

```
crates/gc-suggest/
  Cargo.toml                    # Add criterion dev-dep + [[bench]] section
  benches/
    suggest_bench.rs            # All gc-suggest benchmarks
crates/gc-parser/
  Cargo.toml                    # Add criterion dev-dep + [[bench]] section
  benches/
    parser_bench.rs             # VT parser throughput benchmark
.github/workflows/bench.yml    # Manual-trigger benchmark workflow
```

No new crates. No shared fixture libraries.

### Visibility changes required in `gc-suggest/src/lib.rs`

Criterion benchmarks compile as external crates — they cannot access private modules. The following changes are required:

1. **`mod fuzzy` → `pub mod fuzzy`** — Exposes `fuzzy::rank()` for direct microbenchmarking. The `rank` function is a clean, stateless function with no encapsulation reason to keep private.

2. **`SuggestionEngine::with_providers()`: remove `#[cfg(test)]` guard** — Make it unconditionally `pub`. This constructor is needed for deterministic benchmark setup with controlled providers. It has no security or safety implications.

3. **`mod commands` → `pub mod commands`** — Exposes `CommandsProvider::from_list()` for benchmark setup.

4. **`mod history` → `pub mod history`** — Exposes `HistoryProvider::from_entries()` for benchmark setup.

5. **Remove `#[cfg(test)]` from `CommandsProvider::from_list()` and `HistoryProvider::from_entries()`** — These constructors must be available in bench code.

These are minimal visibility changes — only the constructors needed for deterministic test/bench setup become public. The provider implementations remain internal.

---

## gc-suggest Benchmarks (`suggest_bench.rs`)

### 1. `fuzzy_ranking`

Benchmarks `gc_suggest::fuzzy::rank()` directly (requires `pub mod fuzzy` visibility change above).

**Sub-benchmarks:**

| Name | Candidates | Query | Purpose |
|------|-----------|-------|---------|
| `1k_3char` | 1,000 synthetic filenames | `"fil"` | Baseline |
| `10k_3char` | 10,000 synthetic filenames | `"fil"` | Documented <1ms target |
| `10k_empty` | 10,000 synthetic filenames | `""` | Truncation-only fast path |

**Input construction:** `format!("file_{i}.txt")` for N entries. Candidates constructed once, cloned per iteration.

### 2. `spec_loading`

Benchmarks `SpecStore::load_from_dir()` against the real `specs/` directory (717 JSON specs).

Measures JSON deserialization + transform pipeline validation for all specs. Single benchmark, no variants.

Spec directory resolved via `CARGO_MANIFEST_DIR` (same pattern as existing tests).

### 3. `spec_resolution`

Benchmarks `specs::resolve_spec()` with pre-loaded specs and pre-constructed `CommandContext` structs.

**Sub-benchmarks:**

| Name | Command | Context | Purpose |
|------|---------|---------|---------|
| `shallow` | `git checkout` | word_index=1, current_word="" | 1 level subcommand resolution |
| `deep` | `docker compose up` | word_index=3, current_word="--" | 2 levels + flag matching |

Uses real specs loaded from disk.

### 4. `transform_pipeline`

Benchmarks `execute_pipeline()` with realistic inputs.

**Sub-benchmarks:**

| Name | Transforms | Input | Purpose |
|------|-----------|-------|---------|
| `simple` | `split_lines, filter_empty, trim` | 500 lines of whitespace-padded text | Common fast path |
| `regex` | `split_lines, regex_extract` | 200 lines of `git branch -v` style output | Regex compilation + extraction |
| `json` | `json_extract` | 100 lines, each a JSON object | JSON parsing cost |

Input strings constructed once, cloned per iteration.

### 5. `engine_suggest_sync`

Benchmarks `SuggestionEngine::suggest_sync()` end-to-end with realistic setup.

**Setup (constructed once, reused across iterations):**
- All 717 real specs loaded from `specs/` directory via `SpecStore::load_from_dir()`
- Temp directory with ~2,000 generated files (mix of `.rs`, `.txt`, `.json`, directories)
- `CommandsProvider::from_list()` with ~100 synthetic command names (deterministic)
- `HistoryProvider::from_entries()` with ~50 synthetic history entries (deterministic)
- Engine constructed via `SuggestionEngine::with_providers()` (requires visibility changes described above)

This ensures benchmark results are reproducible across machines — no dependency on the host's `$PATH` or `~/.zsh_history`.

**Sub-benchmarks:**

| Name | Context | Exercises |
|------|---------|-----------|
| `command_position` | word_index=0, query="gi" | commands + history + fuzzy |
| `subcommand_with_spec` | git, word_index=1, query="ch" | spec resolution + fuzzy |
| `filesystem_fallback` | unknown_cmd, word_index=1, query="" | filesystem provider + fuzzy |

---

## gc-parser Benchmarks (`parser_bench.rs`)

### `vt_parse_throughput`

Benchmarks `TerminalParser::process_bytes()`.

**Sub-benchmarks:**

| Name | Input | Size | Purpose |
|------|-------|------|---------|
| `plain_text` | ASCII printable lines | 64 KB | Baseline character dispatch |
| `ansi_colored` | Text with SGR color codes | 64 KB | Typical colored output (git diff, grep) |
| `cursor_heavy` | CSI cursor movement sequences | 64 KB | Worst-case performer dispatch |

Each benchmark uses `BenchmarkGroup::throughput(Throughput::Bytes(size))` so criterion reports MB/s.

A fresh `TerminalParser::new(24, 80)` is constructed per iteration to avoid accumulating state.

**Input construction:**
- `plain_text`: Repeated `"drwxr-xr-x  5 user staff  160 Mar 12 10:00 dirname\n"` lines to fill 64KB
- `ansi_colored`: Repeated `"\x1b[32m+added line\x1b[0m\n\x1b[31m-removed line\x1b[0m\n"` to fill 64KB
- `cursor_heavy`: Repeated `"\x1b[H\x1b[2J\x1b[10;20Htext\x1b[1A\x1b[5C"` to fill 64KB

---

## CI Integration

### `.github/workflows/bench.yml`

```yaml
name: Benchmarks
on:
  workflow_dispatch:

jobs:
  bench:
    runs-on: macos-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo bench --workspace
      - uses: actions/upload-artifact@v4
        with:
          name: criterion-reports
          path: target/criterion/
```

**No baseline comparison in CI.** Shared runner variance (10-20%) makes automated regression detection unreliable. The HTML reports serve as downloadable artifacts for manual inspection.

### Local Usage

```bash
# Run all benchmarks
cargo bench

# Run only one crate
cargo bench -p gc-suggest

# Run one benchmark group
cargo bench -p gc-suggest -- fuzzy_ranking

# Save baseline, make changes, compare
cargo bench -- --save-baseline before
# ... edit code ...
cargo bench -- --baseline before
```

Reports generated at `target/criterion/report/index.html`.

---

## Smoke Test Fixes

Two existing smoke tests in `crates/ghost-complete/tests/smoke.rs` are currently failing and should be fixed as part of this work:

### `test_memory_baseline`

**Problem:** Threshold is 50MB but RSS is ~120MB with 717 embedded specs.

**Fix:** Bump threshold to 150MB. The 50MB target was set for the 34-spec era. With 717 specs embedded via `include_str!`, ~120MB RSS is expected. Binary size optimization (lazy loading, compression) is a v0.2.x concern, not a benchmark concern.

### `test_large_output`

**Problem:** `seq 1 5000` output snapshot captures only 12 bytes. PTY buffer drain timing issue.

**Fix:** Increase drain sleep from 500ms to 1500ms, or switch to a polling loop that waits for expected byte count before snapshotting.

---

## Dependencies

### `gc-suggest/Cargo.toml` additions

```toml
[dev-dependencies]
criterion = { version = "0.5", features = ["html_reports"] }

[[bench]]
name = "suggest_bench"
harness = false
```

### `gc-parser/Cargo.toml` additions

```toml
[dev-dependencies]
criterion = { version = "0.5", features = ["html_reports"] }

[[bench]]
name = "parser_bench"
harness = false
```

No other dependency changes. No feature flags. Visibility changes to `gc-suggest` internals are described in the "Visibility changes required" section above.
