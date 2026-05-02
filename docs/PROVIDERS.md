# Native Providers

Phase 3A introduced a native-provider pipeline in `gc-suggest` that replaces a subset of Fig specs' `requires_js` generators with async Rust code. Providers eliminate the JS runtime dependency for commands whose completion source is a well-behaved subprocess (stable output, no auth, no pagination, no file parsing).

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
   - Add the string→kind arm in `kind_from_type_str`.
   - Add the dispatcher arm in `resolve`.

4. **Test.** Pure-function tests for the extractor (happy path, empty input, malformed input, missing-field filtering). One subprocess-failure test using `run_x_with_binary(tmp.path(), "/nonexistent/x")` — never mutate `$PATH`.

5. **Wire the converter.** In `tools/fig-converter/src/native-map.js`, add an entry to `NATIVE_GENERATOR_MAP` keyed on `script.slice(0, 2).join(' ')`. For providers where the same subprocess maps to different providers via `postProcess` source (arduino-cli boards vs. ports), add a regex check on the third `postProcessSource` argument. For spec-name-scoped mappings (e.g., `conda env list` routes to `mamba_envs` only in `mamba.json`), extend `SPEC_SCOPED_MAP`.

6. **Regenerate specs.** `cd tools/fig-converter && npm run convert`. Spot-check that the affected generators now read `{"type": "<name>"}` with no `script`, `requires_js`, or `js_source` fields.

## Local-project providers

A subset of providers do not shell out at all — they parse a project file in the user's CWD ancestry. Reference implementation: [`crates/gc-suggest/src/providers/local_project/`](../crates/gc-suggest/src/providers/local_project/) (UX-5). Same `Provider` trait as the subprocess providers, with two pattern differences:

1. **Ancestor walk for file discovery.** Each provider walks up to 32 ancestors of `ctx.cwd` to find its file (`Makefile` / `package.json` / `Cargo.toml`). The walk is bounded to defuse pathological symlink loops.
2. **`MtimeCache<T>` invalidation, no TTL.** A module-private cache keyed by absolute file path with `(mtime, size)` invalidation. Cached entries remain valid forever until the source file changes — a hand-edit to `Makefile` is picked up on the next keystroke. LRU-evicted at 64 entries per provider as a hard cap (these files are tiny, so the cap is generous in practice).

### v1 providers

| Type string | File | Replaces |
|---|---|---|
| `makefile_targets` | `GNUmakefile` / `makefile` / `Makefile` (GNU make's documented precedence) | `requires_js: true` generator that shells out to `make -qp` and post-processes the output |
| `npm_scripts` | `package.json` | `bash -c "until [[ -f package.json ]]..."` script with a JS post-processor that projects `scripts` keys |
| `cargo_workspace_members` | `Cargo.toml` (nearest ancestor with `[workspace]`, falls back to nearest `Cargo.toml` for single-package crates) | `cargo metadata --format-version 1 --no-deps` invocation that JSON-parses to extract `packages[].name` |

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
3. Add the variant + dispatcher arms in `providers/mod.rs` (same as for subprocess providers).
4. Hook up the converter in `tools/fig-converter/src/native-map.js`. For script-array shapes use `NATIVE_GENERATOR_MAP` or `SPEC_SCOPED_MAP`; for `_custom` / `_scriptFunction` (where there is no `script` array), extend `matchNativeFromJsSource` with a regex on the JS source.
5. Run `npm --prefix tools/fig-converter test` and `cargo test -p gc-suggest`.

If the upstream specs you're rewriting carry hand-curated `priority` fields that the regen would drop, use the surgical patch script at `tools/fig-converter/scripts/patch-local-project-providers.mjs` as a template — it rewrites only matching generators in place, preserving every other field.

## Caching

Providers currently bypass the `CacheConfig` layer on `GeneratorSpec`. That config (`ttl_seconds`, `cache_by_directory`) applies only to script-based generators. If a provider's underlying subprocess is expensive enough to warrant caching, add it inside the provider itself — either a module-level `Mutex<LruCache<PathBuf, (Instant, T)>>` or a `tokio::sync::OnceCell` guarded by timestamp. Local-project providers use the shared `MtimeCache<T>` defined in `local_project/mod.rs`. Keep cache logic private to the provider module; don't reach into `gc-suggest::cache`. If you find yourself wanting shared caching across providers, that's a signal to design a dedicated provider-level cache API in a follow-up phase.

## Converter eligibility

A generator in the fig source qualifies for native rewriting when `matchNativeGenerator(specName, gen.script, gen._postProcessSource)` returns a non-null `{type: "..."}`. The matcher consults (in order): the arduino-cli postProcess disambiguator, the `SPEC_SCOPED_MAP` for spec-name-scoped mappings, then the global `NATIVE_GENERATOR_MAP`. Any keys not in the map fall through to the existing script/transform/js-source pipeline — providers do not steal matches from postProcess pattern detection.

The `NO_OP_DRIVER_FLAGS` set in `native-map.js` strips driver flags (e.g., `git --no-optional-locks`) before keying the map, so variants like `["git", "--no-optional-locks", "branch"]` still route to `git_branches`. Add to this set when you discover a spec passing no-op flags to a command you've already mapped.

## Error handling

Provider failures must never propagate. Every error path (spawn failure, timeout, non-zero exit, parse error) logs via `tracing::warn!` with structured fields and returns `Ok(Vec::new())`. See [`crates/gc-suggest/src/git.rs`](../crates/gc-suggest/src/git.rs) for the canonical pattern the Phase 3A providers mirror. The suggest engine depends on this contract — a provider that bubbles an `anyhow::Error` will tank the completion pipeline.
