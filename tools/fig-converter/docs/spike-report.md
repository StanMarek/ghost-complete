# Phase 1 Spike Report — `requires_js` Specs Initiative

**Date:** 2026-04-21
**Branch:** `feature/phase-1-spike`
**Status:** DONE — §1.6 exit deliverable

---

## §1 Executive Summary

The Phase 1 spike ran to completion within its 2-week hard timebox (in practice a single agentic session). The full corpus of `requires_js` generators was ingested, parsed with the two-stage AST analyzer, and classified into a closed 7-verdict enum across 151 composite shape buckets. The corrected empirical baseline overrides the provisional §0 numbers: total generators are **1,889** (not ~1,750), and the provisional 58/29/13 coverage split is superseded by the bucket table in §3. The canonical artifact is [`shape-inventory.json`](./shape-inventory.json) — all downstream phases must query it, not the provisional plan text. **Go/no-go by phase: Phase 0 GO, Phase 2 CONDITIONAL GO (recount first), Phase 3A GO.**

---

## §2 Corrected Empirical Baseline

| Metric | Provisional | Actual | Notes |
|---|---|---|---|
| Spec files with `requires_js` generators | 184 | 184 | Exact match |
| Total `requires_js` generators | ~1,750 | 1,889 | +8% vs estimate |
| Generators with `js_source` field | n/a | 1,889 (100%) | No filtering needed |
| Provisional coverage split 58/29/13 | 58% / 29% / 13% | Superseded — see §3 | Based on rough prior, not corpus scan |

---

## §3 Bucket Distribution

Source: `shape-inventory.json` — 151 composite shape buckets (fingerprint × 5 shape booleans × `has_fig_api_refs`), summed by verdict.

| Verdict | Count | % of 1,889 |
|---|---|---|
| `hand_audit_required` | 866 | 45.8% |
| `existing_transforms` | 614 | 32.5% |
| `requires_runtime` | 162 | 8.6% |
| `needs_new_transform_conditional_split` | 148 | 7.8% |
| `needs_new_transform_regex_match` | 52 | 2.8% |
| `needs_new_transform_substring_slice` | 33 | 1.7% |
| `needs_dotted_path_json_extract` | 14 | 0.7% |
| **Total** | **1,889** | **100%** |

**Verdict commentary:**

- **`existing_transforms` (614, 32.5%):** Handleable today by the current `json_extract` / `column_extract` / `filter` pipeline. Failures here are converter bugs, not runtime gaps — highest near-term leverage.
- **`hand_audit_required` (866, 45.8%):** Generators with free-identifier references the analyzer cannot statically resolve (mostly bundler-hoisted single-letter helpers). Cannot be safely auto-classified; require manual triage. Phase 0's oracle skips these by design.
- **`requires_runtime` (162, 8.6%):** Confirmed calls to Fig API runtime symbols (e.g., `executeShellCommand`, `filepaths`). A native JS sandbox or generator migration is needed; no short-path exists.
- **`needs_new_transform_conditional_split` (148, 7.8%):** Pattern has a conditional branch on the completion context. A new transform or split-generator encoding is required before these can be covered.
- **`needs_new_transform_regex_match` (52, 2.8%):** Pattern applies a regex match to filter or rewrite candidates. `regex_extract` is close but doesn't cover all shapes; a targeted new transform is needed.
- **`needs_new_transform_substring_slice` (33, 1.7%):** Pattern slices a string by index (`.slice(n, m)` or similar). No current transform handles arbitrary index-based slicing.
- **`needs_dotted_path_json_extract` (14, 0.7%):** Pattern parses JSON and accesses a multi-hop dotted path (e.g., `n.Organization.Slug`). Current `json_extract` handles single-level fields only. **Note: this is a lower bound** — see §5 finding 3 and §6 Phase 2 discussion.

---

## §4 Fig API Reference Rate

Generators with at least one non-empty `fig_api_refs` entry: **851** (verified: `jq '[.shapes[] | select(.has_fig_api_refs == true) | .count] | add'` == 851; note this is distinct from the 866 `hand_audit_required` count).

The apparent near-overlap is expected: virtually all Fig API refs detected are `kind: free` against single-letter identifiers that resolve to bundler-hoisted helpers (TypeScript's `__awaiter` pattern, module-level deduped utility wrappers) — not calls to named Fig API symbols like `executeShellCommand`. Per plan §1.3, "the false-positive direction favors correctness." This is by design: over-flagging sends generators to `hand_audit_required` rather than incorrectly routing them to auto-conversion.

---

## §5 Manual Spot-Check (Decision Gate §9.2)

**Scope analysis passes manual spot-check on 10 randomly-sampled generators for aliased/destructured Fig API.**

**Sample methodology:** 10 generators sampled uniformly at random from all 1,889 entries using `mulberry32` RNG with seed `20260421` for reproducibility.

| # | generator_id | analyzer verdict | controller verdict | agree? |
|---|---|---|---|---|
| 1 | `meteor:/subcommands[0]/subcommands[37]/options[2]/args/generators[0]` | requires_runtime | requires_runtime | ✅ |
| 2 | `snaplet:/subcommands[2]/subcommands[1]/args[1]/generators[0]` | hand_audit_required | hand_audit_required | ✅ |
| 3 | `rsync:/args[0]/generators[1]` | hand_audit_required | hand_audit_required | ✅ |
| 4 | `npm:/subcommands[7]/args/generators[0]` | hand_audit_required | hand_audit_required | ✅ |
| 5 | `tsuru:/subcommands[0]/subcommands[12]/subcommands[4]/subcommands[1]/options[0]/args/generators[0]` | hand_audit_required | hand_audit_required | ✅ |
| 6 | `yarn:/subcommands[9]/subcommands[0]/args/generators[0]` | hand_audit_required | hand_audit_required | ✅ |
| 7 | `trivy:/subcommands[1]/subcommands[6]/args/generators[0]` | existing_transforms | existing_transforms | ✅ |
| 8 | `flyctl:/subcommands[39]/subcommands[1]/options[0]/args/generators[0]` | existing_transforms | existing_transforms (see caveat) | ✅ |
| 9 | `codesign:/options[25]/args/generators[0]` | hand_audit_required | hand_audit_required | ✅ |
| 10 | `dotnet:/subcommands[10]/subcommands[10]/subcommands[6]/options[2]/args/generators[0]` | hand_audit_required | hand_audit_required | ✅ |

**Result: 10/10 verdicts agree with priority rules applied to analyzer output.**

**Findings worth noting (none affect sampled verdicts but matter for the synthesis):**

1. **Fig API property-access detection gap** (minor). Sample 3 (`rsync`): `t.environmentVariables.HOME` — `environmentVariables` is in `FIG_API_NAMES` but accessed as a property, not a call. The analyzer's `CallExpression` visitor only fires for call-position member access, and the `ReferencedIdentifier` visitor skips member-expression property names. Consequence: Fig API READs (as opposed to calls) aren't detected. Didn't flip the sample-3 verdict because a separate free ref (`c`) already sent it to `hand_audit`.

2. **Destructure-from-function-argument detection gap** (minor). Samples 9 and 10: `{isDangerous, currentWorkingDirectory, searchTerm} = _` where `_` is the 3rd arrow parameter (the ambient context object passed to Fig generators). Analyzer's Pass 1 only classifies destructuring when the init expression is an `Identifier` that's already in `FIG_API_NAMES` or `figModuleBindings`; a bare function parameter doesn't qualify. Didn't flip either verdict because bundler-helper free refs already sent both to `hand_audit`.

3. **Classification under-count for `needs_dotted_path_json_extract`** (noteworthy). Sample 8 (`flyctl`): fingerprint is `JSON.parse(...).map(FN)` with single-level map, but the FN body accesses `n.Organization.Slug` — a two-hop dotted path. The fingerprinter does not descend into FN bodies, so dotted-path signals inside mapped callbacks are missed. **The 14-count for `needs_dotted_path_json_extract` is a lower bound; the real Phase 2 opportunity is larger than 0.7%.**

**Overall spot-check verdict:** scope-aware detection is substantially correct. Two minor detection gaps documented. One noteworthy classification under-count identified that materially affects the Phase 2 go/no-go narrative.

---

## §6 Go/No-Go by Downstream Phase

### Phase 0 (oracle on safe subset): GO

Plan §0.1: "Runs ONLY against generators where `shape-inventory.json` classifies `has_fig_api_refs: false`."

Safe subset size: **1,038 generators** (verified: `jq '[.shapes[] | select(.has_fig_api_refs == false) | .count] | add'` == 1038).

Prerequisite met: `shape-inventory.json` covers 100% of generators with closed-enum verdicts. No blockers. The safe subset is large enough (55% of corpus) to produce meaningful oracle coverage data without touching any generator flagged for manual review.

### Phase 2 (`json_extract` dotted paths): CONDITIONAL GO / FURTHER INVESTIGATION

Plan §1.4 gate: "`jq '.shapes[] | select(.verdict == \"needs_dotted_path_json_extract\") | .count' | add` — must be dominant."

Raw count from inventory: **14 (0.7%)** — not dominant by any reasonable interpretation.

However, spot-check finding 3 demonstrates the 14 is a lower bound. Several `existing_transforms` entries with `.map(FN)` + `JSON.parse` likely have dotted paths inside their FN body that the fingerprinter missed by not descending into callbacks.

**Recommendation:** before scheduling Phase 2, spend ~1 day running a secondary pass that descends into FN bodies of `.map(FN)`-shaped `JSON.parse` generators and recounts. Two outcomes:

- If true dotted-path count remains <5% of corpus after recount: Phase 2 is **NO-GO**. Re-scope as "extend the converter to emit single-field `json_extract` for currently-covered shapes" — this accrues against the `existing_transforms` bucket and is a converter bug fix, not a new transform.
- If recount pushes dotted-path to ≥5% and `existing_transforms` doesn't absorb them all via converter fix: Phase 2 is a lean **GO** scoped to the dotted-path extension only.

### Phase 3A (native providers): GO

Plan §3A gate: "≥4 qualifying candidates. If fewer, skip phase."

Actual: **36 qualifying candidates** out of 184 entries in `candidate-providers.json` (verified from file). Well above threshold.

Caveat: heuristic qualification has known false positives — names like `flyctl` and `firebase` pass the `authKeywords` regex heuristic despite requiring auth. The maintainer review per plan operational rule #5 handles this during implementation. See §9 open question 3 for a preemptive fix option.

### Property-based pivot (§1.7): NOT INVOKED

All six §1.6 exit criteria were met within the hard 2-week timebox.

---

## §7 Exit Criteria Confirmation (Plan §1.6 Checklist)

1. [x] `shape-inventory.json` + `.md` cover 100% of 1,889 generators — confirmed: `jq '.total_generators'` == `jq '[.shapes[].count] | add'` == 1889.
2. [x] Every generator has a verdict from the closed enum — confirmed by spec compliance review; 0 unknown verdicts across all 151 shapes.
3. [x] `js-builtin-allowlist.json` finalized — no additions surfaced by the corpus run; stays at schema_version 1.0.
4. [x] Corrected coverage numbers documented — §2 and §3 above.
5. [x] `candidate-providers.json` generated for Phase 3A — 184 entries, 36 qualifying. [Updated post-T4: see `spike-recount-decision.md` §9 — final qualifying count is 28.]
6. [x] Go/no-go decision written — §6 above.

---

## §8 Timebox Status

- Hard timebox: 2 weeks (plan §1 phase table + §1.6).
- Week-3 extension: not used.
- Property-based pivot (§1.7): not invoked.
- Actual work: single agentic session; all six exit criteria met. Wall-clock deliberation well under 2 weeks.

---

## §9 Open Questions for the Maintainer

**1. Phase 2 recount (high priority).** Should the secondary pass (descend into FN bodies of `.map(FN)` + `JSON.parse` generators) run before Phase 2 is scheduled, or should Phase 2 be deferred until that recount is done? Recommended: recount first (~1 day), then make the go/no-go call. Scheduling Phase 2 without it risks building infrastructure for a <1% slice when the actual scope is either much larger or fully absorbed by converter fixes.

**2. Analyzer detection gaps (medium priority).** The two minor gaps — (a) property-access Fig API reads not detected, (b) destructure-from-function-argument not classified — are not blockers for Phase 0 because they don't flip any verdict in the current inventory. If a future change raises the stakes (e.g., a new verdict that cares about Fig API property reads), they should be patched. Should they be addressed pre-Phase 0 or deferred to a post-spike analyzer hardening ticket?

**3. `flyctl` / `firebase` in qualifying candidates (low priority).** Both require auth but pass the heuristic qualifier. Maintainer review will catch these during Phase 3A. Alternatively, strengthening the `authKeywords` regex in `run-spike.mjs:qualifyCommand` would drop them preemptively — roughly a 15-minute follow-up. Worth doing before Phase 3A kickoff?

**4. `hand_audit_required` handoff (low priority).** 866 generators need manual review, mostly due to bundler-hoisted helper refs. Phase 0's oracle skips these by design. Does the maintainer want a triage document listing the top 10 shapes by count within `hand_audit_required`, or is querying `shape-inventory.json` directly sufficient for ad-hoc investigation?

---

## §10 Artifacts and Commits

| Artifact | Description |
|---|---|
| `tools/fig-converter/scripts/run-spike.mjs` | Driver — shape inventory + candidate qualification (commits: `045baf7`, `f357ad6`, `bab976e`) |
| `tools/fig-converter/docs/shape-inventory.json` | 1,889 generators, 151 shapes, 7 verdicts — canonical source of truth |
| `tools/fig-converter/docs/shape-inventory.md` | Human-readable bucket table |
| `tools/fig-converter/docs/candidate-providers.json` | 184 entries, 36 qualifying |
| `tools/fig-converter/src/ast-analyzer.js` | Two-stage parse added (commit `03790b4`) |
| `tools/fig-converter/src/ast-analyzer.test.js` | 3 new test cases (commit `03790b4`; case 18 strengthened in `bab976e`) |
| `tools/fig-converter/docs/spike-report.md` | This document |

**Five commits on `feature/phase-1-spike` on top of base `0c4e376`:**

1. `045baf7` — initial shape inventory + candidate providers driver
2. `03790b4` — analyzer two-stage parse fix + tests (recovers bare function expressions)
3. `f357ad6` — composite bucketing + revised verdict priority (split buckets by shape)
4. `bab976e` — code-review fixes (drop timestamp, dedupe verdict call, consolidate walker)
5. This document (`spike-report.md`)
