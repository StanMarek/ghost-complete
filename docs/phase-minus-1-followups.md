# Phase -1 Follow-ups

Work deferred from the Phase -1 spec regeneration in `feature/requires-js-groundwork`.
These items existed BEFORE Phase -1 but were surfaced by the first full converter
regen since v0.2.0. Not blockers for Phase -1 shipping.

## 1. cd.json / git.json regen reconciliation

**Status:** `cd.json` and `git.json` deliberately NOT regenerated in Phase -1.

**cd.json:** upstream `@withfig/autocomplete/build/cd.js` uses a JS custom generator
for directory completions. Post-T1, that generator correctly lowers to
`requires_js: true`, dropping the hand-authored `"args": {"template": "folders"}`
shape that engine tests `test_cd_only_shows_directories`,
`test_cd_first_suggestion_is_parent_dir`, and `test_cd_chaining_offers_double_parent`
rely on. Regen here would ship a user-visible regression (`cd <TAB>` loses the
folders template path).

**git.json:** upstream uses `["git", "--no-optional-locks", "branch", ...]` as the
script prefix. `matchNativeGenerator` only recognises the two-token `git branch` /
`git tag` / `git remote` forms. Regen here drops the hand-authored native
`{type: "git_branches"}` / `{type: "git_tags"}` entries, regressing tests
`test_git_checkout_waits_for_ref_generators_in_arg_position` and
`test_git_checkout_with_flag_prefix_still_shows_flags` and (more importantly) the
instant-ref fast path on `git checkout <TAB>`.

**Proposed fix (future phase, ideally 3A):**
- Extend `matchNativeGenerator` to strip `--no-optional-locks` and similar
  no-op flags before matching the first two tokens.
- For `cd.json`: either teach the converter to recognise the specific custom
  generator and emit `template: "folders"`, OR hand-audit after Phase 1's spike
  surfaces whether this is one-off or part of a pattern.

## 2. resolveLoadSpecs has no cycle guard

**Status:** pre-existing, shipped-as-stubs workaround.

`tools/fig-converter/src/index.js` lines 60-101 (`resolveLoadSpecs`) recursively
resolves `loadSpec` references without cycle detection. `@withfig/autocomplete/build/simctl.js`
has `loadSpec: "simctl"` on its `help` subcommand (self-reference) -> infinite
recursion -> Node heap OOM. `xcrun` transitively loads `simctl`, so it cascades.

Current workaround: `simctl.json` and `xcrun.json` ship as ~60-byte stubs and have
not been regenerated since v0.2.0.

**Proposed fix:** add `visited: Set<string>` threaded through `resolveLoadSpecs`,
break the cycle with a console warning, and re-enable full regeneration of
these two specs.

**Resolved in 5afed5a (cycle guard) + d13c3a2 (spec regen).** `resolveLoadSpecs`
now threads a `visited: Set<string>`, initialised with the top-level spec name,
and emits a single `console.warn` when a cycle is detected. `simctl.json` grew
from 59 B to 30.6 KB (37 subcommands); `xcrun.json` grew from 65 B to 58.1 KB
(3 subcommands). Full-corpus regen surfaced no latent cycles beyond these two.

## 3. Single-process `npm run convert` OOMs at 705 specs

**Status:** pre-existing, worked around in this branch.

Dynamic `import()` of 705 upstream spec modules exceeds Node's heap even at
`--max-old-space-size=8192` due to module-graph accumulation.

Current workaround: batch via 36 invocations of 20 specs each; each batch
is a fresh Node process.

**Proposed fix:** either (a) add a batching mode to `src/index.js` with
explicit flush points, or (b) switch `convert` to spawn workers and parallelise.
Option (a) is smaller; option (b) is faster.

**Resolved in 4ac85c8 (batching) + 4f4aab4 (failure-count + stdout-buffer fixes)
+ a273961 (integration test).** `npm run convert` now spawns a fresh Node worker
per batch of 30 specs (configurable via `--batch-size`); each child exits after
its batch, freeing its module cache. One command still does the whole run. Worker
body is factored as `runConversionBatch` so the in-process fast path and the
subprocess workers share logic. Measured on a 716-spec run: 2.83 s wall-clock,
peak RSS ~772 MB (well under the 2 GB target).

## 4. specs/__snapshots__ baseline (Phase 0 prerequisite)

Plan §0.3 requires Phase 0 to snapshot the post-Phase-(-1) spec state. That
baseline capture is a Phase 0 deliverable, not Phase -1. No action needed here.
