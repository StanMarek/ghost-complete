# Native Providers

Phase 3A introduced a native-provider pipeline in `gc-suggest` that replaces a subset of Fig specs' `requires_js` generators with async Rust code. Providers eliminate the JS runtime dependency for commands whose completion source is either a well-behaved subprocess (stable output, no auth, no pagination, no file parsing) or a local project file that can be parsed directly.

Reference implementation: [`crates/gc-suggest/src/providers/arduino_cli.rs`](../crates/gc-suggest/src/providers/arduino_cli.rs) — mirrors the full pattern (subprocess runner, pure extractor, test-injection binary override).

## Adding a new provider

1. **Confirm eligibility.** The command must pass plan §3A's criteria: single subprocess call, no auth, no pagination, stable text output, no file-system parsing, bounded size (<10K lines), no new transitive deps. Candidates live in [`tools/fig-converter/docs/candidate-providers.json`](../tools/fig-converter/docs/candidate-providers.json) with per-criterion booleans from the Phase 1 spike.

2. **Create the provider file.** Add `crates/gc-suggest/src/providers/<name>.rs`. Follow `arduino_cli.rs`'s shape:
   - `const X_TIMEOUT_MS: u64 = 2_000;` — all provider subprocesses share the 2s default.
   - `pub(crate) async fn run_x_with_binary(cwd: &Path, binary: &str) -> Option<T>` — the subprocess runner. The production binary literal (e.g., `"arduino-cli"`) is passed at the `generate()` call site; tests pass a deliberately nonexistent path to exercise the spawn-failure path without mutating `$PATH`. No plain `run_x` wrapper is needed.
   - `fn x_from_output(parsed: T) -> Vec<Suggestion>` — pure extractor, testable without spawning.
   - `pub struct X;` — unsuffixed, one per user-visible completion source (e.g., `ArduinoCliBoards`, `ArduinoCliPorts`, `DefaultsDomains`, `MambaEnvs`, `PandocInputFormats`). When one subprocess feeds multiple providers (arduino-cli's `board list` drives both boards and ports), each provider is a separate struct that shares the runner and extracts its own projection.
   - `impl Provider for X` with `name()` and `async fn generate(ctx)` that delegates to the `generate_with_binary` test seam.
   - `impl X { pub(crate) async fn generate_with_binary(&self, ctx: &ProviderCtx, binary: &str) -> Result<Vec<Suggestion>> { ... } }` — the shared test seam. `generate` calls it with the real binary, tests call it with an injected path. Keeps the spawn-failure contract (`Ok(Vec::new())` on `None` from the runner) in one place.

3. **Register.** In `crates/gc-suggest/src/providers/mod.rs`:
   - `pub mod x;`
   - Add a variant to `ProviderKind`.
   - Add the variant to `ProviderKind::ALL`.
   - Add the string mapping in `ProviderKind::type_str()`.
   - Add the dispatcher arm in `resolve`.

4. **Test.** Pure-function tests for the extractor (happy path, empty input, malformed input, missing-field filtering). One subprocess-failure test using `run_x_with_binary(tmp.path(), "/nonexistent/x")` — never mutate `$PATH`.

5. **Wire the converter.** In `tools/fig-converter/src/native-map.js`, add an entry to `NATIVE_GENERATOR_MAP` keyed on `script.slice(0, 2).join(' ')`. For providers where the same subprocess maps to different providers via `postProcess` source (arduino-cli boards vs. ports), add a regex check on the third `postProcessSource` argument. For spec-name-scoped mappings (e.g., `conda env list` routes to `mamba_envs` only in `mamba.json`), extend `SPEC_SCOPED_MAP`.

6. **Regenerate specs.** `cd tools/fig-converter && npm run convert`. Spot-check that the affected generators now read `{"type": "<name>"}` with no `script`, `requires_js`, or `js_source` fields.

Script + transform lowering is tracked separately from native providers. A generator that stays as `script` / `script_template` plus `transforms` after a JS post-processor was lowered may carry `_lowered_from_requires_js: true`, but it must not keep `requires_js`. `ghost-complete status --json` reports those cases as `requires_js_generators_lowered_to_transforms` and `counters.lowered_to_transforms`; provider rewrites should not use that marker.

## Generator-spec `params`

Generators may carry a flat string-to-string `params` map that the engine threads into the dispatched provider via `ProviderCtx::params`. This is the channel ux-13/14 spec-driven providers (e.g. the planned `AwsSdk` provider) will use to route on structured selection (service, region, profile) without inventing a new generator schema per command. As of the ux-9b precursor no in-tree provider reads the field — the plumbing is purely additive.

JSON shape on a generator:

```json
{
  "type": "aws_sdk",
  "params": {
    "service": "s3",
    "region": "us-east-1"
  }
}
```

`params` defaults to `{}` (`#[serde(default)]`) so existing specs remain valid without the field. Key order is preserved deterministically via `BTreeMap`, which matters for the cache-key hash described below.

Reading from a `Provider`:

```rust
async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
    let service = ctx.params.get("service").map(String::as_str).unwrap_or("");
    let region = ctx.params.get("region").map(String::as_str);
    // ...
    let cache_key = (self.name(), ctx.params_hash());
    // ...
}
```

`ProviderCtx::params_hash()` returns a `u64` over the sorted key/value pairs, suitable for in-process generator caches keyed on the spec's parameter selection. Cross-process stability is not a contract — the hash is stable within a single process only.

## AWS providers

UX-13 adds two AWS-specific provider types:

| Type string | Source | Notes |
|---|---|---|
| `aws_sdk` | Native AWS SDK calls | Experimental and opt-in through `[experimental] aws_sdk_provider = true`. Can make outbound HTTPS calls to AWS endpoints. |
| `aws_profile_names` | AWS profile files | Reads profile names from AWS config/credentials files for `aws --profile <Tab>`-style completions. It does not resolve credentials or call AWS. |

`aws_sdk` replaces selected `aws` CLI script generators with typed SDK calls.
The provider reads operation details from generator `params` such as service,
operation, response field, and cache shape. The SDK path is default-off for
one release:

```toml
[experimental]
aws_sdk_provider = false
aws_sdk_fallback_to_cli = true
```

With the default config, Ghost Complete does not make outbound AWS SDK calls.
When `aws_sdk_provider = true`, completions use the normal AWS credential chain:
environment credentials (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, optional
`AWS_SESSION_TOKEN`), selected profiles (`AWS_PROFILE` or
`AWS_DEFAULT_PROFILE`), regions (`AWS_REGION` or `AWS_DEFAULT_REGION`), and
the AWS config files. Profile names come from `~/.aws/config` sections such as
`[profile dev]` and `~/.aws/credentials` sections such as `[dev]`; the file
locations can be overridden with `AWS_CONFIG_FILE` and
`AWS_SHARED_CREDENTIALS_FILE`.

`aws_sdk_fallback_to_cli = true` keeps the current `aws` CLI script path as a
last resort when SDK completions cannot run because credentials, profile
selection, region configuration, or network access are unavailable. Users who
do not install the AWS CLI can set it to `false`; in that mode SDK failures
return no dynamic AWS suggestions instead of shelling out.

`ghost-complete doctor` includes an AWS credential/profile health line. The
check is intentionally local-only: it inspects relevant environment variables,
AWS file existence, profile names, and visible region settings, but it never
loads the AWS SDK and never makes a live AWS API call. Expired sessions, SSO
login state, AssumeRole failures, and service authorization are still runtime
conditions surfaced by the provider itself.

## Local-project providers

A subset of providers do not shell out at all — they parse a project file in the user's CWD ancestry. Reference implementation: [`crates/gc-suggest/src/providers/local_project/`](../crates/gc-suggest/src/providers/local_project/) (UX-5). Same `Provider` trait as the subprocess providers, with two pattern differences:

1. **Ancestor walk for file discovery.** Each provider walks up to 32 ancestors of `ctx.cwd` to find its file (`Makefile` / `package.json` / `Cargo.toml`). The walk is bounded to defuse pathological symlink loops.
2. **Provider-private caching, no TTL.** `MakefileTargets` and `NpmScripts` use module-private `MtimeCache<T>` instances keyed by absolute file path with `(mtime, size)` invalidation. Cached entries remain valid until the source file changes — a hand-edit to `Makefile` is picked up on the next keystroke. They are FIFO-evicted at 64 entries per provider as a hard cap. `CargoWorkspaceMembers` uses a separate `CargoCache` with per-path stamps for the root manifest, member manifests, glob-prefix directories, and missing-path probes; a hit is valid only when every recorded stamp still matches.

### v1 providers

| Type string | File | Replaces |
|---|---|---|
| `makefile_targets` | `GNUmakefile` / `makefile` / `Makefile` (GNU make's documented precedence) | `requires_js: true` generator that shells out to `make -qp` and post-processes the output |
| `npm_scripts` | `package.json` | `bash -c "until [[ -f package.json ]]..."` script with a JS post-processor that projects `scripts` keys |
| `cargo_workspace_members` | `Cargo.toml` (nearest ancestor with `[workspace]`, falls back to nearest `Cargo.toml` for single-package crates) | `cargo metadata --format-version 1 --no-deps` invocation that JSON-parses to extract `packages[].name` |

### ux-14 tool providers

Phase 5 adds typed providers for CLI state that was previously fetched through
Fig JS generators. The scoped regenerated corpus now contains 621 native Rust
generator entries, including 448 dispatched through the provider registry.

| Type string | Provider file | Source | Notes |
|---|---|---|---|
| `cargo_targets` | `cargo_metadata.rs` | `cargo metadata --format-version 1 --no-deps` | Reads `params.kind` (`bin`, `example`, `test`, `bench`, `lib`). |
| `cargo_features` | `cargo_metadata.rs` | `cargo metadata --format-version 1 --no-deps` | Features for the active package. |
| `npm_dependencies` | `npm_local.rs` | nearest `package.json` | Keys of `dependencies`. |
| `npm_dev_dependencies` | `npm_local.rs` | nearest `package.json` | Keys of `devDependencies`. |
| `npm_all_dependencies` | `npm_local.rs` | nearest `package.json` | Union of dependency fields used by npm remove flows. |
| `docker_images` | `docker.rs` | `docker images --format '{{json .}}'` | Supports `params.binary = "podman"`. |
| `docker_containers` | `docker.rs` | `docker ps --all --format '{{json .}}'` | Supports Docker and Podman specs. |
| `docker_running_containers` | `docker.rs` | `docker ps --filter status=running --format '{{json .}}'` | Used by stop/kill-style flows. |
| `docker_networks` | `docker.rs` | `docker network ls --format '{{json .}}'` | Network names and IDs. |
| `docker_volumes` | `docker.rs` | `docker volume ls --format '{{json .}}'` | Volume names. |
| `k8s_resources` | `kubectl.rs` | `kubectl api-resources` | JSON/name output with legacy text fallback. |
| `k8s_contexts` | `kubectl.rs` | `kubectl config get-contexts -o name` | Honors `KUBECONFIG`. |
| `k8s_pods` | `kubectl.rs` | `kubectl get pods -o json` | Cluster state with short TTL. |
| `k8s_namespaces` | `kubectl.rs` | `kubectl get namespaces -o json` | Namespace names. |
| `k8s_nodes` | `kubectl.rs` | `kubectl get nodes -o json` | Node names. |
| `k8s_services` | `kubectl.rs` | `kubectl get services -o json` | Service names. |
| `tmux_sessions` | `tmux_state.rs` | `tmux list-sessions -F ...` | Skips when `TMUX` is unset. |
| `tmux_windows` | `tmux_state.rs` | `tmux list-windows -a -F ...` | Session/window targets. |
| `tmux_panes` | `tmux_state.rs` | `tmux list-panes -a -F ...` | Pane targets. |
| `tmux_clients` | `tmux_state.rs` | `tmux list-clients -F ...` | Client targets. |
| `systemd_units` | `systemd_units.rs` | `systemctl list-units -o json --all --full` | Falls back to text on old systemd. |
| `systemd_user_units` | `systemd_units.rs` | `systemctl list-units ... --user` | User unit scope. |
| `systemd_active_units` | `systemd_units.rs` | `systemctl list-units ...` | Filters active units. |
| `brew_formulae_installed` | `brew.rs` | `brew list --formula` | Installed formulae. |
| `brew_casks_installed` | `brew.rs` | `brew list --cask` | Installed casks. |
| `brew_formulae_searchable` | `brew.rs` | `brew search` | Searchable formulae, capped in provider code. |
| `dscl_users` | `dscl_principals.rs` | `dscl . list /Users` | Filters system users unless opted in. |
| `dscl_groups` | `dscl_principals.rs` | `dscl . list /Groups` | Filters system groups unless opted in. |

### When to add a new local-project provider

The pattern is a fit when:

- The completion source is a project file the user obviously owns (`docker-compose.yml`, `justfile`, `pnpm-workspace.yaml`, `tsconfig.json`).
- A pure parser is straightforward — no recursive variable expansion, no executing user-provided code.
- mtime is a safe invalidation signal (the file is hand-edited, not regenerated by a build step that touches mtime without changing content).

Skip the local-project pattern (and use a script provider or stay with `requires_js`) when:

- The source is remote (`kubectl contexts`, `aws profiles`).
- The parse needs the host tool's resolver (e.g., `cargo metadata` for full transitive dependency info — but the v1 cargo provider only needs workspace members, which is parseable directly).
- The user expects the completion to reflect tool state that doesn't show up in the file (active container, current git worktree).

### Wiring a local-project provider

1. Create `crates/gc-suggest/src/providers/local_project/<source>.rs` mirroring `makefile.rs` / `npm_scripts.rs` / `cargo_workspace.rs`. Export a `pub struct <Source>;` implementing `Provider`, plus a `pub(crate) async fn generate_with_root(root: &Path)` test seam.
2. Add the module declaration in `local_project/mod.rs`.
3. Add the `ProviderKind` variant, `ProviderKind::ALL` entry, `ProviderKind::type_str()` arm, and `resolve` dispatcher arm in `providers/mod.rs` (same as for subprocess providers).
4. Hook up the converter in `tools/fig-converter/src/native-map.js`. For script-array shapes use `NATIVE_GENERATOR_MAP` or `SPEC_SCOPED_MAP`; for `_custom` / `_scriptFunction` (where there is no `script` array), extend `matchNativeFromJsSource` with a regex on the JS source.
5. Run `npm --prefix tools/fig-converter test` and `cargo test -p gc-suggest`.

If the upstream specs you're rewriting carry hand-curated `priority` fields that the regen would drop, use the surgical patch script at `tools/fig-converter/scripts/patch-local-project-providers.mjs` as a template — it rewrites only matching generators in place, preserving every other field.

## Caching

Providers currently bypass the `CacheConfig` layer on `GeneratorSpec`. That config (`ttl_seconds`, `cache_by_directory`) applies only to script-based generators. If a provider's underlying subprocess is expensive enough to warrant caching, add it inside the provider itself — either a module-level `Mutex<LruCache<PathBuf, (Instant, T)>>` or a `tokio::sync::OnceCell` guarded by timestamp. For local-project providers, `MakefileTargets` and `NpmScripts` use the shared `MtimeCache<T>` helper defined in `local_project/mod.rs`, while `CargoWorkspaceMembers` uses its own `CargoCache` with per-path stamps. Keep cache logic private to the provider module; don't reach into `gc-suggest::cache`. If you find yourself wanting shared caching across providers, that's a signal to design a dedicated provider-level cache API in a follow-up phase.

## Converter eligibility

A generator in the fig source qualifies for native rewriting when `matchNativeGenerator(specName, gen.script, gen._postProcessSource)` returns a non-null `{type: "..."}`. The matcher consults (in order): the arduino-cli postProcess disambiguator, the `SPEC_SCOPED_MAP` for spec-name-scoped mappings, then the global `NATIVE_GENERATOR_MAP`. Any keys not in the map fall through to the existing script/transform/js-source pipeline — providers do not steal matches from postProcess pattern detection.

The `NO_OP_DRIVER_FLAGS` set in `native-map.js` strips driver flags (e.g., `git --no-optional-locks`) before keying the map, so variants like `["git", "--no-optional-locks", "branch"]` still route to `git_branches`. Add to this set when you discover a spec passing no-op flags to a command you've already mapped.

## Error handling

Provider failures must never propagate. Every error path (spawn failure, timeout, non-zero exit, parse error) logs via `tracing::warn!` with structured fields and returns `Ok(Vec::new())`. See [`crates/gc-suggest/src/git.rs`](../crates/gc-suggest/src/git.rs) for the canonical pattern the Phase 3A providers mirror. The suggest engine depends on this contract — a provider that bubbles an `anyhow::Error` will tank the completion pipeline.
