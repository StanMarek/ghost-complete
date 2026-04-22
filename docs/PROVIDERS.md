# Native Providers

Phase 3A introduced a native-provider pipeline in `gc-suggest` that replaces a subset of Fig specs' `requires_js` generators with async Rust code. Providers eliminate the JS runtime dependency for commands whose completion source is a well-behaved subprocess (stable output, no auth, no pagination, no file parsing).

Reference implementation: [`crates/gc-suggest/src/providers/arduino_cli.rs`](../crates/gc-suggest/src/providers/arduino_cli.rs) — mirrors the full pattern (subprocess runner, pure extractor, test-injection binary override).

## Adding a new provider

1. **Confirm eligibility.** The command must pass plan §3A's criteria: single subprocess call, no auth, no pagination, stable text output, no file-system parsing, bounded size (<10K lines), no new transitive deps. Candidates live in [`tools/fig-converter/docs/candidate-providers.json`](../tools/fig-converter/docs/candidate-providers.json) with per-criterion booleans from the Phase 1 spike.

2. **Create the provider file.** Add `crates/gc-suggest/src/providers/<name>.rs`. Follow `arduino_cli.rs`'s shape:
   - `const X_TIMEOUT_MS: u64 = 2_000;` — all provider subprocesses share the 2s default.
   - `pub(crate) async fn run_x(cwd: &Path) -> Option<T>` — thin wrapper that hardcodes the real binary name.
   - `pub(crate) async fn run_x_with_binary(cwd: &Path, binary: &str) -> Option<T>` — parametric binary name for subprocess-failure tests (no `unsafe { set_var }`).
   - `fn x_from_output(parsed: T) -> Vec<Suggestion>` — pure extractor, testable without spawning.
   - `pub struct XProvider;` + `impl Provider for XProvider` with `name()` and `async fn generate(ctx)`.

3. **Register.** In `crates/gc-suggest/src/providers/mod.rs`:
   - `pub mod x;`
   - Add a variant to `ProviderKind`.
   - Add the string→kind arm in `kind_from_type_str`.
   - Add the dispatcher arm in `resolve`.

4. **Test.** Pure-function tests for the extractor (happy path, empty input, malformed input, missing-field filtering). One subprocess-failure test using `run_x_with_binary(tmp.path(), "/nonexistent/x")` — never mutate `$PATH`.

5. **Wire the converter.** In `tools/fig-converter/src/native-map.js`, add an entry to `NATIVE_GENERATOR_MAP` keyed on `script.slice(0, 2).join(' ')`. For providers where the same subprocess maps to different providers via `postProcess` source (arduino-cli boards vs. ports), add a regex check on the third `postProcessSource` argument. For spec-name-scoped mappings (e.g., `conda env list` routes to `mamba_envs` only in `mamba.json`), extend `SPEC_SCOPED_MAP`.

6. **Regenerate specs.** `cd tools/fig-converter && npm run convert`. Spot-check that the affected generators now read `{"type": "<name>"}` with no `script`, `requires_js`, or `js_source` fields.

## Caching

Providers currently bypass the `CacheConfig` layer on `GeneratorSpec`. That config (`ttl_seconds`, `cache_by_directory`) applies only to script-based generators. If a provider's underlying subprocess is expensive enough to warrant caching, add it inside the provider itself — either a module-level `Mutex<LruCache<PathBuf, (Instant, T)>>` or a `tokio::sync::OnceCell` guarded by timestamp. Keep cache logic private to the provider module; don't reach into `gc-suggest::cache`. If you find yourself wanting shared caching across providers, that's a signal to design a dedicated provider-level cache API in a follow-up phase.

## Converter eligibility

A generator in the fig source qualifies for native rewriting when `matchNativeGenerator(specName, gen.script, gen._postProcessSource)` returns a non-null `{type: "..."}`. The matcher consults (in order): the arduino-cli postProcess disambiguator, the `SPEC_SCOPED_MAP` for spec-name-scoped mappings, then the global `NATIVE_GENERATOR_MAP`. Any keys not in the map fall through to the existing script/transform/js-source pipeline — providers do not steal matches from postProcess pattern detection.

The `NO_OP_DRIVER_FLAGS` set in `native-map.js` strips driver flags (e.g., `git --no-optional-locks`) before keying the map, so variants like `["git", "--no-optional-locks", "branch"]` still route to `git_branches`. Add to this set when you discover a spec passing no-op flags to a command you've already mapped.

## Error handling

Provider failures must never propagate. Every error path (spawn failure, timeout, non-zero exit, parse error) logs via `tracing::warn!` with structured fields and returns `Ok(Vec::new())`. See [`crates/gc-suggest/src/git.rs`](../crates/gc-suggest/src/git.rs) for the canonical pattern the Phase 3A providers mirror. The suggest engine depends on this contract — a provider that bubbles an `anyhow::Error` will tank the completion pipeline.
