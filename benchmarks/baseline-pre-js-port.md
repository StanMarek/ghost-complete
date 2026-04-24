# Phase 0 Baseline — Pre-JS-Port Criterion Numbers

> **Archival.** These numbers are a historical snapshot captured for
> Phase 4 regression detection. As of the Option-B restructure of the
> `bench-regression` gate (`scripts/check-bench.sh` + `ci.yml`), the
> gate no longer reads `benchmarks/baseline-pre-js-port.json`; it
> benches the PR's base ref and HEAD ref back-to-back on the same
> runner and gates on Criterion's own change record. This file is
> retained as a reference for what performance looked like at the
> pre-JS-port capture point.

**Purpose:** Historical reference — performance snapshot captured
before the requires-js-specs port work. The machine-readable sibling
`benchmarks/baseline-pre-js-port.json` carries the same numbers in
structured form.

**Captured at:** commit `0e62bcbf68970657eedf6b5940854f1ab80b2792` on
2026-04-22.
**Hardware:** Apple M2 Pro, 12 cores, 16 GB RAM, macOS 26.4.1 (Darwin
25.4.0 arm64), stable Rust via `rust-toolchain.toml`.
**Raw Criterion report:** `target/criterion/report/index.html`.
**Raw log:** `/tmp/cargo-bench-phase0.log` (retained locally; not checked in).
**Wall time:** `cargo bench` took ~3 minutes end-to-end (15 benchmarks
across 6 groups, 100 samples each plus 3 s warm-up).

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
| 1k_3char    | 104.03 µs    | 103.73 µs    | 104.32 µs    | 104031          |
| 10k_3char   | 1.082 ms     | 1.060 ms     | 1.091 ms     | 1081973         |
| 10k_empty   | 340.47 µs    | 340.16 µs    | 340.96 µs    | 340475          |

### Group: `spec_loading`

| Bench            | Median     | Lower (95 %) | Upper (95 %) | Raw median (ns) |
|------------------|------------|--------------|--------------|-----------------|
| load_717_specs   | 97.49 ms   | 96.73 ms     | 98.39 ms     | 97490813        |

### Group: `spec_resolution`

| Bench     | Median    | Lower (95 %) | Upper (95 %) | Raw median (ns) |
|-----------|-----------|--------------|--------------|-----------------|
| shallow   | 2.463 µs  | 2.461 µs     | 2.468 µs     | 2463            |
| deep      | 1.278 µs  | 1.277 µs     | 1.280 µs     | 1278            |

### Group: `transform_pipeline`

| Bench    | Median      | Lower (95 %) | Upper (95 %) | Raw median (ns) |
|----------|-------------|--------------|--------------|-----------------|
| simple   | 26.43 µs    | 26.38 µs     | 26.48 µs     | 26427           |
| regex    | 112.88 µs   | 112.58 µs    | 113.33 µs    | 112879          |
| json     | 27.94 µs    | 27.89 µs     | 27.99 µs     | 27937           |

### Group: `engine_suggest_sync`

| Bench                 | Median      | Lower (95 %) | Upper (95 %) | Raw median (ns) |
|-----------------------|-------------|--------------|--------------|-----------------|
| command_position      | 17.63 µs    | 17.59 µs     | 17.72 µs     | 17634           |
| subcommand_with_spec  | 16.35 µs    | 16.00 µs     | 16.54 µs     | 16345           |
| filesystem_fallback   | 1.060 ms    | 1.059 ms     | 1.066 ms     | 1060472         |

### Group: `vt_parse_throughput`

Each bench processes 64 KiB of synthesised VT input (plain text,
SGR-colored diff, cursor-addressing heavy) through `TerminalParser` from
the `gc-parser` crate. Criterion reports throughput in MiB/s in the
HTML report; timings below are per 64 KiB buffer.

| Bench          | Median      | Lower (95 %) | Upper (95 %) | Raw median (ns) |
|----------------|-------------|--------------|--------------|-----------------|
| plain_text     | 178.87 µs   | 178.70 µs    | 179.13 µs    | 178868          |
| ansi_colored   | 224.05 µs   | 223.54 µs    | 224.54 µs    | 224052          |
| cursor_heavy   | 277.88 µs   | 276.65 µs    | 279.74 µs    | 277885          |

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

1. Run the full benchmark suite. Expect **~3 minutes** of wall time on
   the reference hardware documented above (Apple M2 Pro, 12 cores):

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
