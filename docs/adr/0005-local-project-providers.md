# 0005. Local-project completion providers

- **Status:** Accepted
- **Date:** 2026-05-02
- **Supersedes:** —
- **Superseded by:** —

## Context

The screenshotted-on-day-one demos — `make <TAB>`, `npm run <TAB>`,
`cargo run -p <TAB>` — currently produce nothing. The upstream Fig
specs back those positions with `requires_js: true` generators, and
Ghost Complete intentionally does not embed a JS runtime (separate,
deferred initiative).

`requires_js` counts in the priority specs (verified at the time of
writing):

- `specs/cargo.json`: 144
- `specs/docker.json`: 79
- `specs/git.json`: 42
- `specs/npm.json`: 36
- `specs/make.json`: 4

A subset of those generators read **local project files** the user
already owns — `Makefile`, `package.json`, `Cargo.toml`. We don't need
JavaScript to read those files. We don't even need to shell out to
`make`/`cargo`/`npm`. Pure file parsing in Rust closes the gap for the
demo cases without unblocking the JS runtime first.

The decision under this ADR is **how** to wire those Rust providers
into the spec system.

## Decision

Implement three providers under
`crates/gc-suggest/src/providers/local_project/`:

- `MakefileTargets` (type string `makefile_targets`)
- `NpmScripts` (type string `npm_scripts`)
- `CargoWorkspaceMembers` (type string `cargo_workspace_members`)

Each is registered as a `ProviderKind` variant, included in
`ProviderKind::ALL`, mapped by `ProviderKind::type_str()`, and handled
by the async dispatcher in `providers/mod.rs` exactly like every other
provider.

`MakefileTargets` and `NpmScripts` share an `MtimeCache<T>` keyed by
absolute file path with `(mtime, size)` invalidation — no TTL, since
hand-edited project files are the only invalidation signal that
matters and mtime captures it. Hard cap of 64 entries per provider;
FIFO-evicted on insert.

Cargo's workspace resolution needed more than a single `(mtime, size)`
pair on the root manifest (member files and glob-prefix directories
also drive validity, plus missing-path probes for newly-created
crates), so `CargoWorkspaceMembers` ships its own `CargoCache` with a
list of per-path `Stamp`s; only `MakefileTargets` and `NpmScripts`
share the simpler `MtimeCache<T>` described above.

**Hookup:** option (c) — extend the converter (`native-map.js`) and
introduce new provider type strings. The converter rewrites matching
`requires_js` generators into native `{ "type": "<name>" }` entries,
preserving an upstream `cache` field when one is present.

Three options were considered for hookup:

- **(a) Engine reads `requires_js` and dispatches by command name +
  position.** Rejected. Couples to a transient flag on the spec —
  the moment a Fig source bumps a generator from `requires_js: true`
  to a regular `script`, the engine silently stops firing the native
  provider. No build-time signal.
- **(b) Engine hardcodes `(command, subcommand, flag)` matching and
  bypasses the spec system entirely.** Rejected. Re-implements
  arg-position resolution outside `resolve_spec`, so spec edits
  (renaming a subcommand, reordering options) silently stop firing
  the provider. Same silent-failure class as (a).
- **(c) Converter rewrite + spec-declared provider types.**
  Accepted. The spec declares the provider at the exact arg
  position; the engine dispatches what the spec says. Same path as
  every other provider. Adding a new local-project provider is one
  converter map entry + one Rust file + three lines in `providers/mod.rs`.

For the `_custom` / `_scriptFunction` upstream shapes (where there is
no `script` array to key off — the entire generator is a JS function),
the converter grew a sibling helper `matchNativeFromJsSource` that
pattern-matches on the stringified function source. Currently used to
catch make's `make -qp | awk ...` shape; the seam is open for future
provider migrations that work at the same layer.

## Consequences

### Positive

- **Demo cases land.** `make <TAB>` lists targets; `npm run <TAB>` lists
  `package.json#scripts` keys; `cargo run -p <TAB>` and the cargo package
  flags patched in `specs/cargo.json` list workspace members.
- **No JS runtime dependency.** The three providers ship as pure Rust
  parsers. The deferred JS-runtime initiative remains independently
  scopable.
- **No subprocess overhead.** Hand-rolled Makefile parser beats `make
  -qp` by ~30–50 ms cold start on a typical 200-line Makefile, AND
  avoids the fork-per-keystroke that the cache is designed to prevent.
  `serde_json` on `package.json` and `toml` on `Cargo.toml` are both
  sub-millisecond on realistic inputs.
- **Symmetric extensibility.** Adding `yarn` / `pnpm` / `just` /
  `docker-compose` later reuses the same three-step pattern documented
  in `docs/PROVIDERS.md` §Local-project providers. The npm parser, in
  particular, is one line away from also serving yarn (same
  `package.json#scripts` shape).
- **Strip-on-rewrite contract.** The converter drops `requires_js`,
  `js_source`, `script`, and `script_template` whenever a generator is
  routed to a native provider, so converted specs carry only the
  native provider type plus optional `cache`. Pinned by tests in
  `tools/fig-converter/src/index.test.js`.

### Negative

- **Hand-parsed Makefile misses edge cases.** Computed includes,
  recursive variable expansion, and pattern rules with computed
  prerequisites are out of scope. The 95% case (a flat target list)
  is fully covered; missed targets are omitted from the
  `makefile_targets` provider result. The current make spec does not
  request an additional filesystem provider for the target arg.
- **Workspace glob expansion is deliberately narrow.** Literal paths
  and trailing `prefix/*` are supported; anything more exotic
  (`crates/**/leaf`, brace expansion) is logged-and-skipped. The user
  can `cd <crate-dir>` and run the bare command as a workaround.
- **One new workspace dep (`toml = "0.8"`).** The `toml` crate is
  built on top of `toml_edit` (which we already pull in for the
  config-edit subcommand), so the additional compile cost is limited
  to `toml`'s thin `Deserialize` layer — effectively free.
- **`serde_json` `preserve_order` feature is now on.** Required for
  the `npm_scripts` provider to emit script keys in `package.json`
  source order rather than alphabetical (BTreeMap default). Switches
  serde_json's `Map` from `BTreeMap` to `IndexMap` workspace-wide for
  `gc-suggest`. Verified non-regressing against the full
  `cargo test --workspace` suite.

### Neutral

- **`package.json#fig.scripts` overrides not honoured in v1.**
  Upstream Fig spec supports them; our parser reads only the `scripts`
  object. Tracked as v2 polish.
- **mtime, not inotify.** No filesystem watchers — invalidation
  happens on the next read. A user editing `Makefile` in their editor
  sees the new targets on the next `<TAB>`, not before. Fine for
  interactive completion; would matter for a long-running daemon.
- **Surgical patch script for the regen.** The full converter regen
  drops hand-curated `priority` fields that the upstream specs no
  longer carry. We added a one-shot patch script
  (`tools/fig-converter/scripts/patch-local-project-providers.mjs`) to
  rewrite only matching generators in place, preserving the rest.
  Future local-project provider additions can extend the same script
  rather than running the destructive full regen.

## Alternatives considered

- **Implement a JS runtime first.** Rejected for this scope — the
  three demo cases here don't need JS at all, and shipping them now
  is a strict UX win independent of whatever the JS-runtime initiative
  ends up looking like.
- **Shell out to `make -qp`, `npm pkg get scripts`, `cargo metadata`.**
  Rejected. Each of those forks per keystroke until the cache warms,
  and the cache only warms after the first paint. The hand-parse is
  tens of milliseconds faster cold and adds no transitive deps.
- **Ship one provider (cargo) first, defer make and npm.** Rejected.
  All three share the local-project-provider shape and converter
  wiring; make and npm share `MtimeCache<T>`, while cargo's
  `CargoCache` handles workspace stamp invalidation. The marginal cost
  of shipping all three together is one provider file each. Splitting
  them across PRs would just move review work into the future.

## References

- `crates/gc-suggest/src/providers/local_project/mod.rs` —
  `MtimeCache<T>` for make/npm
- `crates/gc-suggest/src/providers/local_project/makefile.rs` —
  Makefile parser + provider
- `crates/gc-suggest/src/providers/local_project/npm_scripts.rs` —
  `package.json#scripts` parser + provider
- `crates/gc-suggest/src/providers/local_project/cargo_workspace.rs`
  — workspace member parser + provider
- `crates/gc-suggest/tests/local_project_e2e.rs` — engine-level
  end-to-end tests
- `tools/fig-converter/src/native-map.js` — converter mapping table
  + `matchNativeFromJsSource` for the `_custom` shape
- `docs/PROVIDERS.md` §Local-project providers
- `docs/COMPLETION_SPEC.md` — three new entries in the native types
  table
