# CI Gates

## Overview

Seven CI gates live in `.github/workflows/ci.yml`: binary size, snapshot diff, fig-converter oracle, corpus hash determinism (fig-converter PR subset), corpus hash determinism (full corpus on trunk pushes), coverage baseline drift, and coverage regression. Benchmark-regression checking is intentionally **not** a CI gate — it is run manually at release time (see [Release-time benchmark checking](#release-time-benchmark-checking) below). The gates are wired via `needs:` dependencies, which controls **ordering within a workflow run** — i.e. a gate waits for its prerequisite jobs before it starts. That is a separate concern from **branch protection**, which is what blocks the GitHub merge button on a PR. A repo admin must explicitly configure each PR status check as required in GitHub's branch-protection settings (see [Branch-protection configuration](#branch-protection-configuration) below). Without that step, the gates run and report results but cannot block a merge.

---

## Gates

### Binary size gate

**Job name in CI:** `Binary size gate`
**YAML key:** `binary-size-gate`
**Trigger:** `needs: [check]` — runs after the `check` job succeeds.

**Purpose:** enforces two independent size constraints on the release binary, and records the measured size as a workflow artifact:

1. **Recorded size artifact** — every CI run writes `size.txt` (single integer, bytes, with trailing newline — same format as [`benchmarks/binary-size-baseline.txt`](../benchmarks/binary-size-baseline.txt)) and uploads it as the `ghost-complete-size` workflow artifact. PR reviewers and the release author can download the artifact from the run summary page to see the exact byte count without re-running the job. The size is computed with `wc -c` rather than `du -b` because BSD `du` on `macos-latest` runners has no `-b` flag.
2. **Absolute ceiling (110 MB)** — the binary must not exceed 110 MB. Raising it requires an explicit plan amendment. The ceiling moved from 30 MB to 110 MB in `ux-8` to admit the AWS completion spec; zstd-compressing embedded specs (a separate plan) is the principled reclaim path that should drop the binary back near the original ceiling.
3. **Per-phase delta budget (default +2 MB, label override +5 MB)** — the binary must not have grown by more than the delta budget since the size recorded in [`benchmarks/binary-size-baseline.txt`](../benchmarks/binary-size-baseline.txt). The default budget is `PHASE_BUDGET` (`2MB`). On `pull_request` events, applying the **`binary-size-allow-delta`** label raises the budget to `LABEL_OVERRIDE_BUDGET` (`5MB`) for that PR only — the gate's "Pick delta budget" step inspects `github.event.pull_request.labels` and emits the override decision in the job log. Pushes to trunk branches (`master` or `main`) always use the strict 2 MB budget (no PR labels to read). The label is the explicit acknowledgement that a PR is expected to grow the binary; without it, growth >2 MB fails the gate. Update the baseline file in the same PR (see "Baseline maintenance" below) once the change is justified — the override is for the merge, not for permanent tolerance. Create the label one-time via `gh label create binary-size-allow-delta --description "Raise binary-size delta budget from 2MB to 5MB for this PR" --color FBCA04`; the gate fails closed (strict 2 MB) if the label is missing.

**Stripping note.** The release profile sets `strip = "symbols"`. The size measurement in this gate reflects the stripped binary, and [`benchmarks/binary-size-baseline.txt`](../benchmarks/binary-size-baseline.txt) is captured from the same stripped build — baseline and live measurement use the same shape. Toggling `strip` off would invalidate the baseline.

**Failure modes:**

- Absolute ceiling failure: binary size exceeds 110 MB.
- Delta budget failure: binary grew by more than the selected budget (2 MB strict / 5 MB with label) since the baseline was recorded.

**Status today:** production-live and **passing**. The binary-size intervention (minified embedded specs + stripped `js_source`) dropped the binary to ~28.4 MB, under the original 30 MB ceiling. The `ux-8` AWS spec restoration brought the binary to ~102 MB; the ceiling moved to 110 MB to match plus headroom. The artifact upload + label override were added in `ux-9b` Phase 4. Ready to add to branch protection.

**How to debug locally:**

```bash
cargo build --release
scripts/check-binary-size.sh --absolute-max 110MB
scripts/check-binary-size.sh --delta-max 2MB
# Equivalent of the artifact upload step:
wc -c < target/release/ghost-complete | tr -d ' ' > size.txt
```

For exploratory size attribution (which crate / function dominates the binary), run `cargo bloat`:

```bash
cargo install cargo-bloat                    # one-time
cargo bloat --release --crates                # crate-level breakdown
cargo bloat --release -n 30                   # top 30 functions by size
cargo bloat --release --filter '^aws'         # focus on a path prefix
```

`cargo bloat` is a debugging tool, **not a CI gate** — its codegen-unit estimates are too coarse for a hard fail. Use it locally when investigating an unexpected binary growth flagged by the delta gate.

**Baseline maintenance:** when a change legitimately grows the binary, update the baseline file. The script accepts both formats (bare integer or `du -b` output) but the canonical form for macOS-latest CI runners is the bare-integer `wc -c` output:

```bash
wc -c < target/release/ghost-complete | tr -d ' ' > benchmarks/binary-size-baseline.txt
# or equivalently on a GNU coreutils machine:
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

### Corpus hash determinism gates

**Job names in CI:** `Corpus hash determinism (fig-converter)`, `Corpus hash determinism (full corpus)`
**YAML keys:** `corpus-hash-gate`, `corpus-hash-gate-master`
**Trigger:** the PR job runs after `check` on `pull_request` events and uses a path filter so the expensive converter steps only run when `tools/fig-converter/**` changes. The full-corpus job runs after `check` only on pushes to `master` or `main`; it is not a PR check.

**Purpose:** verifies that deterministic fig-converter output is reproducible. The PR gate runs `check-corpus-hash.mjs` twice over a representative spec subset, then runs the fig-converter package test suite, including `src/determinism.test.js`. The trunk-push gate runs the same hash check across the full corpus. Both hash checks depend on the converter exiting non-zero for any per-spec conversion failure, so CI cannot accept two matching hashes from a partial or empty corpus.

**Failure modes:**

- Converter failure: any requested spec fails to load or convert, including missing specs and worker-batch failures.
- Hash mismatch: two deterministic runs over the same requested corpus produce different `corpus-hash.txt` values.
- Hash file failure: `corpus-hash.txt` is missing or unreadable after a converter run.

**Status today:** production-live. `Corpus hash determinism (fig-converter)` is the PR check to add to branch protection. `Corpus hash determinism (full corpus)` is a trunk-push safety net and should not be added as a PR branch-protection check.

**How to debug locally:**

```bash
node tools/fig-converter/scripts/check-corpus-hash.mjs --specs git,docker,kubectl,brew,cargo,make,npm,ls
node tools/fig-converter/scripts/check-corpus-hash.mjs
cd tools/fig-converter && npm test
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

### Coverage regression

**Job name in CI:** `Coverage regression`
**YAML key:** `coverage-regression`
**Trigger:** `needs: [check]` — runs after the `check` job succeeds. Wired with `continue-on-error: true` pending promotion to a hard gate.

**Purpose:** fails when the live `requires_js_generators_unsupported` count from `ghost-complete status --json` rises above the latest `docs/coverage-baseline.json` row by more than the configured tolerance (default: 0), or when any command is reported `commands_nonfunctional > 0`. Catches regressions where:

- a converter change drops `js_runtime` metadata from generators that previously dispatched, or
- a spec edit moves a previously-supported generator into the unsupported bucket, or
- a malformed or unreadable spec fails to load into the runtime store.

**Failure modes:**

- Hard fail (exit 1): `requires_js_generators_unsupported > baseline + tolerance`. The error message names the delta and points at `docs/coverage-baseline.json` for the refresh path.
- Hard fail (exit 1): `commands_nonfunctional > 0`. Always a defect — independent of baseline.
- Soft warning (`::warning::` annotation, exit 0): the unsupported count rose by 1..=tolerance. Surfaces in the PR checks panel without blocking the merge.

**Status today:** wired into CI as **non-blocking** (`continue-on-error: true`). The gate runs and reports results, but a failing run does not fail the workflow. Promotion to a blocking status check is a separate follow-up; the maintainer who promotes it is responsible for first refreshing the baseline if the live numbers have drifted.

**How to debug locally:**

```bash
cargo build --release
scripts/check-coverage-regression.sh                                # default tolerance 0
scripts/check-coverage-regression.sh --tolerance 100                # accept up to 100 new unsupported generators
scripts/check-coverage-regression.sh --status-json /tmp/status.json # bypass binary invocation
COVERAGE_REGRESSION_TOLERANCE=50 scripts/check-coverage-regression.sh
bash scripts/check-coverage-regression.test.sh                      # self-tests
```

**Baseline refresh:** when an intentional change increases the unsupported count or modifies coverage, append a new release row to [`docs/coverage-baseline.json`](./coverage-baseline.json) capturing the new floor. The script reads `releases[-1]` (the latest entry) so an append-only history preserves prior numbers for trend reporting in `ghost-complete status` while updating the gate's baseline.

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
| `Corpus hash determinism (fig-converter)` | Ready to add. This is the PR corpus-hash check. |
| `Corpus hash determinism (full corpus)` | Push-to-trunk safety net only. Do not add as a PR branch-protection check. |
| `Coverage baseline drift` | Informational only (non-blocking warning). Do not add to branch protection. |
| `Coverage regression` | Wired as `continue-on-error: true` during the initial rollout. Promotion to a hard gate is a separate follow-up; the maintainer who promotes it is responsible for refreshing the baseline first. |

> **Note on job names vs. YAML keys:** GitHub branch protection displays the `name:` field of each job, not the YAML key. `Binary size gate` (the name) corresponds to `binary-size-gate` (the key). Using the YAML key in the search box will not match.

---

## FAQ

**"Why is the ceiling 110 MB?"**

The 30 MB ceiling was set during the requires-js-specs initiative as the target the binary needed to reach after specs were trimmed. The intervention (minified embedded specs + stripped `js_source`) brought the release binary to ~28.4 MB, under budget. In `ux-8` the AWS spec was restored: 409 inlined service sub-specs (upstream ships 418 `.js` files but the top-level `aws.js` only references 408 via `loadSpec` — 9 deprecated services are unreferenced) carrying ~28 MB of upstream description text, which `include_str!` roundtrips into ~2× `__const` data. The release binary moved to ~102 MB; the ceiling moved to 110 MB to match plus ~8% headroom. The delta budget (`PHASE_BUDGET=2MB`) still handles the near-term constraint — "don't grow from the current baseline". These are two independent checks; both must pass. zstd-compressing embedded specs is tracked as a follow-on plan; landing it should let the ceiling drop back near the original 30 MB level.

**"When should I apply the `binary-size-allow-delta` label?"**

Only when a PR is *expected* to grow the binary by more than 2 MB and the growth is reviewed and justified — for example, restoring a previously-pruned spec, adding a new built-in provider with substantial static data, or opting into a new compile-time feature. The label raises the delta gate from 2 MB to 5 MB for that PR. The 110 MB absolute ceiling still applies; the label cannot override it. Pushes to trunk branches (`master` or `main`) always use the strict 2 MB budget (no PR labels to read), so the label only affects the PR build that introduces the change. Update [`benchmarks/binary-size-baseline.txt`](../benchmarks/binary-size-baseline.txt) in the same PR — the override exists to admit a single justified jump, not to live with permanent slack.

**"Can I skip a gate on a specific PR?"**

No. Required status checks are all-or-nothing. For a legitimate one-off exception (e.g. an unavoidable binary size overrun covered by a plan amendment), the admin must:

1. Temporarily remove the specific status check from branch protection.
2. Merge the PR.
3. Re-add the status check immediately after.

This is an emergency procedure. Document the exception in the PR description and in the relevant plan file.

**"Why is coverage baseline drift non-failing?"**

A stale baseline is a documentation-freshness signal, not a correctness problem. Blocking merges because `docs/coverage-baseline.json` is old would halt unrelated work whenever the maintainer forgets to refresh stats. The warning annotation surfaces the issue in the PR checks panel without stopping the line.
