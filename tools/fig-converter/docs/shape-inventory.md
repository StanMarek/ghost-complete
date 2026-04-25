# Shape Inventory — Phase 1 Classification Spike

> Canonical machine-readable data: [shape-inventory.json](./shape-inventory.json)

**Total generators analysed:** 1889
**Total distinct shapes:** 157

## Verdict Distribution

| Verdict | Count | % |
|---------|-------|---|
| `hand_audit_required` | 866 | 45.8% |
| `existing_transforms` | 614 | 32.5% |
| `requires_runtime` | 162 | 8.6% |
| `needs_new_transform_conditional_split` | 148 | 7.8% |
| `needs_new_transform_regex_match` | 52 | 2.8% |
| `needs_new_transform_substring_slice` | 33 | 1.7% |
| `needs_dotted_path_json_extract` | 14 | 0.7% |

## Shapes Table

| shape_id | count | fingerprint (≤80 chars) | verdict | has_fig_api_refs | example_spec |
|----------|-------|-------------------------|---------|------------------|--------------|
| `parse-map` | 174 | `JSON.parse(...).map(<OBJ>)` | `existing_transforms` | false | fly.json |
| `unknown` | 73 | `...` | `needs_new_transform_conditional_split` | false | bazel.json |
| `arr-with-fig-refs` | 69 | `ARR` | `hand_audit_required` | true | docker-compose.json |
| `startswith-split-map-with-fig-refs` | 68 | `(.startsWith(STR) ? ARR : .split(STR).map(<OBJ>))` | `hand_audit_required` | true | chezmoi.json |
| `unknown-with-fig-refs` | 65 | `...` | `hand_audit_required` | true | copilot.json |
| `split-map-parse` | 63 | `.split(STR).map(<JSON.parse(...)>).map(<OBJ>)` | `existing_transforms` | false | docker.json |
| `unknown-2` | 58 | `...` | `requires_runtime` | false | meteor.json |
| `ue-with-fig-refs` | 51 | `ue(...,...,...,<>)` | `hand_audit_required` | true | cloudflared.json |
| `empty` | 50 | `` | `existing_transforms` | false | arduino-cli.json |
| `match-map-split-trim` | 50 | `.match(REGEX).map(<.split(STR)>).map(<.map(<.trim()>)>).map(<OBJ>)` | `needs_new_transform_regex_match` | false | flutter.json |
| `map-with-fig-refs` | 45 | `o(...).map(<OBJ>)` | `hand_audit_required` | true | tsuru.json |
| `arr` | 44 | `ARR` | `existing_transforms` | false | ng.json |
| `typewithoutname-with-fig-refs` | 42 | `.typeWithoutName(...)` | `hand_audit_required` | true | kubecolor.json |
| `parse-map-2` | 42 | `JSON.parse(...).map(<.PROP>)` | `existing_transforms` | false | tsh.json |
| `a-m-with-fig-refs` | 41 | `A(...,...,...,<M(...,...,...,...)>)` | `hand_audit_required` | true | cargo.json |
| `includes-typewithoutname-with-fig-refs` | 40 | `(.includes(STR) ? .typeWithoutName(...) : .PROP)` | `hand_audit_required` | true | kubecolor.json |
| `keys-map` | 38 | `Object.keys(...).map(<...>)` | `needs_new_transform_conditional_split` | false | chezmoi.json |
| `unknown-with-fig-refs-2` | 34 | `...` | `hand_audit_required` | true | cargo.json |
| `de-with-fig-refs` | 28 | `de(...,...,...,<>)` | `hand_audit_required` | true | coda.json |
| `unknown-3` | 28 | `...` | `existing_transforms` | false | conda.json |
| `parse-map-3` | 26 | `JSON.parse(...).PROP.map(<OBJ>)` | `existing_transforms` | false | cargo.json |
| `r-d-with-fig-refs` | 25 | `R(...,...,...,<D(...,...)>)` | `hand_audit_required` | true | chezmoi.json |
| `pe-with-fig-refs` | 24 | `pe(...,...,...,<>)` | `hand_audit_required` | true | dotenv-vault.json |
| `unknown-with-fig-refs-3` | 23 | `...` | `hand_audit_required` | true | nx.json |
| `re-with-fig-refs` | 22 | `Re(...,...,...,<>)` | `hand_audit_required` | true | cargo.json |
| `entries-sort-localecompare-map` | 22 | `Object.entries(...).sort(<(... ? ... : (... ? NUM : .localeCompare(...)))>).m...` | `existing_transforms` | false | fly.json |
| `unknown-4` | 22 | `...` | `requires_runtime` | false | systemctl.json |
| `empty-with-fig-refs` | 19 | `` | `hand_audit_required` | true | bun.json |
| `trim-split-map-with-fig-refs` | 19 | `.trim().split(STR).map(<OBJ>)` | `hand_audit_required` | true | dscl.json |
| `filter-map` | 19 | `.filter(<...>).map(<OBJ>)` | `existing_transforms` | false | eslint.json |
| `empty-2` | 18 | `` | `existing_transforms` | false | bosh.json |
| `unknown-with-fig-refs-4` | 18 | `...` | `hand_audit_required` | true | chezmoi.json |
| `unknown-with-fig-refs-5` | 18 | `...` | `hand_audit_required` | true | rush.json |
| `arr-2` | 17 | `ARR` | `existing_transforms` | false | asdf.json |
| `arr-3` | 16 | `ARR` | `existing_transforms` | false | bun.json |
| `empty-3` | 16 | `` | `requires_runtime` | false | bun.json |
| `arr-arr` | 16 | `(... ? ARR : ARR)` | `needs_new_transform_conditional_split` | false | kubecolor.json |
| `le-with-fig-refs` | 15 | `le(...,...,...,<>)` | `hand_audit_required` | true | asr.json |
| `parse-error` | 15 | `<parse_error>` | `hand_audit_required` | false | dotnet.json |
| `trim-split-filter-map` | 15 | `.trim().split(STR).filter(<(... && ...)>).map(<OBJ>)` | `requires_runtime` | false | gem.json |
| `keys-map-2` | 14 | `Object.keys(...).map(<OBJ>)` | `existing_transforms` | false | cargo.json |
| `ce-with-fig-refs` | 12 | `ce(...,...,...,<>)` | `hand_audit_required` | true | ansible-playbook.json |
| `from` | 12 | `Array.from(...)` | `existing_transforms` | false | expo-cli.json |
| `split-map` | 11 | `.split(STR).map(<.split(STR)>).map(<OBJ>)` | `requires_runtime` | false | chown.json |
| `unknown-5` | 10 | `...` | `needs_new_transform_substring_slice` | false | deno.json |
| `ge-with-fig-refs` | 9 | `ge(...,...,...,<>)` | `hand_audit_required` | true | apt.json |
| `ae-with-fig-refs` | 9 | `ae(...,...,...,<>)` | `hand_audit_required` | true | dotnet.json |
| `split-map-filter-with-fig-refs` | 9 | `.split(STR).map(<>).filter(<(... && ...)>).map(<OBJ>)` | `hand_audit_required` | true | kitty.json |
| `parse-map-4` | 9 | `JSON.parse(...).PROP.map(<>)` | `existing_transforms` | false | multipass.json |
| `split-map-2` | 8 | `.split(STR).map(<OBJ>)` | `requires_runtime` | false | bun.json |
| `parse-map-5` | 8 | `JSON.parse(...).map(<OBJ>)` | `existing_transforms` | false | elm-json.json |
| `parse-map-6` | 8 | `JSON.parse(...).PROP.PROP.map(<OBJ>)` | `needs_dotted_path_json_extract` | false | expo-cli.json |
| `unknown-6` | 8 | `...` | `needs_new_transform_substring_slice` | false | gpg.json |
| `parse-map-7` | 7 | `JSON.parse(...).PROP.map(<OBJ>)` | `existing_transforms` | false | amplify.json |
| `trim-split-filter-startswith-map-replace` | 7 | `.trim().split(STR).filter(<.startsWith(...)>).map(<.replace(REGEX,STR)>).map(...` | `requires_runtime` | false | apt.json |
| `split-map-with-fig-refs` | 7 | `.split(STR).map(<OBJ>)` | `hand_audit_required` | true | asdf.json |
| `unknown-7` | 7 | `...` | `existing_transforms` | false | cordova.json |
| `empty-with-fig-refs-2` | 7 | `` | `hand_audit_required` | true | deno.json |
| `k-w-with-fig-refs` | 7 | `k(...,...,...,<W(...,...)>)` | `hand_audit_required` | true | esbuild.json |
| `arr-with-fig-refs-2` | 7 | `ARR` | `hand_audit_required` | true | gh.json |
| `from-map` | 7 | `Array.from(...).map(<OBJ>)` | `needs_new_transform_substring_slice` | false | n.json |
| `split-map-3` | 6 | `.split(STR).map(<OBJ>)` | `existing_transforms` | false | assimp.json |
| `arr-4` | 6 | `(... ? ... : ARR)` | `needs_new_transform_conditional_split` | false | bun.json |
| `includes-with-fig-refs` | 6 | `(.includes(STR) ? ... : ...)` | `hand_audit_required` | true | chezmoi.json |
| `me-with-fig-refs` | 6 | `me(...,...,...,<>)` | `hand_audit_required` | true | pnpx.json |
| `split-map-parse-2` | 6 | `.split(STR).map(<JSON.parse(...)>).map(<OBJ>)` | `existing_transforms` | false | podman.json |
| `parse-map-split` | 6 | `JSON.parse(...).PROP.PROP.map(<.split(STR)[COMPUTED]>)` | `needs_dotted_path_json_extract` | false | scarb.json |
| `trim-split-map` | 5 | `.trim().split(STR)[COMPUTED].split(STR)[COMPUTED].split(STR)[COMPUTED].split(...` | `existing_transforms` | false | bat.json |
| `fe-with-fig-refs` | 5 | `Fe(...,...,...,<>)` | `hand_audit_required` | true | bun.json |
| `from-map-2` | 5 | `Array.from(...).map(<OBJ>)` | `existing_transforms` | false | envchain.json |
| `filter-trim-tolowercase-startswith-map-with-fig-refs` | 5 | `await c(STR,...,...,STR).filter(<(.trim().toLowerCase().startsWith(STR) && .....` | `hand_audit_required` | true | kitty.json |
| `unknown-8` | 5 | `...` | `requires_runtime` | false | wd.json |
| `fe-with-fig-refs-2` | 4 | `fe(...,...,...,<>)` | `hand_audit_required` | true | dotnet.json |
| `parse-map-with-fig-refs` | 4 | `(... ? ARR : JSON.parse(...).map(...))` | `hand_audit_required` | true | gh.json |
| `empty-4` | 4 | `` | `needs_new_transform_conditional_split` | false | ipatool.json |
| `arr-5` | 4 | `ARR` | `needs_new_transform_conditional_split` | false | kubecolor.json |
| `typewithoutname-with-fig-refs-2` | 4 | `.typeWithoutName(STR)` | `hand_audit_required` | true | kubecolor.json |
| `arr-6` | 4 | `ARR` | `requires_runtime` | false | make.json |
| `keys-map-3` | 4 | `Object.keys(...).map(<OBJ>)` | `requires_runtime` | false | projj.json |
| `t-q-with-fig-refs` | 4 | `T(...,...,...,<q(...,...)>)` | `hand_audit_required` | true | scc.json |
| `filter-trim-tolowercase-startswith-map-with-fig-refs-2` | 4 | `await d(STR,...,...,STR).filter(<(.trim().toLowerCase().startsWith(STR) && .....` | `hand_audit_required` | true | scp.json |
| `j-q-with-fig-refs` | 4 | `j(...,...,...,<q(...,...,...,...)>)` | `hand_audit_required` | true | swift.json |
| `empty-with-fig-refs-3` | 3 | `` | `hand_audit_required` | true | cargo.json |
| `te-arr-with-fig-refs` | 3 | `Te(...,...,...,<((... && ...) ? ... : ARR)>)` | `hand_audit_required` | true | chezmoi.json |
| `includes` | 3 | `((.includes(STR) \|\| .includes(STR)) ? ARR : ARR)` | `needs_new_transform_conditional_split` | false | chezmoi.json |
| `filter-has` | 3 | `.filter(<(.has(...) ? ... : ...)>)` | `needs_new_transform_substring_slice` | false | chezmoi.json |
| `unknown-with-fig-refs-6` | 3 | `...` | `hand_audit_required` | true | dd.json |
| `x-j-with-fig-refs` | 3 | `x(...,...,...,<j(...,...)>)` | `hand_audit_required` | true | deno.json |
| `o-arr-with-fig-refs` | 3 | `o(...,ARR)` | `hand_audit_required` | true | git-flow.json |
| `arr-7` | 3 | `ARR` | `requires_runtime` | false | goto.json |
| `d-with-fig-refs` | 3 | `d(...)` | `hand_audit_required` | true | just.json |
| `unknown-with-fig-refs-7` | 3 | `...` | `hand_audit_required` | true | nx.json |
| `resolve-with-fig-refs` | 3 | `Promise.resolve(...)` | `hand_audit_required` | true | oxlint.json |
| `ye-with-fig-refs` | 3 | `ye(...,...,...,<>)` | `hand_audit_required` | true | pm2.json |
| `parse-map-8` | 3 | `JSON.parse(...).map(<OBJ>)` | `existing_transforms` | false | watson.json |
| `keys-map-4` | 2 | `Object.keys(...).map(<OBJ>)` | `existing_transforms` | false | ansible-doc.json |
| `g-te-with-fig-refs` | 2 | `G(...,...,...,<te(...,...)>)` | `hand_audit_required` | true | bun.json |
| `g-te-with-fig-refs-2` | 2 | `G(...,...,...,<te(...,...)>)` | `hand_audit_required` | true | bun.json |
| `trim-split-slice-map-with-fig-refs` | 2 | `.trim().split(STR).slice(NUM).map(<OBJ>)` | `hand_audit_required` | true | cap.json |
| `empty-with-fig-refs-4` | 2 | `` | `hand_audit_required` | true | dcli.json |
| `split-map-parse-3` | 2 | `.split(STR).map(<JSON.parse(...)>).map(<...>)` | `existing_transforms` | false | docker.json |
| `a-g-with-fig-refs` | 2 | `A(...,...,...,<G(...,...,...,...)>)` | `hand_audit_required` | true | gh.json |
| `unknown-9` | 2 | `...` | `existing_transforms` | false | lerna.json |
| `he-with-fig-refs` | 2 | `he(...,...,...,<>)` | `hand_audit_required` | true | limactl.json |
| `q-j-with-fig-refs` | 2 | `q(...,...,...,<j(...,...)>)` | `hand_audit_required` | true | osqueryi.json |
| `entries-map-reduce-with-fig-refs` | 2 | `Object.entries(...).map(<...>).reduce(<ARR>,ARR).map(<OBJ>)` | `hand_audit_required` | true | pnpx.json |
| `parse-map-9` | 2 | `JSON.parse(...).map(<OBJ>)` | `requires_runtime` | false | shadcn-ui.json |
| `map-with-fig-refs-2` | 2 | `(... ? .PROP.PROP.map(<OBJ>) : ARR)` | `hand_audit_required` | true | spring.json |
| `values-map-with-fig-refs` | 2 | `Object.values(...).map(<OBJ>)` | `hand_audit_required` | true | tailscale.json |
| `we-with-fig-refs` | 2 | `we(...,...,...,<>)` | `hand_audit_required` | true | vsce.json |
| `arr-8` | 2 | `ARR` | `existing_transforms` | false | yarn.json |
| `i-h-with-fig-refs` | 1 | `I(...,...,...,<H(...,...)>)` | `hand_audit_required` | true | airflow.json |
| `slice-some-map` | 1 | `(.slice(NUM,...).some(<...>) ? ARR.map(<OBJ>) : ARR.map(<OBJ>))` | `needs_new_transform_substring_slice` | false | brew.json |
| `arr-with-fig-refs-3` | 1 | `ARR` | `hand_audit_required` | true | cargo.json |
| `startswith-with-fig-refs` | 1 | `((... \|\| ...) ? ... : (.startsWith(STR) ? ... : ...))` | `hand_audit_required` | true | chezmoi.json |
| `d-c-with-fig-refs` | 1 | `D(...,...,...,<C(...,...)>)` | `hand_audit_required` | true | codesign.json |
| `empty-5` | 1 | `` | `requires_runtime` | false | dapr.json |
| `parse-map-10` | 1 | `JSON.parse(...).map(<OBJ>)` | `requires_runtime` | false | degit.json |
| `entries-map-with-fig-refs` | 1 | `(... ? ARR : Object.entries(...).map(<OBJ>))` | `hand_audit_required` | true | deno.json |
| `parse-map-11` | 1 | `JSON.parse(...).PROP.map(<OBJ>)` | `existing_transforms` | false | deno.json |
| `map` | 1 | `.PROP.map(<OBJ>)` | `existing_transforms` | false | deployctl.json |
| `includes-map-with-fig-refs` | 1 | `(.includes(...) ? [COMPUTED].map(<OBJ>) : ARR)` | `hand_audit_required` | true | dscacheutil.json |
| `unknown-with-fig-refs-8` | 1 | `(...)(...,...)` | `hand_audit_required` | true | dscacheutil.json |
| `startswith-keys-map` | 1 | `((... \|\| [COMPUTED].startsWith(STR)) ? Object.keys(...).map(<OBJ>) : ARR)` | `needs_new_transform_conditional_split` | false | echo.json |
| `values-map` | 1 | `Object.values(...).map(<OBJ>)` | `existing_transforms` | false | env.json |
| `k-w-with-fig-refs-2` | 1 | `k(...,...,...,<W(...,...)>)` | `hand_audit_required` | true | esbuild.json |
| `j-p-with-fig-refs` | 1 | `j(...,...,...,<P(...,...,...,...)>)` | `hand_audit_required` | true | file.json |
| `isnan-isinteger` | 1 | `(Number.isNaN(...) ? ARR : (Number.isInteger(...) ? ((... \|\| ...) ? ARR : ARR...` | `needs_new_transform_conditional_split` | false | firefox.json |
| `map-with-fig-refs-3` | 1 | `h(ARR,<...>).map(<((.PROP && ...) ? OBJ : OBJ)>)` | `hand_audit_required` | true | fnm.json |
| `map-reverse-with-fig-refs` | 1 | `ARR.map(<OBJ>).reverse()` | `hand_audit_required` | true | fvm.json |
| `arr-9` | 1 | `ARR` | `needs_new_transform_substring_slice` | false | git-cliff.json |
| `filter-every-includes-map` | 1 | `.filter(<.every(<.includes(...)>)>).map(<>)` | `requires_runtime` | false | j.json |
| `unknown-10` | 1 | `...` | `needs_new_transform_regex_match` | false | kill.json |
| `trim-split-map-2` | 1 | `.trim().split(STR).map(<OBJ>)` | `needs_new_transform_substring_slice` | false | killall.json |
| `map-2` | 1 | `ARR.map(<OBJ>)` | `existing_transforms` | false | lsof.json |
| `map-3` | 1 | `.map(<OBJ>)` | `needs_new_transform_regex_match` | false | lsof.json |
| `map-4` | 1 | `.map(<OBJ>)` | `needs_new_transform_conditional_split` | false | lsof.json |
| `f-e-with-fig-refs` | 1 | `F(...,...,...,<E(...,...)>)` | `hand_audit_required` | true | man.json |
| `get-with-fig-refs` | 1 | `(.get(...) \|\| ARR)` | `hand_audit_required` | true | man.json |
| `split-filter-endswith-map` | 1 | `.split(STR).filter(<.endsWith(STR)>).map(<OBJ>)` | `requires_runtime` | false | mdfind.json |
| `arr-arr-2` | 1 | `(... ? ARR : ARR)` | `needs_new_transform_substring_slice` | false | nx.json |
| `parse-map-with-fig-refs-2` | 1 | `JSON.parse(...).map(<OBJ>)` | `hand_audit_required` | true | op.json |
| `ln-with-fig-refs` | 1 | `ln(...,...,...,<>)` | `hand_audit_required` | true | pipenv.json |
| `unknown-with-fig-refs-9` | 1 | `(...)(...,...)` | `hand_audit_required` | true | pkgutil.json |
| `arr-with-fig-refs-4` | 1 | `ARR` | `hand_audit_required` | true | rich.json |
| `map-5` | 1 | `ARR.map(<[COMPUTED]>).map(<OBJ>)` | `requires_runtime` | false | robot.json |
| `keys-reduce` | 1 | `Object.keys(...).reduce(<ARR>,ARR)` | `existing_transforms` | false | sake.json |
| `t-q-with-fig-refs-2` | 1 | `T(...,...,...,<q(...,...)>)` | `hand_audit_required` | true | scc.json |
| `flatmap-filter-sort-localecompare-map-with-fig-refs` | 1 | `.PROP.PROP.flatMap(<...>).filter(<...>).sort(<.PROP.localeCompare(...)>).map(...` | `hand_audit_required` | true | spring.json |
| `split-filter-test-map-with-fig-refs` | 1 | `.split(STR).filter(<.test(...)>).map(<OBJ>)` | `hand_audit_required` | true | tldr.json |
| `map-with-fig-refs-4` | 1 | `ARR.map(<OBJ>)` | `hand_audit_required` | true | trap.json |
| `pe-with-fig-refs-2` | 1 | `Pe(...,...,...,<>)` | `hand_audit_required` | true | ts-node.json |
| `entries-map-with-fig-refs-2` | 1 | `Object.entries(...).map(<...>)` | `hand_audit_required` | true | turbo.json |
| `trim-slice-split-filter-map` | 1 | `.trim().slice(NUM,...).split(STR).filter(<...>).map(<OBJ>)` | `needs_new_transform_substring_slice` | false | v.json |
| `arr-10` | 1 | `ARR` | `requires_runtime` | false | yarn.json |
| `map-6` | 1 | `ARR.map(<OBJ>)` | `needs_new_transform_conditional_split` | false | ykman.json |
| `split-slice-map` | 1 | `.split(STR).slice(NUM).map(<OBJ>)` | `requires_runtime` | false | youtube-dl.json |

## Per-Verdict Breakdown (Top 5 Shapes Each)

### `hand_audit_required` (866 generators, 81 shapes)

- **`arr-with-fig-refs`** (69): `ARR`
- **`startswith-split-map-with-fig-refs`** (68): `(.startsWith(STR) ? ARR : .split(STR).map(<OBJ>))`
- **`unknown-with-fig-refs`** (65): `...`
- **`ue-with-fig-refs`** (51): `ue(...,...,...,<>)`
- **`map-with-fig-refs`** (45): `o(...).map(<OBJ>)`

### `existing_transforms` (614 generators, 32 shapes)

- **`parse-map`** (174): `JSON.parse(...).map(<OBJ>)`
- **`split-map-parse`** (63): `.split(STR).map(<JSON.parse(...)>).map(<OBJ>)`
- **`empty`** (50): ``
- **`arr`** (44): `ARR`
- **`parse-map-2`** (42): `JSON.parse(...).map(<.PROP>)`

### `requires_runtime` (162 generators, 19 shapes)

- **`unknown-2`** (58): `...`
- **`unknown-4`** (22): `...`
- **`empty-3`** (16): ``
- **`trim-split-filter-map`** (15): `.trim().split(STR).filter(<(... && ...)>).map(<OBJ>)`
- **`split-map`** (11): `.split(STR).map(<.split(STR)>).map(<OBJ>)`

### `needs_new_transform_conditional_split` (148 generators, 11 shapes)

- **`unknown`** (73): `...`
- **`keys-map`** (38): `Object.keys(...).map(<...>)`
- **`arr-arr`** (16): `(... ? ARR : ARR)`
- **`arr-4`** (6): `(... ? ... : ARR)`
- **`empty-4`** (4): ``

### `needs_new_transform_regex_match` (52 generators, 3 shapes)

- **`match-map-split-trim`** (50): `.match(REGEX).map(<.split(STR)>).map(<.map(<.trim()>)>).map(<OBJ>)`
- **`unknown-10`** (1): `...`
- **`map-3`** (1): `.map(<OBJ>)`

### `needs_new_transform_substring_slice` (33 generators, 9 shapes)

- **`unknown-5`** (10): `...`
- **`unknown-6`** (8): `...`
- **`from-map`** (7): `Array.from(...).map(<OBJ>)`
- **`filter-has`** (3): `.filter(<(.has(...) ? ... : ...)>)`
- **`slice-some-map`** (1): `(.slice(NUM,...).some(<...>) ? ARR.map(<OBJ>) : ARR.map(<OBJ>))`

### `needs_dotted_path_json_extract` (14 generators, 2 shapes)

- **`parse-map-6`** (8): `JSON.parse(...).PROP.PROP.map(<OBJ>)`
- **`parse-map-split`** (6): `JSON.parse(...).PROP.PROP.map(<.split(STR)[COMPUTED]>)`

