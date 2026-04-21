# CI Gates — Phase 0.5

## Overview

Phase 0.5 introduces four CI gates to prevent regressions during the requires-js-specs work. The gates enforce binary size budgets, spec snapshot consistency, fig-converter correctness, and benchmark performance. They are wired into `.github/workflows/ci.yml` via `needs:` dependencies, which controls **ordering within a workflow run** — i.e. a gate waits for its prerequisite jobs before it starts. That is a separate concern from **branch protection**, which is what blocks the GitHub merge button on a PR. A repo admin must explicitly configure the four status checks as required in GitHub's branch-protection settings (see [Branch-protection configuration](#branch-protection-configuration) below). Without that step, the gates run and report results but cannot block a merge.

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

- Absolute ceiling failure: binary size exceeds 30 MB. CI shows red. This is intentional — the gate is meant to stay red until the binary is actually shrunk to the target, so the goal is not quietly forgotten.
- Delta budget failure: binary grew by more than `PHASE_BUDGET` since the baseline was recorded.

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

**Status today:** `specs/__snapshots__/` does not exist yet — it lands in session D of Phase 0. Until then, `scripts/check-snapshots.sh` detects the missing directory, emits a warning, and exits 0. The gate does not block anything.

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

**Status today:** the `oracle:changed` npm script in `tools/fig-converter/package.json` is a stub that exits 0 with "not yet implemented". Real oracle output lands in Phase 0 (Session A downstream).

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

**Status today:** `scripts/check-bench.sh` is a stub. The baseline file (`benchmarks/baseline-pre-js-port.md`) does not exist yet — it lands in Phase 0.4. Until then the script exits 0 with a warning. The gate does not block anything.

**How to debug locally:**

```bash
cargo bench
scripts/check-bench.sh --threshold 10
```

---

## Branch-protection configuration

These steps require repo admin access. Without them the gates run but **do not block merge**.

1. Go to <https://github.com/StanMarek/ghost-complete/settings/branches>.
2. Edit the branch protection rule for `master`, or create one if none exists.
3. Enable **"Require status checks to pass before merging"**.
4. In the status check search box, add the following checks by their **exact names** (these are the human-readable `name:` values from the CI YAML, not the YAML job keys):
   - `Binary size gate`
   - `Snapshot diff gate`
   - `Oracle gate (fig-converter)`
   - `Bench regression gate`
5. Save the rule.

These four checks are added **alongside** any existing required checks (e.g. `Check`, `Test`, `Clippy`, `Format`, `MSRV (1.86)`, `Linux tripwire (compile-check only)`). They replace nothing.

> **Note on job names vs. YAML keys:** GitHub branch protection displays the `name:` field of each job, not the YAML key. `Binary size gate` (the name) corresponds to `binary-size-gate` (the key). Using the YAML key in the search box will not match.

---

## When gates turn into real enforcement

Three of the four gates are currently stubs that exit 0 when their prerequisite artifacts are absent:

| Gate | Activates when | Lands in |
|------|---------------|----------|
| `Snapshot diff gate` | `specs/__snapshots__/` directory is created | Phase 0, session D |
| `Oracle gate (fig-converter)` | Real `oracle:changed` npm script replaces the stub | Phase 0, session A |
| `Bench regression gate` | `benchmarks/baseline-pre-js-port.md` is committed | Phase 0.4 |

Once all three artifacts exist, the gates enforce for real. The `Binary size gate` is already enforcing the delta budget now (as long as `benchmarks/binary-size-baseline.txt` exists).

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
