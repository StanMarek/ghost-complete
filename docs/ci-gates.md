# CI Gates

## Overview

Five CI gates live in `.github/workflows/ci.yml`. Four were introduced in Phase 0.5 (binary size, snapshot diff, fig-converter oracle, bench regression); a fifth (coverage baseline drift) was added in Phase 4. The gates are wired via `needs:` dependencies, which controls **ordering within a workflow run** — i.e. a gate waits for its prerequisite jobs before it starts. That is a separate concern from **branch protection**, which is what blocks the GitHub merge button on a PR. A repo admin must explicitly configure each status check as required in GitHub's branch-protection settings (see [Branch-protection configuration](#branch-protection-configuration) below). Without that step, the gates run and report results but cannot block a merge.

---

## Gates

### Binary size gate

**Job name in CI:** `Binary size gate`
**YAML key:** `binary-size-gate`
**Trigger:** `needs: [check]` — runs after the `check` job succeeds.

**Purpose:** enforces two independent size constraints on the release binary:

1. **Absolute ceiling (30 MB)** — the binary must not exceed 30 MB. This limit is fixed. Raising it requires an explicit plan amendment.
2. **Per-phase delta budget (default +2 MB)** — the binary must not have grown by more than `PHASE_BUDGET` (set to `2MB` in the job env) since the size recorded in [`benchmarks/binary-size-baseline.txt`](../benchmarks/binary-size-baseline.txt).

**Failure modes:**

- Absolute ceiling failure: binary size exceeds 30 MB.
- Delta budget failure: binary grew by more than `PHASE_BUDGET` since the baseline was recorded.

**Status today:** production-live. Currently **FAILS** on the absolute-ceiling check (binary ~47 MB, target 30 MB). This is intentional per the plan — the gate stays red until Phase 4 (G.8) brings the binary under budget. Admin decision pending: add to branch protection AFTER the binary reaches ≤30 MB.

**How to debug locally:**

```bash
cargo build --release
scripts/check-binary-size.sh --absolute-max 30MB
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

### Bench regression gate

**Job name in CI:** `Bench regression gate`
**YAML key:** `bench-regression`
**Trigger:** `needs: [check]`

**Purpose:** fails if any Criterion benchmark group regresses more than 10% relative to the saved baseline.

**Failure modes:** a benchmark's mean is more than 10% slower than its recorded baseline value.

**Status today:** production-live. Compares against [`benchmarks/baseline-pre-js-port.json`](../benchmarks/baseline-pre-js-port.json). Fails if any group regresses >10%.

**How to debug locally:**

```bash
cargo bench
scripts/check-bench.sh --threshold 10
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
| `Bench regression gate` | Ready to add. |
| `Binary size gate` | **NOT READY.** Do not add until Phase 4 G.8 gets the binary ≤30 MB. Adding now would block every PR. |
| `Coverage baseline drift` | Informational only (non-blocking warning). Do not add to branch protection. |

> **Note on job names vs. YAML keys:** GitHub branch protection displays the `name:` field of each job, not the YAML key. `Binary size gate` (the name) corresponds to `binary-size-gate` (the key). Using the YAML key in the search box will not match.

---

## FAQ

**"Why is the 30 MB ceiling lower than the current binary (~47 MB)?"**

The 30 MB target is where the requires-js-specs work is expected to land after specs are removed from the embedded binary. The ceiling is set now to make the goal explicit: the gate intentionally fails red until the binary is shrunk. The delta budget (`PHASE_BUDGET=2MB`) handles the near-term constraint — "don't grow from the current baseline". These are two independent checks; both must pass.

**"Can I skip a gate on a specific PR?"**

No. Required status checks are all-or-nothing. For a legitimate one-off exception (e.g. an unavoidable binary size overrun covered by a plan amendment), the admin must:

1. Temporarily remove the specific status check from branch protection.
2. Merge the PR.
3. Re-add the status check immediately after.

This is an emergency procedure. Document the exception in the PR description and in the relevant plan file.

**"Why is coverage baseline drift non-failing?"**

A stale baseline is a documentation-freshness signal, not a correctness problem. Blocking merges because `docs/coverage-baseline.json` is old would halt unrelated work whenever the maintainer forgets to refresh stats. The warning annotation surfaces the issue in the PR checks panel without stopping the line.
