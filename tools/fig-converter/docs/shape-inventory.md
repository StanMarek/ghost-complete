# Shape Inventory — Phase 1 Classification Spike

> Canonical machine-readable data: [shape-inventory.json](./shape-inventory.json)

**Total generators analysed:** 1889
**Total distinct shapes:** 77

## Verdict Distribution

| Verdict | Count | % |
|---------|-------|---|
| `hand_audit_required` | 1712 | 90.6% |
| `needs_new_transform_json_parse_simple` | 100 | 5.3% |
| `existing_transforms` | 35 | 1.9% |
| `requires_runtime` | 24 | 1.3% |
| `needs_new_transform_conditional_split` | 9 | 0.5% |
| `needs_dotted_path_json_extract` | 8 | 0.4% |
| `needs_new_transform_substring_slice` | 1 | 0.1% |

## Shapes Table

| shape_id | count | fingerprint (≤80 chars) | verdict | has_fig_api_refs | example_spec |
|----------|-------|-------------------------|---------|------------------|--------------|
| `parse-error` | 536 | `<parse_error>` | `hand_audit_required` | false | amplify.json |
| `unknown` | 330 | `...` | `hand_audit_required` | true | cargo.json |
| `parse-map` | 231 | `JSON.parse(...).map(FN)` | `hand_audit_required` | true | degit.json |
| `arr` | 150 | `ARR` | `hand_audit_required` | true | asdf.json |
| `empty` | 73 | `` | `hand_audit_required` | true | arduino-cli.json |
| `ue-fn` | 51 | `ue(...,...,...,FN)` | `hand_audit_required` | true | cloudflared.json |
| `a-fn` | 43 | `A(...,...,...,FN)` | `hand_audit_required` | true | cargo.json |
| `parse-map-2` | 39 | `JSON.parse(...).PROP.map(FN)` | `needs_new_transform_json_parse_simple` | false | cargo.json |
| `de-fn` | 28 | `de(...,...,...,FN)` | `hand_audit_required` | true | coda.json |
| `r-fn` | 25 | `R(...,...,...,FN)` | `hand_audit_required` | true | chezmoi.json |
| `pe-fn` | 24 | `pe(...,...,...,FN)` | `hand_audit_required` | true | dotenv-vault.json |
| `re-fn` | 22 | `Re(...,...,...,FN)` | `hand_audit_required` | true | cargo.json |
| `entries-sort-map` | 22 | `Object.entries(...).sort(FN).map(FN)` | `needs_new_transform_json_parse_simple` | false | fly.json |
| `trim-split-map` | 20 | `.trim().split(STR).map(FN)` | `hand_audit_required` | true | dscl.json |
| `filter-map` | 20 | `.filter(FN).map(FN)` | `existing_transforms` | false | eslint.json |
| `keys-map` | 19 | `Object.keys(...).map(FN)` | `needs_new_transform_json_parse_simple` | false | cargo.json |
| `split-map` | 18 | `.split(STR).map(FN).map(FN)` | `needs_new_transform_json_parse_simple` | false | docker.json |
| `split-map-2` | 15 | `.split(STR).map(FN)` | `hand_audit_required` | true | asdf.json |
| `le-fn` | 15 | `le(...,...,...,FN)` | `hand_audit_required` | true | asr.json |
| `trim-split-filter-map` | 15 | `.trim().split(STR).filter(FN).map(FN)` | `requires_runtime` | false | gem.json |
| `ce-fn` | 12 | `ce(...,...,...,FN)` | `hand_audit_required` | true | ansible-playbook.json |
| `from` | 12 | `Array.from(...)` | `existing_transforms` | false | expo-cli.json |
| `ge-fn` | 9 | `ge(...,...,...,FN)` | `hand_audit_required` | true | apt.json |
| `ae-fn` | 9 | `ae(...,...,...,FN)` | `hand_audit_required` | true | dotnet.json |
| `split-map-filter` | 9 | `.split(STR).map(FN).filter(FN).map(FN)` | `hand_audit_required` | true | kitty.json |
| `k-fn` | 8 | `k(...,...,...,FN)` | `hand_audit_required` | true | esbuild.json |
| `parse-map-3` | 8 | `JSON.parse(...).PROP.PROP.map(FN)` | `needs_dotted_path_json_extract` | false | expo-cli.json |
| `trim-split-filter-map-2` | 7 | `.trim().split(STR).filter(FN).map(FN).map(FN)` | `requires_runtime` | false | apt.json |
| `includes` | 6 | `(.includes(STR) ? ... : ...)` | `hand_audit_required` | true | chezmoi.json |
| `me-fn` | 6 | `me(...,...,...,FN)` | `hand_audit_required` | true | pnpx.json |
| `fe-fn` | 5 | `Fe(...,...,...,FN)` | `hand_audit_required` | true | bun.json |
| `j-fn` | 5 | `j(...,...,...,FN)` | `hand_audit_required` | true | file.json |
| `filter-map-2` | 5 | `await c(STR,...,...,STR).filter(FN).map(FN)` | `hand_audit_required` | true | kitty.json |
| `t-fn` | 5 | `T(...,...,...,FN)` | `hand_audit_required` | true | scc.json |
| `g-fn` | 4 | `G(...,...,...,FN)` | `hand_audit_required` | true | bun.json |
| `fe-fn-2` | 4 | `fe(...,...,...,FN)` | `hand_audit_required` | true | dotnet.json |
| `parse-map-4` | 4 | `(... ? ARR : JSON.parse(...).map(...))` | `hand_audit_required` | true | gh.json |
| `typewithoutname` | 4 | `.typeWithoutName(STR)` | `hand_audit_required` | true | kubecolor.json |
| `filter-map-3` | 4 | `await d(STR,...,...,STR).filter(FN).map(FN)` | `hand_audit_required` | true | scp.json |
| `te-fn` | 3 | `Te(...,...,...,FN)` | `hand_audit_required` | true | chezmoi.json |
| `filter` | 3 | `.filter(FN)` | `needs_new_transform_conditional_split` | false | chezmoi.json |
| `x-fn` | 3 | `x(...,...,...,FN)` | `hand_audit_required` | true | deno.json |
| `values-map` | 3 | `Object.values(...).map(FN)` | `hand_audit_required` | true | env.json |
| `from-map` | 3 | `Array.from(...).map(FN)` | `existing_transforms` | false | envchain.json |
| `o-arr` | 3 | `o(...,ARR)` | `hand_audit_required` | true | git-flow.json |
| `d` | 3 | `d(...)` | `hand_audit_required` | true | just.json |
| `arr-arr` | 3 | `(... ? ARR : ARR)` | `needs_new_transform_conditional_split` | false | kubectx.json |
| `resolve` | 3 | `Promise.resolve(...)` | `hand_audit_required` | true | oxlint.json |
| `ye-fn` | 3 | `ye(...,...,...,FN)` | `hand_audit_required` | true | pm2.json |
| `trim-split-slice-map` | 2 | `.trim().split(STR).slice(NUM).map(FN)` | `hand_audit_required` | true | cap.json |
| `unknown-2` | 2 | `(...)(...,...)` | `hand_audit_required` | true | dscacheutil.json |
| `he-fn` | 2 | `he(...,...,...,FN)` | `hand_audit_required` | true | limactl.json |
| `split-filter-map` | 2 | `.split(STR).filter(FN).map(FN)` | `hand_audit_required` | true | mdfind.json |
| `q-fn` | 2 | `q(...,...,...,FN)` | `hand_audit_required` | true | osqueryi.json |
| `entries-map-reduce` | 2 | `Object.entries(...).map(FN).reduce(FN,ARR).map(FN)` | `hand_audit_required` | true | pnpx.json |
| `map` | 2 | `(... ? .PROP.PROP.map(FN) : ARR)` | `hand_audit_required` | true | spring.json |
| `map-2` | 2 | `ARR.map(FN)` | `hand_audit_required` | true | trap.json |
| `we-fn` | 2 | `we(...,...,...,FN)` | `hand_audit_required` | true | vsce.json |
| `i-fn` | 1 | `I(...,...,...,FN)` | `hand_audit_required` | true | airflow.json |
| `slice-some-map` | 1 | `(.slice(NUM,...).some(FN) ? ARR.map(FN) : ARR.map(FN))` | `needs_new_transform_conditional_split` | false | brew.json |
| `startswith` | 1 | `((... \|\| ...) ? ... : (.startsWith(STR) ? ... : ...))` | `hand_audit_required` | true | chezmoi.json |
| `d-fn` | 1 | `D(...,...,...,FN)` | `hand_audit_required` | true | codesign.json |
| `entries-map` | 1 | `(... ? ARR : Object.entries(...).map(FN))` | `hand_audit_required` | true | deno.json |
| `map-3` | 1 | `.PROP.map(FN)` | `needs_new_transform_json_parse_simple` | false | deployctl.json |
| `includes-map` | 1 | `(.includes(...) ? [COMPUTED].map(FN) : ARR)` | `hand_audit_required` | true | dscacheutil.json |
| `startswith-keys-map` | 1 | `((... \|\| [COMPUTED].startsWith(STR)) ? Object.keys(...).map(FN) : ARR)` | `needs_new_transform_conditional_split` | false | echo.json |
| `isnan-isinteger` | 1 | `(Number.isNaN(...) ? ARR : (Number.isInteger(...) ? ((... \|\| ...) ? ARR : ARR...` | `needs_new_transform_conditional_split` | false | firefox.json |
| `f-fn` | 1 | `F(...,...,...,FN)` | `hand_audit_required` | true | man.json |
| `get` | 1 | `(.get(...) \|\| ARR)` | `hand_audit_required` | true | man.json |
| `ln-fn` | 1 | `ln(...,...,...,FN)` | `hand_audit_required` | true | pipenv.json |
| `map-4` | 1 | `ARR.map(FN).map(FN)` | `requires_runtime` | false | robot.json |
| `keys-reduce` | 1 | `Object.keys(...).reduce(FN,ARR)` | `needs_new_transform_json_parse_simple` | false | sake.json |
| `flatmap-filter-sort-map` | 1 | `.PROP.PROP.flatMap(FN).filter(FN).sort(FN).map(FN)` | `hand_audit_required` | true | spring.json |
| `pe-fn-2` | 1 | `Pe(...,...,...,FN)` | `hand_audit_required` | true | ts-node.json |
| `entries-map-2` | 1 | `Object.entries(...).map(FN)` | `hand_audit_required` | true | turbo.json |
| `trim-slice-split-filter-map` | 1 | `.trim().slice(NUM,...).split(STR).filter(FN).map(FN)` | `needs_new_transform_substring_slice` | false | v.json |
| `split-slice-map` | 1 | `.split(STR).slice(NUM).map(FN)` | `requires_runtime` | false | youtube-dl.json |

## Per-Verdict Breakdown (Top 5 Shapes Each)

### `hand_audit_required` (1712 generators, 57 shapes)

- **`parse-error`** (536): `<parse_error>`
- **`unknown`** (330): `...`
- **`parse-map`** (231): `JSON.parse(...).map(FN)`
- **`arr`** (150): `ARR`
- **`empty`** (73): ``

### `needs_new_transform_json_parse_simple` (100 generators, 6 shapes)

- **`parse-map-2`** (39): `JSON.parse(...).PROP.map(FN)`
- **`entries-sort-map`** (22): `Object.entries(...).sort(FN).map(FN)`
- **`keys-map`** (19): `Object.keys(...).map(FN)`
- **`split-map`** (18): `.split(STR).map(FN).map(FN)`
- **`map-3`** (1): `.PROP.map(FN)`

### `existing_transforms` (35 generators, 3 shapes)

- **`filter-map`** (20): `.filter(FN).map(FN)`
- **`from`** (12): `Array.from(...)`
- **`from-map`** (3): `Array.from(...).map(FN)`

### `requires_runtime` (24 generators, 4 shapes)

- **`trim-split-filter-map`** (15): `.trim().split(STR).filter(FN).map(FN)`
- **`trim-split-filter-map-2`** (7): `.trim().split(STR).filter(FN).map(FN).map(FN)`
- **`map-4`** (1): `ARR.map(FN).map(FN)`
- **`split-slice-map`** (1): `.split(STR).slice(NUM).map(FN)`

### `needs_new_transform_conditional_split` (9 generators, 5 shapes)

- **`filter`** (3): `.filter(FN)`
- **`arr-arr`** (3): `(... ? ARR : ARR)`
- **`slice-some-map`** (1): `(.slice(NUM,...).some(FN) ? ARR.map(FN) : ARR.map(FN))`
- **`startswith-keys-map`** (1): `((... \|\| [COMPUTED].startsWith(STR)) ? Object.keys(...).map(FN) : ARR)`
- **`isnan-isinteger`** (1): `(Number.isNaN(...) ? ARR : (Number.isInteger(...) ? ((... \|\| ...) ? ARR : ARR...`

### `needs_dotted_path_json_extract` (8 generators, 1 shapes)

- **`parse-map-3`** (8): `JSON.parse(...).PROP.PROP.map(FN)`

### `needs_new_transform_substring_slice` (1 generators, 1 shapes)

- **`trim-slice-split-filter-map`** (1): `.trim().slice(NUM,...).split(STR).filter(FN).map(FN)`

