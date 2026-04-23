# Phase 4 T8 — Binary Size Investigation

**Date:** 2026-04-23
**Branch:** `integration/requires-js-specs` (PR #75)
**Author:** Stanislaw Marek
**Status:** DONE — intervention landed; binary is 29,798,512 bytes
(28.42 MB), well under the 30 MB ceiling.

## TL;DR

A single build-time intervention — minifying the embedded
completion-spec JSON and stripping the runtime-unused `js_source`
field — brings `target/release/ghost-complete` from
**49,923,744 bytes (47.61 MB)** to **29,798,512 bytes (28.42 MB)**:
a **20,125,232-byte / 40.3 %** reduction. No runtime code was
changed and no new runtime dependency was added; the only new
dependency is a `[build-dependencies]` addition of `serde_json`
(already a transitive workspace dep, so no new crates download at
release time).

All 16 test suites remain green, clippy is clean, and
`scripts/check-bench.sh` reports "no regressions > 10%" across all
15 Criterion benchmarks against the pre-JS-port baseline.

## Why the binary was 47 MB

`size target/release/ghost-complete` attribution on the baseline:

```
sectname        segname         size         notes
__text          __TEXT          0x003aaa14   3.84 MiB   (compiled code)
__const         __TEXT          0x028629f0   42.08 MiB  (read-only data)
__cstring       __TEXT          0x000163c2   91 KiB
__eh_frame      __TEXT          0x0008efbc   586 KiB
__gcc_except_tab                0x0002f0a4   193 KiB
__unwind_info                   0x00018b88   99 KiB
```

Code was **3.8 MiB**. The bulk (**42 MiB**) was `__TEXT.__const`:
read-only data, dominated by the 709 JSON completion specs embedded
via `include_str!` in `crates/gc-suggest/src/embedded.rs`.

`cargo bloat --crates` top 10 on the baseline binary (percentages
of the **.text** section only — not the whole binary, which was 92 %
data):

```
File  .text     Size Crate
1.5%  19.7% 741.2KiB std
0.8%  10.5% 392.8KiB gc_suggest
0.8%  10.3% 387.6KiB regex_automata
0.5%   6.4% 240.5KiB gc_pty
0.5%   6.2% 231.4KiB clap_builder
0.4%   4.9% 185.4KiB regex_syntax
0.3%   4.3% 162.5KiB toml
0.3%   4.3% 160.3KiB toml_edit
0.3%   4.0% 151.7KiB tokio
0.3%   4.0% 150.5KiB gc_config
```

Top 20 functions were all in the noise (0.2–0.4 % of `.text` each,
mostly `clap_builder` command-building, `gc_pty::proxy::run_proxy`
closures, and `regex_automata` DFA builders — none actionable for
size without breaking functionality).

So: the investigation ladder's **Step 1** (attribution) confirmed
that this is a **data-size** problem, not a code-size problem.
Steps 3 (feature flags) and 4 (LTO/strip) could only touch the
3.8 MiB code segment; they cannot move the needle on a 42 MiB data
segment. That made **Step 2** (embedded-spec handling) the only
viable intervention.

## Spec-corpus breakdown

Measured by walking all 709 `specs/*.json` and summing field
contributions:

| Field               | Cumulative bytes | Share   |
|---------------------|-----------------:|--------:|
| Whole corpus (source, pretty-printed JSON) | 21,371,214 | 100 %   |
| `description` strings | 5,378,226 | 25.2 % |
| `js_source` strings   |    435,186 |  2.0 % |
| `_corrected_in` strings |    1,539 |  0.01 % |

Re-serialising each spec with `serde_json::to_string` (no
pretty-print whitespace) drops the corpus from 21,371,214 bytes
to 11,794,333 bytes — **9,576,881 bytes (44.8 %) of the corpus is
whitespace**. Adding `js_source` strip brings it to 11,306,699
bytes (12 MiB source; ~45 % of original).

The binary grew by roughly 2× the source-byte count pre-intervention
(21 MiB on disk → 42 MiB in `__const`), so shrinking the source to
11.3 MiB roughly halves the binary, which is exactly what we
observed (48 MB → 30 MB).

## The intervention

Added `crates/gc-suggest/build.rs`:

1. Reads every `../../specs/*.json` at build time.
2. Parses each through `serde_json::Value`.
3. Walks the tree and removes every `js_source` key found
   (including nested under subcommands, options, args).
   - `_corrected_in` is **kept** — it's consumed at runtime by
     `ghost-complete doctor` to surface mis-converted generators.
   - `description` is **kept** — it's shown in the popup next to
     each suggestion.
4. Re-serialises with `serde_json::to_string` (compact, no
   whitespace).
5. Writes the stripped, minified spec to
   `$OUT_DIR/specs-min/<name>.json`.
6. Emits `$OUT_DIR/embedded_specs.rs` with the full
   `EMBEDDED_SPECS: &[(&str, &str)]` const, each tuple pointing at
   the corresponding stripped file via `include_str!`.
7. Emits `cargo:rerun-if-changed=` for the specs directory and
   every spec file so cargo reruns the script on spec
   additions/removals/edits.

`crates/gc-suggest/src/embedded.rs` now drops the hand-maintained
709-entry `include_str!` list (~1 160 lines) and `include!`s the
generated file:

```rust
include!(concat!(env!("OUT_DIR"), "/embedded_specs.rs"));
```

The original `specs/` source tree is untouched. Tests that read
`specs/git.json` or `specs/curl.json` (e.g.
`test_curl_dash_o_resolve_spec_sets_wants_filepaths`,
`test_deserialize_git_spec`) see the pretty-printed source unchanged.
The converter under `tools/fig-converter/` still emits
pretty-printed JSON.

## Measurements

| Metric                                       | Baseline       | After T8       | Δ              |
|---------------------------------------------:|---------------:|---------------:|---------------:|
| `wc -c target/release/ghost-complete` (bytes)| 49,923,744     | 29,798,512     | −20,125,232    |
| Release binary (MB)                          | 47.61 MB       | 28.42 MB       | −19.19 MB      |
| `wc -c target/dist/ghost-complete` (bytes)   | 49,631,328     | 29,784,000     | −19,847,328    |
| Dist binary (MB)                             | 47.33 MB       | 28.40 MB       | −18.93 MB      |
| `.text` section                              | 3.84 MiB       | 3.84 MiB       | 0 (no code change) |
| `__const`                                    | 42.08 MiB      | ~20 MiB        | −22 MiB        |
| `cargo test --workspace --release`           | green          | green          |                |
| `cargo clippy --all-targets`                 | clean          | clean          |                |

The dist profile (what release artifacts actually ship) is
29,784,000 bytes — also under 30 MB, with 215 KB of headroom after
thin LTO.

## Benchmark regression check

Ran `cargo bench` end-to-end and then `bash scripts/check-bench.sh`.
Final result:

```
PASS: checked 15 benchmark(s); no regressions > 10%.
```

All 15 benchmarks across 6 groups (`fuzzy_ranking`, `spec_loading`,
`spec_resolution`, `transform_pipeline`, `engine_suggest_sync`,
`vt_parse_throughput`) came in within ±10 % of the pre-JS-port
baseline. `spec_loading` specifically is unchanged — the benchmark
loads specs from `specs/` (source tree, untouched), not from the
embedded copies.

### Noise-induced false positive during investigation

The first bench run flagged `transform_pipeline/json` as a +24 %
regression. The root cause was machine contention — a concurrent
`cargo build --profile dist` and the bench process having just been
killed and restarted.  Rerunning `cargo bench -p gc-suggest --
'transform_pipeline'` on a quiet machine reproduced 27.8 µs vs the
baseline's 27.9 µs — i.e. within 0.4 %. After that rerun,
`check-bench.sh` passed cleanly. The json-transform code path
operates on 100 synthetic JSON lines generated in-memory; it does
not touch embedded specs, so there's no mechanism by which this
intervention could affect it.

## Investigation ladder — what was tried, what shipped

| Step | Intervention                                          | Estimated delta | Outcome | Shipped? |
|-----:|:------------------------------------------------------|----------------:|:--------|:---------|
| 1    | `cargo bloat --crates` attribution                    | (diagnostic)    | Confirmed data-size problem (42 MiB `__const`), not code | N/A |
| 2a   | Strip `description` from embedded specs               | ~5 MB source   | **Not attempted** — `description` is used at runtime (shown in popup) | No |
| 2a   | Strip `js_source` from embedded specs                 | ~435 KB source | Part of the shipping intervention | Yes |
| 2a   | Minify embedded JSON (remove whitespace)              | ~9.6 MB source | Part of the shipping intervention | Yes |
| 2b   | zstd compression of embedded specs with runtime decompress | unknown | **Not needed** — 2a alone hit the target | No |
| 2c   | Sidecar `js_source` file on disk                      | ~435 KB | **Not needed** — plain strip from embedded copies was simpler and the field is diagnostic-only | No |
| 3    | Dependency audit / feature flag trimming              | <1 MB plausible | **Not needed** — code is 3.8 MiB total, not the bottleneck | No |
| 4    | `[profile.release]` LTO/strip/opt-level=z tweaks      | ~1–2 MB | **Not needed** — would only touch 3.8 MiB of code; dist profile already does thin-LTO | No |

Note on 2a `description`: descriptions are the single largest
stripable field (5.4 MB vs 435 KB for `js_source`). We left them in
because `resolve_spec` copies `description` into every emitted
`Suggestion` and the popup renderer shows them. Stripping them
would degrade the user-facing UX ("what does this flag do?") in
exchange for a binary-size win we don't need. This is a good
candidate for a **future optional toggle** — see "Follow-up
opportunities" below.

## Follow-up opportunities (for a later phase — not needed now)

1. **Optional `description` stripping behind a feature flag.** If a
   future release wants a smaller binary for space-constrained
   install targets (e.g. a hypothetical `ghost-complete-lite`), a
   `--no-default-features --features strip-descriptions` build
   could land another ~5 MB of savings by setting
   `description: None` on every spec in `build.rs`.

2. **zstd compression at build time + runtime decompress.**
   Adds a runtime `zstd` dep, costs ~5–10 µs per spec load, but
   shrinks the corpus by another ~3–4×. Only worth it if we
   need to drop below ~15 MB.

3. **`__const` sharing across sibling specs.** Many options are
   repeated across 200+ specs (`--help`, `--version`, `--quiet`).
   A build-time interner that computed once and reused descriptions
   across specs could save another 1–2 MB but would require
   restructuring the runtime spec-load path.

4. **`[profile.release] strip = true`** in the workspace
   `Cargo.toml`. This would save ~1.6 MB (measured: strip-symbols
   on the post-intervention binary). Skipped for now because the
   current binary is already under the ceiling and strip can make
   panics less diagnosable; worth revisiting when the code segment
   crosses ~5 MB.

## Artefacts

- `crates/gc-suggest/build.rs` — build script (new).
- `crates/gc-suggest/Cargo.toml` — added `[build-dependencies]
  serde_json = "1"`.
- `crates/gc-suggest/src/embedded.rs` — replaced 1 160 lines of
  hand-maintained `include_str!` tuples with a single `include!` of
  the generated file; updated the module doc.
- `benchmarks/binary-size-baseline.txt` — updated from
  49,385,984 → 29,798,512 so future `check-binary-size.sh
  --delta-max` runs catch regressions against the new floor.
- `docs/phase-4-binary-size-findings.md` — this document.
