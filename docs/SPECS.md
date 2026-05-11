# Specs

Ghost Complete ships 709 Fig-compatible JSON completion specs sourced from
[`@withfig/autocomplete`](https://github.com/withfig/autocomplete) and converted
offline. The converted JSON lives under [`specs/`](../specs/) (~74 MB on disk
since the AWS spec was restored in `ux-8`) and is embedded into the binary at
build time via `include_str!`, so the shipped `ghost-complete` has zero
runtime spec-fetch cost and no network dependency. The embed is produced by
[`crates/gc-suggest/build.rs`](../crates/gc-suggest/build.rs), which minifies
each spec and defensively strips any straggler `js_source` field before
`include_str!` bakes it into the binary. The converter no longer emits
`js_source` — runtime-needed JS is preserved on
`js_runtime.source` (a structured object the build pipeline retains untouched).
Stale user-installed specs from older converter versions are still tolerated:
the embed pass drops their `js_source` so the embedded format stays uniform.
The release binary measures ~102 MB (under the 110 MB CI ceiling enforced by
[`docs/ci-gates.md`](./ci-gates.md#binary-size-gate); zstd-compressing the
embedded corpus is the queued reclaim path). On-disk `specs/*.json` remain
pretty-printed; only the binary-embedded copies are minified.

**JavaScript runtime:** the JS runtime lives in [`gc-jsrt`](../crates/gc-jsrt/)
— a bounded QuickJS evaluator (via rquickjs, default on). Upstream specs
that include inline JS generators (`postProcess`, `custom`,
`trigger`, `script: () => [...]`) are still preferred to be lowered
declaratively at convert time or replaced with a native Rust provider when the
shape is reusable; otherwise the converter emits a `js_runtime` block on the
generator and the runtime evaluates it at suggestion time. See
[`docs/PROVIDERS.md`](./PROVIDERS.md), [`docs/JS_RUNTIME.md`](./JS_RUNTIME.md),
and the umbrella initiative referenced below.

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
   (declarative transform fingerprints, including Fig helper recovery through
   `helper-registry.json` / `helper-matcher.js`) and
   [`native-map.js`](../tools/fig-converter/src/native-map.js) (script →
   native provider lookup). Run `npm --prefix tools/fig-converter test` when
   touching converter logic — the Rust `cargo test` suite does not cover it.
3. **`specs/*.json`** — committed, pretty-printed output. Snapshot-diff CI gate
   guards against silent large-scale regeneration drift.
4. **`crates/gc-suggest/build.rs`** — minifies each spec into `OUT_DIR` so
   `include_str!` bakes a compact copy into the binary. Drops any legacy
   `js_source` field defensively (the converter no longer emits it;
   structured `js_runtime.source` survives untouched and is asserted
   present at the same depth). Hand-editing the embed list (or bypassing
   `build.rs`) would break the binary-size gate.
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
runtime machinery we already have. Two examples of this in action:
dotted-path `json_extract` / `json_extract_array` (14 generators across
`expo`, `expo-cli`, `pnpx`, `react-native`, `scarb`) and the new `suffix`
transform that unlocked declarative output for template-literal concatenation.
ux-10b applies the same rule to Fig's minified AWS helper calls, comma-list
cleanup shapes, and earlier postProcess-to-transform matches: 1,558 generators
now carry `_lowered_from_requires_js: true` for status accounting.

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
  the embedded specs at release time. (Legacy fields, retained across schema
  bumps so older `BaselineRelease` consumers keep parsing.)
- `native_providers`, `corrected_generators`, `hand_audit_required` — not
  derivable from the scanned specs alone; maintained manually per release.
- **Coverage breakdown fields** (carried in the same release row, parsed via
  the flatten-extra map for forward compatibility):
  `spec_files_total`, `commands_addressable`,
  `commands_(fully|partially|non)functional`,
  `requires_js_generators_(total|supported|unsupported)`,
  `requires_js_generators_token_only`,
  `requires_js_generators_lowered_to_transforms`,
  `requires_js_generators_static_extracted`, and
  `command_alias_conflicts`. See `docs/COMPLETION_SPEC.md` for the
  classification rules. `requires_js_generators_supported` is broken down
  per `js_runtime.kind` (`post_process`, `script_function`, `custom`,
  `token_only`) in `status --json`, while the `counters` block carries the
  migration counters used by the native-completion roadmap.

`ghost-complete status --json` emits a `spec_counts` object whose keys
mirror the new baseline fields one-to-one. The legacy keys
(`total`, `fully_functional`, `partially_functional`, `embedded`,
`filesystem_overrides`, `parse_errors`) are retained alongside for
backwards compat. The schema_version field on the JSON output bumped from
`"1.0"` to `"1.1"` when the new keys landed; old consumers see the legacy
keys unchanged.

The output also carries a top-level `file_scan` block (`spec_files_total`,
`requires_js_generators_total`) that is computed independently from the
runtime loader index. This is on purpose: SpecStore is keyed by filename
stem so commands stay addressable even when two files declare the same
`name`; alias conflicts are surfaced as load-side warnings and exposed
through `status --json` and `ghost-complete doctor`.

Refresh workflow at release time:

```sh
# 1. Capture the current scan.
ghost-complete status --json > /tmp/status.json

# 2. (Optional) cross-check with the repo-local script that walks the spec
#    JSON via jq, independent of the runtime loader. The two should agree
#    on `requires_js_generators_total`, `command_alias_conflicts`, and
#    `spec_files_total`. The fully/partially counts can differ slightly
#    when the same `name` is shared by multiple files (the runtime
#    classifies per addressable command; the script per file). To keep
#    those two sources visibly distinct, the script emits the file-level
#    counts as `file_scan_fully_functional` / `file_scan_partially_functional`,
#    while the runtime-level counts in `status --json` keep the
#    `commands_*` prefix.
scripts/count-spec-coverage.sh --json > /tmp/scan.json

# 3. Hand-edit docs/coverage-baseline.json: append a new object to `releases`
#    with the following fields, drawing on the scan output plus the
#    manually-maintained fields:
#      - version                              (the new release tag)
#      - timestamp                            (ISO 8601 UTC)
#      - total_specs / fully_functional       (legacy — keep populated for
#                                              backwards compat)
#      - requires_js_generators               (legacy — keep populated)
#      - native_providers                     (count files in providers/ that
#                                              are wired into the ProviderKind enum)
#      - corrected_generators                 (count of `_corrected_in` markers:
#                                              `grep -cR '"_corrected_in"' specs/`)
#      - hand_audit_required                  (from the spike inventory, carried
#                                              forward until a recount)
#      - spec_files_total                     (from status.json file_scan.spec_files_total)
#      - commands_addressable                 (from status.json spec_counts.commands_addressable)
#      - commands_fully_functional            (from status.json)
#      - commands_partially_functional        (from status.json)
#      - commands_nonfunctional               (from status.json)
#      - requires_js_generators_total         (from status.json)
#      - requires_js_generators_supported     (from status.json)
#      - requires_js_generators_unsupported   (from status.json)
#      - command_alias_conflicts              (from status.json)
#
# 4. Verify the file parses as JSON and that `ghost-complete status` renders
#    the trend section as expected (the last row should show signed deltas
#    against the previous row).
```

The projection is manual by design. A future `scripts/refresh-coverage-baseline.mjs`
could automate the projection from `status --json` plus `grep`, but the
`native_providers` and `hand_audit_required` fields rely on analyses that
live outside the scanned spec JSON. An honest documented step beats a
half-finished automation.

**Owner:** maintainer, as part of the release checklist. **CI drift warning:**
a non-failing job on `master` surfaces a nudge when the baseline is stale;
see [`docs/ci-gates.md`](./ci-gates.md) for the full gate catalogue.

## The `_corrected_in` format extension

The converter previously emitted wrong completions for two patterns —
`.substring(0, N)` / `.slice(0, N)` misconverted to `column_extract`
(byte-offset mistaken for whitespace columns), and `JSON.parse` without
a resolvable field access silently falling back to `json_extract: "name"`.
Both were corrected by downgrading the affected
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
