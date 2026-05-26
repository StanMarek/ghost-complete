# CI Gates

## Overview

Eight CI gates live in `.github/workflows/ci.yml`: binary size, snapshot diff, fig-converter oracle, corpus hash determinism (fig-converter PR subset), corpus hash determinism (full corpus on trunk pushes), coverage baseline drift, coverage regression, and zsh/ZLE shell smoke. Two more gates live outside `ci.yml`: `cargo-deny check` runs alongside `cargo audit` in [`.github/workflows/audit.yml`](../.github/workflows/audit.yml) (Cargo manifest / lockfile changes and a weekly cron), and `Smoke packaged artifacts` runs in [`.github/workflows/release.yml`](../.github/workflows/release.yml) on every release tag — those two are documented under [Audit workflow](#audit-workflow) and [Release-only gates](#release-only-gates) below. Benchmark-regression checking is intentionally **not** a CI gate — it is run manually at release time (see [Release-time benchmark checking](#release-time-benchmark-checking) below). The gates are wired via `needs:` dependencies, which controls **ordering within a workflow run** — i.e. a gate waits for its prerequisite jobs before it starts. That is a separate concern from **branch protection**, which is what blocks the GitHub merge button on a PR. A repo admin must explicitly configure each PR status check as required in GitHub's branch-protection settings (see [Branch-protection configuration](#branch-protection-configuration) below). Without that step, the gates run and report results but cannot block a merge.

---

## Gates

### Binary size gate

**Job name in CI:** `Binary size gate`
**YAML key:** `binary-size-gate`
**Trigger:** `needs: [check]` — runs after the `check` job succeeds.

**Purpose:** enforces two independent size constraints on the release binary, and records the measured size as a workflow artifact:

1. **Recorded size artifact** — every CI run writes `size.txt` (single integer, bytes, with trailing newline — same format as [`benchmarks/binary-size-baseline.txt`](../benchmarks/binary-size-baseline.txt)) and uploads it as the `ghost-complete-size` workflow artifact. PR reviewers and the release author can download the artifact from the run summary page to see the exact byte count without re-running the job. The size is computed with `wc -c` rather than `du -b` because BSD `du` on `macos-latest` runners has no `-b` flag.
2. **Absolute ceiling (110 MB)** — the binary must not exceed 110 MB. Raising it requires an explicit plan amendment. The ceiling moved from 30 MB to 110 MB in `ux-8` to admit the AWS completion spec; the ux-12b zstd compression work reclaimed the embedded-corpus growth while keeping the ceiling unchanged.
3. **Per-phase delta budget (default +2 MB, label override +5 MB)** — the binary must not have grown by more than the delta budget since the size recorded in [`benchmarks/binary-size-baseline.txt`](../benchmarks/binary-size-baseline.txt). The default budget is `PHASE_BUDGET` (`2MB`). On `pull_request` events, applying the **`binary-size-allow-delta`** label raises the budget to `LABEL_OVERRIDE_BUDGET` (`5MB`) for that PR only — the gate's "Pick delta budget" step inspects `github.event.pull_request.labels` and emits the override decision in the job log. Label add/remove events rerun the PR workflow, so adding the override after a failed size gate is enough to re-evaluate the current label set. Pushes to trunk branches (`master` or `main`) always use the strict 2 MB budget (no PR labels to read). The label is the explicit acknowledgement that a PR is expected to grow the binary; without it, growth >2 MB fails the gate. Update the baseline file in the same PR (see "Baseline maintenance" below) once the change is justified — the override is for the merge, not for permanent tolerance. Create the label one-time via `gh label create binary-size-allow-delta --description "Raise binary-size delta budget from 2MB to 5MB for this PR" --color FBCA04`; the gate fails closed (strict 2 MB) if the label is missing.

**Stripping note.** The release profile sets `strip = "symbols"`. The size measurement in this gate reflects the stripped binary, and [`benchmarks/binary-size-baseline.txt`](../benchmarks/binary-size-baseline.txt) is captured from the same stripped build — baseline and live measurement use the same shape. Toggling `strip` off would invalidate the baseline.

**Failure modes:**

- Absolute ceiling failure: binary size exceeds 110 MB.
- Delta budget failure: binary grew by more than the selected budget (2 MB strict / 5 MB with label) since the baseline was recorded.

**Status today:** production-live and **passing**. The binary-size baseline is 21,383,696 bytes (~20.4 MiB, ~21.4 MB), under the original 30 MB ceiling and well below the 110 MB absolute ceiling. The `ux-8` AWS spec restoration previously brought the binary to ~102 MB; ux-12b zstd compression reclaimed that embedded-corpus growth. The artifact upload + label override were added in `ux-9b` Phase 4. Ready to add to branch protection.

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

**Status today:** production-live. `specs/__snapshots__/` is populated (711 snapshots). `scripts/check-snapshots.sh` runs on every CI build.

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
**Trigger:** the PR job runs after `check` on `pull_request` events and uses a path filter so the expensive converter steps only run when `tools/fig-converter/**` or the CI workflow changes. The full-corpus job runs after `check` only on pushes to `master` or `main`; it is not a PR check.

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
**Trigger:** `needs: [check]` — runs after the `check` job succeeds. Blocking gate.

**Purpose:** fails when the live `requires_js_generators_unsupported` count from `ghost-complete status --json` rises above the latest `docs/coverage-baseline.json` row by more than the configured tolerance (default: 0), or when any command is reported `commands_nonfunctional > 0`. Catches regressions where:

- a converter change drops `js_runtime` metadata from generators that previously dispatched, or
- a spec edit moves a previously-supported generator into the unsupported bucket, or
- a malformed or unreadable spec fails to load into the runtime store.

**Failure modes:**

- Hard fail (exit 1): `requires_js_generators_unsupported > baseline + tolerance`. The error message names the delta and points at `docs/coverage-baseline.json` for the refresh path.
- Hard fail (exit 1): `commands_nonfunctional > 0`. Always a defect — independent of baseline.
- Soft warning (`::warning::` annotation, exit 0): the unsupported count rose by 1..=tolerance. Surfaces in the PR checks panel without blocking the merge.

**Status today:** wired into CI as **blocking**. A failing run fails the workflow. The baseline was refreshed for `0.17-rc` when the gate was promoted from `continue-on-error: true`. Ready to add to branch protection.

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

### Zsh/ZLE shell smoke

**Job name in CI:** `zsh/ZLE shell smoke`
**YAML key:** `zsh-zle-smoke`
**Trigger:** `needs: [check]` — runs after the `check` job succeeds. Blocking gate.

**Purpose:** exercises the production zsh shell integration (`shell/ghost-complete.zsh`) under a real `/bin/zsh --no-rcs` and asserts that the ZLE widget `_gc_report_buffer` emits OSC 7772 frames with the correct percent-encoding. Catches regressions in the encoder for characters that would otherwise corrupt a frame mid-stream (semicolons, BEL `0x07`, ESC `0x1B`, literal `%`), validates UTF-8 round-trip, exercises the OSC 7 path encoder, and verifies the `GHOST_COMPLETE_ACTIVE` gate guard turns the widget into a no-op outside the proxy. The matching runtime parser path lives in `gc-parser` and is unit-tested in Rust; this gate validates the shell-side producer end-to-end against a real zsh so a shell-script regression cannot ship undetected by `cargo test`.

**Failure modes:**

- Encoder regression: a frame is missing percent-encoding for one of the documented byte classes.
- Gate guard regression: `_gc_report_buffer` emits OSC 7772 when `GHOST_COMPLETE_ACTIVE` is unset.
- Environment failure: `zsh` is not on `PATH`, or `shell/ghost-complete.zsh` is missing.

**Status today:** production-live. The check runs on every PR and trunk push. Ready to add to branch protection.

**How to debug locally:**

```bash
scripts/check-zsh-zle-smoke.sh
```

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

## Audit workflow

The `audit` workflow ([`.github/workflows/audit.yml`](../.github/workflows/audit.yml)) runs two dependency-policy checks on every Cargo manifest / lockfile change and on a weekly Monday cron. Both checks are blocking — a failure fails the workflow.

> Both checks live in the single `cargo audit` job in `audit.yml`; failure of either step fails the job.

### cargo audit step

**Action:** `rustsec/audit-check@v2`.
**Trigger:** Cargo.toml or Cargo.lock changes (PR or push to `master`), changes to `audit.yml` itself, and the weekly cron (`0 12 * * 1`).

**Purpose:** scans the resolved dependency graph against the RustSec advisory database. Flags known vulnerabilities. Posts a GitHub Check annotation with the affected crates and advisory IDs.

**Failure modes:** any unyanked advisory at `error` severity (per `audit-check`'s defaults) against a crate in `Cargo.lock`.

### cargo-deny step

**Step name:** `Run cargo-deny`, using `EmbarkStudios/cargo-deny-action@v2` with `command: check` and `arguments: --all-features`.
**Trigger:** same trigger set as the `cargo audit` step (they share the `cargo audit` job).

**Purpose:** enforces the policy in [`deny.toml`](../deny.toml) — license allow/deny lists, banned crates, source allowlist, and duplicate-version policy. `cargo deny check` runs the full check matrix (`advisories`, `bans`, `licenses`, `sources`).

**Failure modes:**

- Disallowed license: a dependency carries a license outside the allow list in `deny.toml`.
- Banned crate: a dependency matches a `[bans] deny` entry.
- Untrusted source: a dependency comes from a registry/git source outside the `[sources]` allowlist.
- Duplicate-version policy: `multiple-versions` is currently `warn` while we ladder up to `deny`; future tightening will turn this into a hard fail.

**Status today:** production-live. Should not be added as a branch-protection check on PRs unless the PR touches Cargo manifests (the workflow's path filter already gates it); branch protection cannot express "required only when this path changed".

**How to debug locally:**

```bash
cargo audit                              # one-time: cargo install cargo-audit
cargo deny check                         # one-time: cargo install cargo-deny
cargo deny check --all-features
```

---

## Release-only gates

The `release` workflow ([`.github/workflows/release.yml`](../.github/workflows/release.yml)) runs on `push` of any version-shaped tag. It hosts one smoke gate that is **not** part of CI and only ever runs at release time.

### Smoke packaged artifacts

**Job name in release workflow:** `Smoke packaged artifacts`
**YAML key:** `artifact-smoke`
**Trigger:** `needs: [build-local-artifacts, build-global-artifacts]` inside `release.yml`. Runs after every successful artifact build and gates the downstream `host` job (which is what actually publishes the GitHub Release).

**Purpose:** refuses to publish a release whose packaged macOS artifact can't execute `--version`, `--help`, `validate-specs --json`, `status --json`, or `install --dry-run` cleanly. Native-arch binaries (arm64 on the `macos-latest` runner) execute end-to-end against an isolated `HOME` and `cwd` so the test reflects the binary's embedded spec corpus only — not anything that would otherwise leak in from the runner's filesystem. Cross-arch binaries (x86_64) get a structural smoke (extract + `file(1)` arch check) since the runner can't execute them; the script warns loudly if arch detection is inconclusive.

**Failure modes:**

- No executable `ghost-complete` extracted from the archive.
- `validate-specs --json` or `status --json` returns fewer than 10 fully-functional specs (regression in the embedded corpus).
- `install --dry-run` writes to the isolated `HOME` (a real side effect during what is supposed to be a dry run).
- Arch detection inconclusive (WARN on stderr; structural-only smoke for that artifact).
- Zero artifacts of the expected shape found at all (driver loop in the workflow step fails closed).

**Status today:** production-live. Gates the `host` job in `release.yml`; nothing publishes without it.

**How to debug locally:**

```bash
cargo build --release
# Approximate the packaged path: build, tar, run the smoke script against
# the archive. The smoke script itself is the canonical reproducer:
scripts/check-release-artifact-smoke.sh <path/to/ghost-complete-*-apple-darwin.tar.{gz,xz}>
```

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
| `Coverage regression` | Ready to add. Blocking gate (refresh `docs/coverage-baseline.json` when tightening tolerance). |
| `zsh/ZLE shell smoke` | Ready to add. |
| `cargo audit` (audit workflow — covers both `cargo audit` and `cargo deny check` steps) | Path-filtered to Cargo manifest / lockfile changes. Branch protection cannot express "required only when this path changed"; leave unenforced and let the workflow's own path filter gate it. |
| `Smoke packaged artifacts` (release workflow) | Release-only — not a PR check. Gates the `host` job inside `release.yml`; cannot meaningfully be added to PR branch protection. |

> **Note on job names vs. YAML keys:** GitHub branch protection displays the `name:` field of each job, not the YAML key. `Binary size gate` (the name) corresponds to `binary-size-gate` (the key). Using the YAML key in the search box will not match.

---

## FAQ

**"Why is the ceiling 110 MB?"**

The 30 MB ceiling was set during the requires-js-specs initiative as the target the binary needed to reach after specs were trimmed. The first intervention (minified embedded specs + stripped `js_source`) brought the release binary under that budget. In `ux-8` the AWS spec was restored: 409 inlined service sub-specs (upstream ships 418 `.js` files but the top-level `aws.js` only references 408 via `loadSpec` — 9 deprecated services are unreferenced) carrying ~28 MB of upstream description text, which the old `include_str!` path round-tripped into ~2× `__const` data. The release binary moved to ~102 MB; the ceiling moved to 110 MB to match plus ~8% headroom. The ux-12b zstd archive replaced that raw embedded JSON path, and the current binary-size baseline is 21,383,696 bytes. The delta budget (`PHASE_BUDGET=2MB`) still handles the near-term constraint — "don't grow from the current baseline". These are two independent checks; both must pass.

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
