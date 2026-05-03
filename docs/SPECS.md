# Specs

Ghost Complete ships 709 Fig-compatible JSON completion specs sourced from
[`@withfig/autocomplete`](https://github.com/withfig/autocomplete) and converted
offline. The converted JSON lives under [`specs/`](../specs/) (~74 MB on disk
since the AWS spec was restored in `ux-8`) and is embedded into the binary at
build time via `include_str!`, so the shipped `ghost-complete` has zero
runtime spec-fetch cost and no network dependency. The embed is produced by
[`crates/gc-suggest/build.rs`](../crates/gc-suggest/build.rs), which strips the
runtime-unused `js_source` field and minifies each spec before `include_str!`
bakes it into the binary — minified embedded corpus is ~47 MB and the release
binary measures ~102 MB (under the 110 MB CI ceiling enforced by
[`docs/ci-gates.md`](./ci-gates.md#binary-size-gate); zstd-compressing the
embedded corpus is the queued reclaim path). On-disk `specs/*.json`
remain pretty-printed; only the binary-embedded copies are minified.

**Non-goal:** embedding a JavaScript runtime. Upstream specs sometimes include
inline JS generators (`postProcess`, `custom`, `trigger`); we either rewrite
those declaratively at convert time, replace them with a native Rust provider,
or mark the generator `requires_js` and ship it as a functional no-op. See
[`docs/PROVIDERS.md`](./PROVIDERS.md) and the umbrella initiative referenced
below.

## Conversion pipeline

```
┌─────────────────┐   npm run convert    ┌──────────────┐   build.rs     ┌─────────────┐   include_str!   ┌───────────┐
│ @withfig/...    │ ────────────────────▶│ specs/*.json │ ─────────────▶ │ OUT_DIR/    │ ───────────────▶ │ Rust bin  │
│ (TS + JS AST)   │                      │ (committed,  │                │ *.json      │                  │ (runtime) │
└─────────────────┘                      │  pretty)     │                │ (minified,  │                  └───────────┘
       ▲                                 └──────────────┘                │  no js_src) │
       │                                        ▲                        └─────────────┘
 upstream updates                    post-process-matcher.js
 (manual pull-through)               + native-map.js rules
```

Stages:

1. **Upstream `@withfig/autocomplete`** — TypeScript sources with inline JS for
   dynamic generators. Checked out as a sibling of the converter workspace.
2. **`tools/fig-converter/`** (Node, offline) — entry point
   [`tools/fig-converter/src/index.js`](../tools/fig-converter/src/index.js)
   runs `cleanSpec` over each spec, then routes generator nodes through
   [`post-process-matcher.js`](../tools/fig-converter/src/post-process-matcher.js)
   (declarative transform fingerprints) and
   [`native-map.js`](../tools/fig-converter/src/native-map.js) (script →
   native provider lookup). Run `npm --prefix tools/fig-converter test` when
   touching converter logic — the Rust `cargo test` suite does not cover it.
3. **`specs/*.json`** — committed, pretty-printed output. Snapshot-diff CI gate
   guards against silent large-scale regeneration drift.
4. **`crates/gc-suggest/build.rs`** — strips the runtime-unused `js_source`
   field from every generator and minifies the JSON into `OUT_DIR` so
   `include_str!` bakes a compact copy into the binary. Hand-editing the
   embed list (or bypassing `build.rs`) would break the binary-size gate.
5. **Rust binary** — `crates/gc-suggest/src/specs.rs` deserializes via serde
   at load time. Unknown generator types log a `warn!` and are skipped.

Upstream pull-through is a manual operation: bump the `@withfig/autocomplete`
submodule/checkout, run `npm run convert`, review the snapshot diff, commit.

## Hand-port vs converter extension

When a requires-JS generator needs to become declarative, the decision is
between extending the converter to recognize the pattern across all specs or
editing the generated JSON for one spec. The axes:

| Signal | Converter extension | Hand-port |
|---|---|---|
| Number of generators affected | 3 or more | 1-3 |
| Pattern distinguishable by AST fingerprint | yes | no |
| Transformation expressible in the current Rust runtime | yes, or a small extension | needs a brand-new transform variant |
| Reviewer can spot-check via snapshot diff | yes (wide but uniform diff) | yes, but per-file |

Extend the converter when the JS pattern is widespread, the shape is
recognizable from the AST (or from a stable fingerprint of the generator
`script` / `postProcess` body), and the resulting transformation maps onto
runtime machinery we already have. Phase 4 shipped two instances of this:
dotted-path `json_extract` / `json_extract_array` (14 generators across
`expo`, `expo-cli`, `pnpx`, `react-native`, `scarb`) and the new `suffix`
transform that unlocked declarative output for template-literal concatenation.

Hand-port when the JS is idiosyncratic (one or two generators), the pattern
can't be mechanically recognized (e.g. the shape hides behind a string
template literal the AST analyzer won't resolve without inlining), or the
runtime gap would need a new primitive per-case that won't see reuse. The
docker `service scale` generator is the template case here: it emits
`${serviceName}=` via a JS template literal the AST tooling doesn't
reconstruct. We added the `suffix` transform (reusable) and hand-edited
`specs/docker.json` for the one generator that needed it.

## Native providers

Some requires-JS generators are better replaced with async Rust code than with
declarative transforms — usually because the underlying subprocess returns
structured output that's awkward to parse with the current transform set.
See [`docs/PROVIDERS.md`](./PROVIDERS.md) for the full contract (eligibility
criteria, file layout, converter wiring).

## Coverage measurement and baseline refresh

Coverage is tracked in [`docs/coverage-baseline.json`](./coverage-baseline.json)
with `schema_version: "1.0"` and one row per release. Each row records:

- `version`, `timestamp` — release identity.
- `total_specs`, `fully_functional`, `requires_js_generators` — scanned from
  the embedded specs at release time.
- `native_providers`, `corrected_generators`, `hand_audit_required` — not
  derivable from the scanned specs alone; maintained manually per release.

`ghost-complete status --json` (added in Phase 4) emits a `spec_counts` object,
but the keys are deliberately different from the baseline-row schema: `status`
reports shipped/loaded counts (`total`, `partially_functional`, `embedded`,
`filesystem_overrides`, `parse_errors`), while the baseline tracks the
classification breakdown used for trend reporting. A refresh therefore involves
a small projection, not a copy.

Refresh workflow at release time:

```sh
# 1. Capture the current scan.
ghost-complete status --json > /tmp/status.json

# 2. Hand-edit docs/coverage-baseline.json: append a new object to `releases`
#    with the following fields, drawing on the scan output plus the
#    manually-maintained fields:
#      - version                    (the new release tag, e.g. "0.10.0")
#      - timestamp                  (ISO 8601 UTC)
#      - total_specs                (from status.json spec_counts.total)
#      - fully_functional           (from status.json spec_counts.fully_functional)
#      - requires_js_generators     (from the spike report; recount as needed)
#      - native_providers           (count current files in providers/ that
#                                    are wired into the ProviderKind enum)
#      - corrected_generators       (count of `_corrected_in` markers:
#                                    `grep -cR '"_corrected_in"' specs/`)
#      - hand_audit_required        (from the spike inventory, carried
#                                    forward until a recount)
#
# 3. Verify the file parses as JSON and that `ghost-complete status` renders
#    the trend section as expected (the last row should show signed deltas
#    against the previous row).
```

The projection is manual by design. A future `scripts/refresh-coverage-baseline.mjs`
could automate the `total_specs` / `fully_functional` / `corrected_generators`
projection from `status --json` plus `grep`, but the `native_providers`,
`requires_js_generators`, and `hand_audit_required` fields rely on analyses
that live outside the scanned spec JSON. An honest documented step beats a
half-finished automation.

**Owner:** maintainer, as part of the release checklist. **CI drift warning:**
a non-failing job on `master` surfaces a nudge when the baseline is stale;
see [`docs/ci-gates.md`](./ci-gates.md) for the full gate catalogue.

## The `_corrected_in` format extension

Phase -1 discovered that the converter had silently emitted wrong completions
for two patterns — `.substring(0, N)` / `.slice(0, N)` misconverted to
`column_extract` (byte-offset mistaken for whitespace columns), and
`JSON.parse` without a resolvable field access silently falling back to
`json_extract: "name"`. Both were corrected by downgrading the affected
generators to `requires_js` until a proper fix lands, and a format-extension
marker `_corrected_in: "vX.Y.Z"` was introduced so users can see which
generators changed behaviour between releases.

**Where it lives.** The converter allowlists the field in `cleanSpec`'s
generator-field allowlist
([`tools/fig-converter/src/index.js`](../tools/fig-converter/src/index.js)).
The Rust loader deserializes it via `#[serde(rename = "_corrected_in")]` on
the `GeneratorSpec` struct
([`crates/gc-suggest/src/specs.rs`](../crates/gc-suggest/src/specs.rs)).

**Why it persists.** Unlike a transient release-notes entry, the marker stays
in the spec across regenerations so any future `ghost-complete doctor` run
can enumerate the affected generators and show the version in which the
correction landed. It is a durable spec-format extension, per the umbrella
plan's explicit embrace of this single extension.

**How it surfaces.** `ghost-complete doctor` lists affected generators under
its corrected-generator check. `ghost-complete validate-specs --json` emits
one NDJSON row per spec plus a trailing `{"summary":{...}}` row, with the
marker visible on inspected generator nodes.

**When to add a new marker.** Only when the converter itself changed behaviour
in a way that needs user-visible acknowledgment — i.e. a correction, not a
feature. Do not set `_corrected_in` for a new transform landing or a new
native provider wiring up; those are ordinary coverage improvements and
belong in the changelog.

## Cross-references

- [`CONTRIBUTING.md`](../CONTRIBUTING.md) — project contribution workflow.
- [`docs/PROVIDERS.md`](./PROVIDERS.md) — native-provider contract and how to
  add one.
- [`docs/ARCHITECTURE.md`](./ARCHITECTURE.md) — overall system design.
- [`docs/COMPLETION_SPEC.md`](./COMPLETION_SPEC.md) — the Fig-compatible spec
  format reference.
- [`docs/ci-gates.md`](./ci-gates.md) — CI gate catalogue (binary-size,
  snapshot-diff, oracle, baseline-drift). Benchmark-regression checking is
  run manually at release time, not on every PR.
- [PR #75 — requires-JS specs multi-phase initiative (umbrella)](https://github.com/StanMarek/ghost-complete/pull/75) —
  the long-lived tracking PR; plan lives there since the planning doc is
  intentionally gitignored.

Optional: coverage badge evaluated and skipped — the shields.io
`dynamic/json` endpoint fetches from `raw.githubusercontent.com` on the
default branch, which 404s until this worktree merges to `master`.
Re-evaluate post-merge; the endpoint URL shape that works is
`https://img.shields.io/badge/dynamic/json?url=<raw.githubusercontent.com URL>&label=fully%20functional&query=%24.releases%5B-1%3A%5D.fully_functional&suffix=%20%2F%20709`.
