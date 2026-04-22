# Phase 2 Recount Decision — `needs_dotted_path_json_extract`

**Date:** 2026-04-22
**Branch:** `feature/phase-2-recount`
**Status:** DONE — Phase 2 go/no-go gate

---

## §1 Executive Summary

Callback-body fingerprint descent produced **zero reclassification** across verdict boundaries. `needs_dotted_path_json_extract` stayed at **14 generators (0.7%)** — the old count was NOT a lower bound in the corpus. The single dotted-path shape bucket carved into 2 variants, and a handful of neighbouring buckets fragmented similarly, but no generator was promoted into the dotted-path verdict from anywhere else. **Recommendation: NO GO on Phase 2; fold the 14 cases into Phase 4 cleanup as a converter extension.**

---

## §2 Methodology

- **T1 fingerprinter change** (commit `1d568d0`): `.map(FN)` / `.filter(FN)` calls now descend into the callback body and emit `.map(<inner>)` / `.filter(<inner>)` where `<inner>` is the recursively fingerprinted body. Prior behaviour was opaque `.map(FN)` regardless of body shape.
- **T2 spike re-run** (commit `07131c0`): regenerated `shape-inventory.json` and `shape-inventory.md` against the updated fingerprinter. `candidate-providers.json` left untouched (T1/T2 do not affect provider qualification).
- **Unchanged:** `run-spike.mjs` driver logic, the `assignVerdict` detector, the closed 7-verdict enum, and every heuristic used for `has_fig_api_refs`. Only the fingerprint string changed, which affects shape bucket cardinality, not verdict assignment.

---

## §3 Before / After Bucket Table

| Verdict | Pre (count/%) | Post (count/%) | Shape variants (pre → post) |
|---|---|---|---|
| `hand_audit_required` | 866 (45.8%) | 866 (45.8%) | 79 → 81 (+2) |
| `existing_transforms` | 614 (32.5%) | 614 (32.5%) | 29 → 32 (+3) |
| `requires_runtime` | 162 (8.6%) | 162 (8.6%) | 19 → 19 |
| `needs_new_transform_conditional_split` | 148 (7.8%) | 148 (7.8%) | 11 → 11 |
| `needs_new_transform_regex_match` | 52 (2.8%) | 52 (2.8%) | 3 → 3 |
| `needs_new_transform_substring_slice` | 33 (1.7%) | 33 (1.7%) | 9 → 9 |
| `needs_dotted_path_json_extract` | 14 (0.7%) | 14 (0.7%) | 1 → 2 (+1) |
| **Total** | **1,889 (100%)** | **1,889 (100%)** | **151 → 157 (+6)** |

Pre-recount source: `spike-report.md` §3. Post-recount source: `shape-inventory.json` at HEAD (`07131c0`), verified with `jq`.

---

## §4 Reclassification Analysis

**Zero generators moved across verdict boundaries.** The callback descent carved existing shape buckets more finely; it did not promote any generator from `existing_transforms` (or anywhere else) into `needs_dotted_path_json_extract`.

The two post-recount dotted-path fingerprints are:

1. `JSON.parse(...).PROP.PROP.map(<OBJ>)` — 8 generators
2. `JSON.parse(...).PROP.PROP.map(<.split(STR)[COMPUTED]>)` — 6 generators

Both have `JSON.parse(...).PROP.PROP` on the **outer** chain — already detected pre-T1 as a dotted-path signal by the verdict rules. The T1 descent only split the single pre-existing `JSON.parse(...).PROP.PROP.map(FN)` bucket (14 generators, 1 variant) by callback body shape, turning 1 variant into 2 while leaving total count fixed.

Crucially, T1 did NOT surface any `JSON.parse`-inside-callback patterns elsewhere. Spike finding 3 hypothesised that buckets like `.split().map(x => JSON.parse(x).a.b)` might be hiding in `existing_transforms`. If any such shapes existed, the descent would have emitted `.map(<JSON.parse(...).PROP.PROP>)` fingerprints and the verdict detector would have routed them to the dotted-path bucket. Since `existing_transforms` held steady at 614 generators and the dotted-path bucket held steady at 14, those shapes do not exist in any material quantity in the 1,889-generator corpus.

---

## §5 Phase 2 Go/No-Go

Decision rubric (plan §1.4 + spike §6):

- ≥20% of total → GO as standalone Phase 2.
- 5–20% → CONDITIONAL — keep scheduled, down-scope to 3 days.
- <5% → NO GO — fold into `existing_transforms` as a converter fix, absorb into Phase 4 cleanup.

14 / 1,889 = **0.74%** — well below the 5% threshold. **Recommendation: NO GO.**

Rationale:

- The old 14-count was not a lower bound. Spike finding 3's prediction did not hold against the corpus.
- Callback descent was the principled way to test the hypothesis; it returned negative with zero cross-verdict movement.
- The 14 cases fit the existing `json_extract` transform extension pattern — they need a dotted-path modifier, not a new transform class.
- A dedicated 3-day Phase 2 build-out for 0.7% of the corpus is disproportionate infrastructure cost for marginal coverage.
- Phase 4 cleanup is the natural home for a narrow converter extension covering the 2 identified shape variants.

---

## §6 Other Findings Surfaced by the Recount

- **`hand_audit_required` variants 79 → 81 (+2).** Reflects callback descent on fig-api-containing generators; the inner shapes now differ where they previously collapsed into `.map(FN)`. Triage priority unchanged — these still require manual audit.
- **`existing_transforms` variants 29 → 32 (+3).** The descent surfaced shape groupings previously indistinguishable. Candidates for converter-fix hardening in Phase 4 — the new variants may reveal tighter code-gen opportunities.
- **No new shapes emerged outside existing verdict buckets.** All six new variants landed inside verdicts that already existed pre-T1. The verdict enum remains closed at 7.

---

## §7 Next Actions

- **Phase 2:** CLOSE as "skipped per recount." Umbrella PR #75 checklist update is a separate step (session-close housekeeping, not this doc).
- **Phase 4:** absorb the 14 dotted-path cases as a small converter extension in `post-process-matcher.js` (or equivalent). Targeted extension to emit `json_extract` with dotted-path support for the 2 identified shape variants. Expected diff: small. Bench/binary impact: negligible.
- **T4 (authKeywords):** independent of the Phase 2 decision; proceeds this session per plan.

---

## §8 Artifact Pointers

| Artifact | Description |
|---|---|
| `tools/fig-converter/docs/shape-inventory.json` | Post-recount canonical inventory (157 shapes, 1,889 generators) |
| `tools/fig-converter/docs/shape-inventory.md` | Human-readable bucket table (post-recount) |
| `tools/fig-converter/docs/candidate-providers.json` | Unchanged by T1/T2 (184 entries, 36 qualifying) |
| `tools/fig-converter/docs/spike-report.md` | Phase 1 report — pre-recount source of truth for §3 |

**Commits:**

- `1d568d0` — feat(fig-converter): descend into .map/.filter callback bodies for fingerprinting
- `07131c0` — chore: re-run Phase 1 spike with callback-body fingerprinting
