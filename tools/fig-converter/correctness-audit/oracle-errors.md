# Oracle Error Dispositions — Phase 0 Correctness Audit

_Phase 0 exit requires every `oracle_error` in `docs/oracle-results.json` to have an explicit disposition here. Grouped dispositions are marked with `## Auto-disposition:`._

_Canonical machine-readable data: `../../docs/oracle-results.json`._

## Summary

- Safe-subset size: **1038**
- Outcome totals: pass=439, fail=1, oracle_error=598
- Shapes fixtured (8): `parse-map`, `split-map`, `empty`, `parse-map-2`, `parse-map-3`, `unknown-3`, `entries-sort-map`, `empty-2`
- Every `oracle_error` in this run has class `missing_fixture` (no `js_exception`, `js_timeout`, `rust_exception`, or `source_missing` observed).

## Auto-disposition: missing_fixture

All 598 `oracle_error` entries in this run are `missing_fixture` — one-per-generator for shapes we deliberately chose not to fixture. One disposition line per *shape* below; the generator-level mapping is in `oracle-results.json`.

### `requires_runtime` (19 shapes, 162 generators)

_Rationale:_ Shape contains `await` or other runtime-only constructs; Rust transform pipeline cannot reproduce JS async semantics. No feasible fixture exists until a runtime-aware evaluator lands. Follow-up: consider a mini-VM or WASM-backed evaluator, or downgrade these generators to `requires_js` with a future fallback.

- `unknown-2` (58 gens, example: `meteor.json`) — disposition: missing_fixture; verdict is `requires_runtime`.
- `unknown-4` (22 gens, example: `systemctl.json`) — disposition: missing_fixture; verdict is `requires_runtime`.
- `empty-3` (16 gens, example: `bun.json`) — disposition: missing_fixture; verdict is `requires_runtime`.
- `trim-split-filter-map` (15 gens, example: `gem.json`) — disposition: missing_fixture; verdict is `requires_runtime`.
- `split-map-2` (11 gens, example: `chown.json`) — disposition: missing_fixture; verdict is `requires_runtime`.
- `split-map-3` (8 gens, example: `bun.json`) — disposition: missing_fixture; verdict is `requires_runtime`.
- `trim-split-filter-map-2` (7 gens, example: `apt.json`) — disposition: missing_fixture; verdict is `requires_runtime`.
- `unknown-8` (5 gens, example: `wd.json`) — disposition: missing_fixture; verdict is `requires_runtime`.
- `arr-6` (4 gens, example: `make.json`) — disposition: missing_fixture; verdict is `requires_runtime`.
- `keys-map-3` (4 gens, example: `projj.json`) — disposition: missing_fixture; verdict is `requires_runtime`.
- `arr-7` (3 gens, example: `goto.json`) — disposition: missing_fixture; verdict is `requires_runtime`.
- `parse-map-7` (2 gens, example: `shadcn-ui.json`) — disposition: missing_fixture; verdict is `requires_runtime`.
- `empty-5` (1 gens, example: `dapr.json`) — disposition: missing_fixture; verdict is `requires_runtime`.
- `parse-map-8` (1 gens, example: `degit.json`) — disposition: missing_fixture; verdict is `requires_runtime`.
- `filter-map-2` (1 gens, example: `j.json`) — disposition: missing_fixture; verdict is `requires_runtime`.
- `split-filter-map` (1 gens, example: `mdfind.json`) — disposition: missing_fixture; verdict is `requires_runtime`.
- `map-5` (1 gens, example: `robot.json`) — disposition: missing_fixture; verdict is `requires_runtime`.
- `arr-10` (1 gens, example: `yarn.json`) — disposition: missing_fixture; verdict is `requires_runtime`.
- `split-slice-map` (1 gens, example: `youtube-dl.json`) — disposition: missing_fixture; verdict is `requires_runtime`.

### `needs_new_transform_conditional_split` (11 shapes, 148 generators)

_Rationale:_ Shape has inline conditionals (`if (x) return []` or ternaries inside the map) that the transform pipeline cannot express. Follow-up: extend `error_guard` with multi-pattern support, or add a `conditional_return` transform.

- `unknown` (73 gens, example: `bazel.json`) — disposition: missing_fixture; verdict is `needs_new_transform_conditional_split`.
- `keys-map` (38 gens, example: `chezmoi.json`) — disposition: missing_fixture; verdict is `needs_new_transform_conditional_split`.
- `arr-arr` (16 gens, example: `kubecolor.json`) — disposition: missing_fixture; verdict is `needs_new_transform_conditional_split`.
- `arr-4` (6 gens, example: `bun.json`) — disposition: missing_fixture; verdict is `needs_new_transform_conditional_split`.
- `empty-4` (4 gens, example: `ipatool.json`) — disposition: missing_fixture; verdict is `needs_new_transform_conditional_split`.
- `arr-5` (4 gens, example: `kubecolor.json`) — disposition: missing_fixture; verdict is `needs_new_transform_conditional_split`.
- `includes` (3 gens, example: `chezmoi.json`) — disposition: missing_fixture; verdict is `needs_new_transform_conditional_split`.
- `startswith-keys-map` (1 gens, example: `echo.json`) — disposition: missing_fixture; verdict is `needs_new_transform_conditional_split`.
- `isnan-isinteger` (1 gens, example: `firefox.json`) — disposition: missing_fixture; verdict is `needs_new_transform_conditional_split`.
- `map-4` (1 gens, example: `lsof.json`) — disposition: missing_fixture; verdict is `needs_new_transform_conditional_split`.
- `map-6` (1 gens, example: `ykman.json`) — disposition: missing_fixture; verdict is `needs_new_transform_conditional_split`.

### `needs_new_transform_regex_match` (3 shapes, 52 generators)

_Rationale:_ Shape uses `.matchAll(regex)` / `string.match(regex)` over the raw command output. The existing `regex_extract` transform operates line-by-line post-split; it cannot replicate match-over-unsplit-string semantics. Follow-up: add a `regex_match_extract` transform.

- `match-map` (50 gens, example: `flutter.json`) — disposition: missing_fixture; verdict is `needs_new_transform_regex_match`.
- `unknown-10` (1 gens, example: `kill.json`) — disposition: missing_fixture; verdict is `needs_new_transform_regex_match`.
- `map-3` (1 gens, example: `lsof.json`) — disposition: missing_fixture; verdict is `needs_new_transform_regex_match`.

### `needs_new_transform_substring_slice` (9 shapes, 33 generators)

_Rationale:_ Shape reads `str.substring`, `str.slice`, or `arr.slice` to carve out segments. Transform pipeline has no substring primitive. Follow-up: add a `substring_extract` transform with start/end indices.

- `unknown-5` (10 gens, example: `deno.json`) — disposition: missing_fixture; verdict is `needs_new_transform_substring_slice`.
- `unknown-6` (8 gens, example: `gpg.json`) — disposition: missing_fixture; verdict is `needs_new_transform_substring_slice`.
- `from-map` (7 gens, example: `n.json`) — disposition: missing_fixture; verdict is `needs_new_transform_substring_slice`.
- `filter` (3 gens, example: `chezmoi.json`) — disposition: missing_fixture; verdict is `needs_new_transform_substring_slice`.
- `slice-some-map` (1 gens, example: `brew.json`) — disposition: missing_fixture; verdict is `needs_new_transform_substring_slice`.
- `arr-9` (1 gens, example: `git-cliff.json`) — disposition: missing_fixture; verdict is `needs_new_transform_substring_slice`.
- `trim-split-map-2` (1 gens, example: `killall.json`) — disposition: missing_fixture; verdict is `needs_new_transform_substring_slice`.
- `arr-arr-2` (1 gens, example: `nx.json`) — disposition: missing_fixture; verdict is `needs_new_transform_substring_slice`.
- `trim-slice-split-filter-map` (1 gens, example: `v.json`) — disposition: missing_fixture; verdict is `needs_new_transform_substring_slice`.

### `needs_dotted_path_json_extract` (1 shapes, 14 generators)

_Rationale:_ Shape does `JSON.parse(...).a.b.c.map(...)` with 2+ property hops. Current `json_extract` only supports a top-level key. Follow-up: add JSONPath-lite support (see shape verdict name).

- `parse-map-4` (14 gens, example: `expo-cli.json`) — disposition: missing_fixture; verdict is `needs_dotted_path_json_extract`.

### `hand_audit_required` (1 shapes, 15 generators)

_Rationale:_ AST parsing failed (`<parse_error>`) — source uses syntactic features the Babel parser rejects (rare minified idioms). Needs individual human inspection to determine whether the generator is salvageable via a raw-text rewrite or should be left `requires_js`.

- `parse-error` (15 gens, example: `dotnet.json`) — disposition: missing_fixture; verdict is `hand_audit_required`.

### `existing_transforms` (21 shapes, 174 generators)

_Rationale:_ Shape is theoretically expressible with existing transforms BUT was not fixtured in this pass. Reason varies by shape: some fall outside the top-20 coverage budget; some have too-loose shape buckets (members read different JSON fields or use array-only vs hash-only semantics) to be captured by a single Rust pipeline. Follow-up: either split the shape bucket with tighter fingerprints or fixture per sub-shape.

- `arr` (44 gens, example: `ng.json`) — disposition: missing_fixture; verdict is `existing_transforms`.
- `filter-map` (19 gens, example: `eslint.json`) — disposition: missing_fixture; verdict is `existing_transforms`.
- `arr-2` (17 gens, example: `asdf.json`) — disposition: missing_fixture; verdict is `existing_transforms`.
- `arr-3` (16 gens, example: `bun.json`) — disposition: missing_fixture; verdict is `existing_transforms`.
- `keys-map-2` (14 gens, example: `cargo.json`) — disposition: missing_fixture; verdict is `existing_transforms`.
- `from` (12 gens, example: `expo-cli.json`) — disposition: missing_fixture; verdict is `existing_transforms`.
- `split-map-4` (8 gens, example: `docker.json`) — disposition: missing_fixture; verdict is `existing_transforms`.
- `parse-map-5` (7 gens, example: `amplify.json`) — disposition: missing_fixture; verdict is `existing_transforms`.
- `unknown-7` (7 gens, example: `cordova.json`) — disposition: missing_fixture; verdict is `existing_transforms`.
- `split-map-5` (6 gens, example: `assimp.json`) — disposition: missing_fixture; verdict is `existing_transforms`.
- `trim-split-map` (5 gens, example: `bat.json`) — disposition: missing_fixture; verdict is `existing_transforms`.
- `from-map-2` (5 gens, example: `envchain.json`) — disposition: missing_fixture; verdict is `existing_transforms`.
- `parse-map-6` (3 gens, example: `watson.json`) — disposition: missing_fixture; verdict is `existing_transforms`.
- `keys-map-4` (2 gens, example: `ansible-doc.json`) — disposition: missing_fixture; verdict is `existing_transforms`.
- `unknown-9` (2 gens, example: `lerna.json`) — disposition: missing_fixture; verdict is `existing_transforms`.
- `arr-8` (2 gens, example: `yarn.json`) — disposition: missing_fixture; verdict is `existing_transforms`.
- `parse-map-9` (1 gens, example: `deno.json`) — disposition: missing_fixture; verdict is `existing_transforms`.
- `map` (1 gens, example: `deployctl.json`) — disposition: missing_fixture; verdict is `existing_transforms`.
- `values-map` (1 gens, example: `env.json`) — disposition: missing_fixture; verdict is `existing_transforms`.
- `map-2` (1 gens, example: `lsof.json`) — disposition: missing_fixture; verdict is `existing_transforms`.
- `keys-reduce` (1 gens, example: `sake.json`) — disposition: missing_fixture; verdict is `existing_transforms`.

## Per-generator dispositions (non-`missing_fixture`)

_None observed in this run._ If future oracle runs produce `js_exception`, `js_timeout`, `rust_exception`, or `source_missing` errors, add per-generator dispositions in this section using the schema:

```
- generator_id: <id>
  exception: <class + message>
  disposition: retry_different_input | reclassify_hand_audit | fix_allowlist | missing_fixture
  rationale: <one line>
```

## Known `fail` outcomes (not `oracle_error`, but noted here for maintainers)

- `docker:/subcommands[51]/subcommands[7]/args/generators[0]` (shape: `split-map`): item[0] name/text mismatch: js="item1=" rust="item1"
  - Analysis: this docker generator uses a template-literal that appends `=` to the extracted name (e.g. `${n.Name}=`). The split-map bucket merges members that DO append suffixes with members that do not — a shape-bucket-too-loose signal. Not a fixture bug; not a code bug. Follow-up: tighten the fingerprint to separate suffix-appending variants, or leave flagged here.

## Phase 0 exit check

Every `oracle_error` in `oracle-results.json` is accounted for by the auto-dispositions above (grouped by shape). Line-count sanity check: this file covers 65 distinct shapes, totaling 598 generators.
