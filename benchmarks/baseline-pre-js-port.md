# Phase 0 Baseline — Pre-JS-Port Criterion Numbers

**Purpose:** Phase 4 regression detection. Any benchmark regressing >10% vs
these numbers fails the CI `bench-regression` gate
(`scripts/check-bench.sh`). The machine-readable sibling
`benchmarks/baseline-pre-js-port.json` is the ground truth the script reads;
this Markdown file is the human-friendly view of the same data.

**Captured at:** commit `dde88eac8de097745e9a56ede023afdb2a6ee705` on
2026-04-24.
**Hardware:** GitHub Actions `macos-latest` runner (same environment the
`bench-regression` gate in `ci.yml` executes in), stable Rust via
`rust-toolchain.toml`. Previously captured locally on an Apple M2 Pro,
but the gate runs on hosted CI hardware which is materially slower, so
the baseline was recaptured on that environment to eliminate a uniform
hardware-delta false-positive across every bench.
**Raw Criterion report:** `target/criterion/report/index.html` (produced
by the `Benchmarks` workflow in `.github/workflows/bench.yml`, uploaded
as the `criterion-reports` artifact).
**Wall time:** `cargo bench --workspace` takes ~5 minutes end-to-end on
the CI runner (15 benchmarks across 6 groups, 100 samples each plus 3 s
warm-up).

Each row below shows the **median** and the 95 % confidence interval on
the median (the `median.point_estimate` and
`median.confidence_interval.{lower,upper}_bound` fields of
`estimates.json`). Units are chosen for readability; the raw ns value is
the authoritative copy in the sibling JSON and is what
`scripts/check-bench.sh` compares against.

## Benchmarks

### Group: `fuzzy_ranking`

| Bench       | Median       | Lower (95 %) | Upper (95 %) | Raw median (ns) |
|-------------|--------------|--------------|--------------|-----------------|
| 1k_3char    | 115.72 µs    | 114.31 µs    | 116.71 µs    | 115717          |
| 10k_3char   | 1.150 ms     | 1.131 ms     | 1.176 ms     | 1150470         |
| 10k_empty   | 371.14 µs    | 366.93 µs    | 377.93 µs    | 371138          |

### Group: `spec_loading`

| Bench            | Median     | Lower (95 %) | Upper (95 %) | Raw median (ns) |
|------------------|------------|--------------|--------------|-----------------|
| load_717_specs   | 114.99 ms  | 114.47 ms    | 115.66 ms    | 114994542       |

### Group: `spec_resolution`

| Bench     | Median    | Lower (95 %) | Upper (95 %) | Raw median (ns) |
|-----------|-----------|--------------|--------------|-----------------|
| shallow   | 2.930 µs  | 2.918 µs     | 2.947 µs     | 2930            |
| deep      | 1.518 µs  | 1.494 µs     | 1.544 µs     | 1518            |

### Group: `transform_pipeline`

| Bench    | Median      | Lower (95 %) | Upper (95 %) | Raw median (ns) |
|----------|-------------|--------------|--------------|-----------------|
| simple   | 29.58 µs    | 29.42 µs     | 29.76 µs     | 29576           |
| regex    | 121.37 µs   | 121.02 µs    | 121.97 µs    | 121371          |
| json     | 31.86 µs    | 31.65 µs     | 32.12 µs     | 31858           |

### Group: `engine_suggest_sync`

| Bench                 | Median      | Lower (95 %) | Upper (95 %) | Raw median (ns) |
|-----------------------|-------------|--------------|--------------|-----------------|
| command_position      | 19.48 µs    | 18.95 µs     | 20.08 µs     | 19477           |
| subcommand_with_spec  | 18.13 µs    | 17.66 µs     | 18.39 µs     | 18127           |
| filesystem_fallback   | 1.272 ms    | 1.264 ms     | 1.289 ms     | 1271829         |

### Group: `vt_parse_throughput`

Each bench processes 64 KiB of synthesised VT input (plain text,
SGR-colored diff, cursor-addressing heavy) through `TerminalParser` from
the `gc-parser` crate. Criterion reports throughput in MiB/s in the
HTML report; timings below are per 64 KiB buffer.

| Bench          | Median      | Lower (95 %) | Upper (95 %) | Raw median (ns) |
|----------------|-------------|--------------|--------------|-----------------|
| plain_text     | 204.41 µs   | 202.87 µs    | 207.17 µs    | 204414          |
| ansi_colored   | 246.99 µs   | 246.83 µs    | 247.38 µs    | 246985          |
| cursor_heavy   | 303.10 µs   | 302.45 µs    | 304.20 µs    | 303098          |

## Spec corpus snapshot

- `du -sh specs/*.json` (top-level only, excluding `__snapshots__/`):
  **22 MB** across 709 spec files.
- `du -sh specs/`: **44 MB** (includes the `specs/__snapshots__/` tree
  checked in by Phase 0 T3 — ~22 MB of golden `.snap` files).

Spec counts:

- **Total specs:** 709
- **Specs containing at least one `requires_js: true` generator:** 184

Generator kinds, counted as *generator objects* across every spec
(generators live in nested `generators: [{...}]` arrays, including
inside subcommands, options, and args — so some specs contribute
multiple rows). Counts from the JSON itself:

| Generator kind                                    | Count  |
|---------------------------------------------------|-------:|
| JS-backed (`requires_js: true`)                   | 1889   |
| Shell/script (non-`requires_js`, has `.script`)   |  850   |
| Script template (non-`requires_js`, has `.template` in a generator) | 93 |
| `git_branches`  (Rust-native)                     |    8   |
| `git_tags`      (Rust-native)                     |    1   |
| `git_remotes`   (Rust-native)                     |    1   |
| `git_files`     (Rust-native)                     |    0   |

Template usage at the **arg** level (not in a generator): `template:
"filepaths"` or `template: "folders"` appears 4241 times across the
corpus (one per arg definition).

**Binary size baseline:** See `benchmarks/binary-size-baseline.txt`
(not duplicated here).

## Methodology

1. Run `cargo bench 2>&1 | tee /tmp/cargo-bench-phase0.log` from the
   worktree root. This builds release binaries for `gc-suggest` and
   `gc-parser`, then runs every Criterion group defined in
   `crates/gc-suggest/benches/suggest_bench.rs` and
   `crates/gc-parser/benches/parser_bench.rs`.
2. Criterion writes one `target/criterion/<group>/<bench>/new/estimates.json`
   per benchmark with median + CI.
3. Median + lower/upper bound are extracted from `.median.point_estimate`,
   `.median.confidence_interval.lower_bound`, and
   `.median.confidence_interval.upper_bound`. Values are kept as
   integer nanoseconds (rounded) in `benchmarks/baseline-pre-js-port.json`
   and converted to ns/µs/ms in this Markdown for readability.
4. `scripts/check-bench.sh` reads the JSON at gate time, compares
   median to a fresh `cargo bench` run, and fails if any bench is
   `>10 %` slower (threshold configurable via `--threshold <pct>`).

### Spec corpus `jq` one-liners

Reproducible from the worktree root. `requires_js` is tracked at the
*generator* level (a single spec can have many), so we enumerate every
generator object via the `..` recursive descent.

```bash
# Size and file count
du -sh specs/
find specs -maxdepth 1 -name '*.json' | wc -l

# Count of specs (files) containing any requires_js generator
find specs -maxdepth 1 -name '*.json' -print0 \
  | xargs -0 jq -r '
      if (.. | objects | select(.requires_js? == true)) then input_filename else empty end
    ' \
  | sort -u | wc -l

# Count of requires_js generator *objects* (can be >1 per spec)
find specs -maxdepth 1 -name '*.json' -print0 \
  | xargs -0 jq -c '[.. | objects | .generators? | select(. != null) | .[]? | select(.requires_js == true)] | length' \
  | awk '{s+=$1} END {print s}'

# Count of script generators that are NOT requires_js
find specs -maxdepth 1 -name '*.json' -print0 \
  | xargs -0 jq -c '[.. | objects | .generators? | select(. != null) | .[]? | select(.requires_js != true and (.script != null))] | length' \
  | awk '{s+=$1} END {print s}'

# Count of Rust-native generator objects by kind
find specs -maxdepth 1 -name '*.json' -print0 \
  | xargs -0 jq -c '[.. | objects | .generators? | select(. != null) | .[]? | select(.type == "git_branches")] | length' \
  | awk '{s+=$1} END {print s}'
# (repeat with "git_tags", "git_remotes", "git_files")

# Count of script-template generators (template field inside a generator)
find specs -maxdepth 1 -name '*.json' -print0 \
  | xargs -0 jq -c '[.. | objects | .generators? | select(. != null) | .[]? | select(.template != null)] | length' \
  | awk '{s+=$1} END {print s}'

# Arg-level templates ("filepaths" / "folders")
find specs -maxdepth 1 -name '*.json' -print0 \
  | xargs -0 jq -c '[.. | objects | select(.template == "filepaths" or .template == "folders")] | length' \
  | awk '{s+=$1} END {print s}'
```

### Regenerating the baseline

This recipe is self-sufficient — running it from a clean
`target/criterion/` tree (i.e. right after `cargo bench`) reproduces
the committed `benchmarks/baseline-pre-js-port.json` value-for-value.

1. Run the full benchmark suite. The committed numbers come from the
   GitHub Actions `macos-latest` runner (see `.github/workflows/bench.yml`
   — trigger via `gh workflow run bench.yml --ref master` and download
   the `criterion-reports` artifact). Expect **~5 minutes** of wall time
   on that runner; local M-series laptops will be ~10–20 % faster, so
   regenerating locally will produce values that, by design, trip the
   10 % gate — always regenerate on CI:

    ```bash
    cargo bench 2>&1 | tee /tmp/cargo-bench-phase0.log
    ```

    Criterion drops one `target/criterion/<group>/<bench>/new/estimates.json`
    per benchmark; the recipe below is what turns that tree into the
    JSON baseline.

2. Walk every `estimates.json`, project `.median` into the schema
    `scripts/check-bench.sh` reads, then fold into the nested
    `{groups: {group: {bench: {median_ns, lower_ns, upper_ns}}}}` layout:

    ```bash
    find target/criterion -name 'estimates.json' -path '*/new/*' | while read -r f; do
        group="$(echo "$f" | awk -F'/' '{print $(NF-3)}')"
        bench="$(echo "$f" | awk -F'/' '{print $(NF-2)}')"
        jq -r --arg group "$group" --arg bench "$bench" '
          {
            group: $group,
            bench: $bench,
            median_ns: (.median.point_estimate | round),
            lower_ns: (.median.confidence_interval.lower_bound | round),
            upper_ns: (.median.confidence_interval.upper_bound | round)
          }' "$f"
    done | jq -s '
        group_by(.group)
        | map({
            key: .[0].group,
            value: (
              sort_by(.bench)
              | map({key: .bench, value: {median_ns, lower_ns, upper_ns}})
              | from_entries
            )
          })
        | from_entries
        | {
            schema_version: "1.0",
            captured_at:    (now | todate),
            commit_sha:     "<fill in>",
            groups:         .
          }
    ' > benchmarks/baseline-pre-js-port.json
    ```

3. Fill in `commit_sha` manually with the commit at which the baseline
    was captured, typically:

    ```bash
    jq --arg sha "$(git rev-parse HEAD)" '.commit_sha = $sha' \
       benchmarks/baseline-pre-js-port.json \
       > benchmarks/baseline-pre-js-port.json.tmp \
    && mv benchmarks/baseline-pre-js-port.json.tmp benchmarks/baseline-pre-js-port.json
    ```

    This field is metadata only — `scripts/check-bench.sh` does not
    read it — but it lets future maintainers correlate a baseline
    with the tree that produced it.

4. Verify value-equivalence against the current committed baseline.
    `scripts/check-bench.sh` compares values only, so byte identity
    is not required; canonicalise both sides and diff:

    ```bash
    diff <(jq -S '.groups' benchmarks/baseline-pre-js-port.json) \
         <(jq -S '.groups' /path/to/old/baseline-pre-js-port.json)
    ```

    An empty diff confirms every `(group, bench) → {median_ns,
    lower_ns, upper_ns}` triple is identical.

**Notes for future maintainers:**

- **Key ordering in the committed JSON** reflects `find` discovery
  order at original capture time (Phase 0 T4a) rather than alphabetical.
  The recipe above emits alphabetically-sorted groups and benches
  (`sort_by(.bench)` + jq's `from_entries` which preserves insertion
  order on the group level); values are identical, only key order
  differs. Use `jq -S` for canonical comparison.
- **Manual post-processing at capture time (T4a):** none beyond
  filling in `commit_sha`. Values were taken directly from
  Criterion's `estimates.json`; the per-group Markdown tables in this
  file were hand-typed from the same numbers for readability (ns →
  µs/ms conversion done by the `cargo bench` report).
- **If the `schema_version` changes**, update both this recipe and
  `scripts/check-bench.sh` in the same PR.
