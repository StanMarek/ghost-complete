# CI Gates

## Overview

Four CI gates live in `.github/workflows/ci.yml`. Three were introduced in Phase 0.5 (binary size, snapshot diff, fig-converter oracle); a fourth (coverage baseline drift) was added in Phase 4. Benchmark-regression checking is intentionally **not** a CI gate — it is run manually at release time (see [Release-time benchmark checking](#release-time-benchmark-checking) below). The gates are wired via `needs:` dependencies, which controls **ordering within a workflow run** — i.e. a gate waits for its prerequisite jobs before it starts. That is a separate concern from **branch protection**, which is what blocks the GitHub merge button on a PR. A repo admin must explicitly configure each status check as required in GitHub's branch-protection settings (see [Branch-protection configuration](#branch-protection-configuration) below). Without that step, the gates run and report results but cannot block a merge.

---

## Gates

### Binary size gate

**Job name in CI:** `Binary size gate`
**YAML key:** `binary-size-gate`
**Trigger:** `needs: [check]` — runs after the `check` job succeeds.

**Purpose:** enforces two independent size constraints on the release binary:

1. **Absolute ceiling (110 MB)** — the binary must not exceed 110 MB. Raising it requires an explicit plan amendment. The ceiling moved from 30 MB to 110 MB in `ux-8` to admit the AWS completion spec; zstd-compressing embedded specs (a separate plan) is the principled reclaim path that should drop the binary back near the original ceiling.
2. **Per-phase delta budget (default +2 MB)** — the binary must not have grown by more than `PHASE_BUDGET` (set to `2MB` in the job env) since the size recorded in [`benchmarks/binary-size-baseline.txt`](../benchmarks/binary-size-baseline.txt).

**Failure modes:**

- Absolute ceiling failure: binary size exceeds 110 MB.
- Delta budget failure: binary grew by more than `PHASE_BUDGET` since the baseline was recorded.

**Status today:** production-live and **passing**. Phase 4 T8 landed the original binary-size intervention (minified embedded specs + stripped `js_source`) which dropped the binary to ~28.4 MB, under the original 30 MB ceiling. The `ux-8` AWS spec restoration brought the binary to ~102 MB; the ceiling moved to 110 MB to match plus headroom. See [`docs/phase-4-binary-size-findings.md`](./phase-4-binary-size-findings.md) for the original phase-4 attribution and the `ux-8` PR for the AWS-restoration delta. Ready to add to branch protection.

**How to debug locally:**

```bash
cargo build --release
scripts/check-binary-size.sh --absolute-max 110MB
scripts/check-binary-size.sh --delta-max 2MB
```

**Baseline maintenance:** when a phase legitimately grows the binary, update the baseline file:

```bash
du -b target/release/ghost-complete > benchmarks/binary-size-baseline.txt
```

---

### Snapshot diff gate

**Job name in CI:** `Snapshot diff gate`
**YAML key:** `snapshot-diff-gate`
**Trigger:** `needs: [check, binary-size-gate]` — runs after both `check` and `Binary size gate` succeed. Size is cheaper to check first, and the plan chains them explicitly.

**Purpose:** catches PRs that modify `specs/*.json` files without updating the corresponding `specs/__snapshots__/*.snap` entries.

**Failure modes:** diff found between a spec file and its snapshot.

**Status today:** production-live. `specs/__snapshots__/` is populated (709 snapshots). `scripts/check-snapshots.sh` runs on every CI build.

**How to debug locally:**

```bash
scripts/check-snapshots.sh
```

---

### Oracle gate (fig-converter)

**Job name in CI:** `Oracle gate (fig-converter)`
**YAML key:** `oracle-gate`
**Trigger:** `needs: [check]`, additionally guarded by `if: github.event_name == 'pull_request'` and a path filter on `tools/fig-converter/**`. The gate only runs on PRs that touch the converter.

**Purpose:** runs the fig-converter correctness oracle to detect semantic mismatches between the JS reference implementation and the Rust transform pipeline.

**Failure modes:** oracle reports a mismatch between JS and Rust outputs for any changed converter path.

**Status today:** production-live. Runs on PRs that change `tools/fig-converter/` files. Pass rate: see [`tools/fig-converter/docs/oracle-results.md`](../tools/fig-converter/docs/oracle-results.md).

**How to debug locally:**

```bash
cd tools/fig-converter && npm run oracle:changed
```

---

### Coverage baseline drift

**Job name in CI:** `Coverage baseline drift`
**YAML key:** `coverage-baseline-drift`
**Trigger:** runs on pushes to `master` and on PRs whose base branch is `master` or `main`. Feature-branch pushes are skipped entirely — the gate only cares about what's landing on trunk. No `needs:` dependency; runs in parallel with other gates.

**Purpose:** reminds maintainers to refresh `docs/coverage-baseline.json` when it goes stale. The baseline powers the spec-coverage numbers reported in `ghost-complete status --json` and the `docs/SPECS.md` rollup. Release cadence is roughly monthly; "two releases old" is ~60–90 days; 120 days gives a comfortable buffer before the gate nags.

**Failure modes:** this gate is **NON-FAILING by design**. It always exits 0. When the latest `docs/coverage-baseline.json` release row's `timestamp` is more than 120 days in the past, the job emits a GitHub Actions `::warning::` annotation in the job log (visible in the PR checks panel). The annotation is the only signal — the check itself reports green.

Because it never fails, this job **must not** be added to branch protection. Its purpose is informational drift detection, not gatekeeping.

**How to debug locally:**

```bash
scripts/check-coverage-baseline-drift.sh              # prints "ok: ... days old" or a ::warning:: line
scripts/check-coverage-baseline-drift.sh --quiet      # suppresses the "ok" line
scripts/check-coverage-baseline-drift.sh --threshold 30  # tighten the threshold to simulate drift
```

To refresh the baseline: run `ghost-complete status --json` and follow the process in [`docs/SPECS.md`](./SPECS.md).

---

## Release-time benchmark checking

Benchmark regression is **not** enforced on every PR. Hosted runner variance (±15–20% on single-threaded latency benches) makes CI-gated benchmarking noisy enough that the signal-to-noise ratio doesn't justify the minutes spent. Instead, the release process runs benchmarks locally on a quiet machine and records the numbers in the release PR.

The tooling is preserved:

- [`.github/workflows/bench.yml`](../.github/workflows/bench.yml) — manual `workflow_dispatch` job that runs `cargo bench --workspace` and uploads Criterion reports as an artifact.
- [`scripts/check-bench.sh`](../scripts/check-bench.sh) — threshold-based comparator against a saved Criterion baseline.
- [`benchmarks/`](../benchmarks/) — per-release report files (`v<version>.md`) plus `baseline-pre-js-port.json` for historical diffs.

**Release workflow:**

```bash
cargo bench --workspace -- --save-baseline release-<prev>    # one-time, on the prior release tag
cargo bench --workspace -- --baseline release-<prev>         # on the release candidate
scripts/check-bench.sh --threshold 10                         # optional gate for the release author
```

Include the Criterion summary and any regression >10% in `benchmarks/v<version>.md` as part of the release PR per the process in [`CLAUDE.md`](../CLAUDE.md#benchmarking).

---

## Branch-protection configuration

These steps require repo admin access. Without them the gates run but **do not block merge**.

1. Go to <https://github.com/StanMarek/ghost-complete/settings/branches>.
2. Edit the branch protection rule for `master`, or create one if none exists.
3. Enable **"Require status checks to pass before merging"**.
4. In the status check search box, add the checks listed as "Ready to add" in the table below by their **exact display names** (the human-readable `name:` values from the CI YAML, not the YAML job keys).
5. Save the rule.

These checks are added **alongside** any existing required checks (e.g. `Check`, `Test`, `Clippy`, `Format`, `MSRV (1.86)`, `Linux tripwire (compile-check only)`). They replace nothing.

### Readiness table

| Gate | Branch protection status |
|---|---|
| `Snapshot diff gate` | Ready to add. |
| `Oracle gate (fig-converter)` | Ready to add. |
| `Binary size gate` | Ready to add. |
| `Coverage baseline drift` | Informational only (non-blocking warning). Do not add to branch protection. |

> **Note on job names vs. YAML keys:** GitHub branch protection displays the `name:` field of each job, not the YAML key. `Binary size gate` (the name) corresponds to `binary-size-gate` (the key). Using the YAML key in the search box will not match.

---

## FAQ

**"Why is the ceiling 110 MB?"**

The 30 MB ceiling was set during the requires-js-specs initiative as the target the binary needed to reach after specs were trimmed. Phase 4 T8 landed the intervention (minified embedded specs + stripped `js_source`) and the release binary stabilised at ~28.4 MB, under budget. In `ux-8` the AWS spec was restored: 409 inlined service sub-specs (upstream ships 418 `.js` files but the top-level `aws.js` only references 408 via `loadSpec` — 9 deprecated services are unreferenced) carrying ~28 MB of upstream description text, which `include_str!` roundtrips into ~2× `__const` data. The release binary moved to ~102 MB; the ceiling moved to 110 MB to match plus ~8% headroom. The delta budget (`PHASE_BUDGET=2MB`) still handles the near-term constraint — "don't grow from the current baseline". These are two independent checks; both must pass. zstd-compressing embedded specs is tracked as a follow-on plan; landing it should let the ceiling drop back near the original 30 MB level.

**"Can I skip a gate on a specific PR?"**

No. Required status checks are all-or-nothing. For a legitimate one-off exception (e.g. an unavoidable binary size overrun covered by a plan amendment), the admin must:

1. Temporarily remove the specific status check from branch protection.
2. Merge the PR.
3. Re-add the status check immediately after.

This is an emergency procedure. Document the exception in the PR description and in the relevant plan file.

**"Why is coverage baseline drift non-failing?"**

A stale baseline is a documentation-freshness signal, not a correctness problem. Blocking merges because `docs/coverage-baseline.json` is old would halt unrelated work whenever the maintainer forgets to refresh stats. The warning annotation surfaces the issue in the PR checks panel without stopping the line.
