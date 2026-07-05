# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.19.0] - 2026-07-05

### Added

- **Otty terminal support**: Ghost Complete now recognizes Otty
  (`TERM_PROGRAM=otty`, bundle id `io.appmakes.otty`), a Ghostty fork, as a
  first-class supported terminal. It
  inherits Ghostty's exact capability profile — DECSET 2026 synchronized output
  and native OSC 133 prompt markers — so popups render at the correct cursor
  position with no configuration and **without** `[experimental] multi_terminal`.
  Unlike Ghostty proper, Otty does not set `GHOSTTY_RESOURCES_DIR`, so it is
  detected purely via `TERM_PROGRAM`; the zsh init whitelist and the shell
  integration's native-OSC-133 gate were updated to match. Brings the supported
  terminal count to 10 (#153).

### Changed

- Dependency maintenance clearing the Dependabot backlog. Each bump was verified
  against the full test / clippy `-D warnings` / fmt gate, and every
  breaking-semver bump additionally passed an adversarial changelog-vs-usage
  audit before merge:
  - `shlex` 1.3.0 → 2.0.1 (#133) — the one API we use (`split`) is behaviorally
    unchanged; 2.0 only removed convenience/`unsafe` APIs we never call.
  - `sha2` 0.10.9 → 0.11.0 (#134) — RustCrypto digest 0.11 generation; SHA-256
    output is byte-identical, now pinned by a FIPS-180-4 known-answer test.
  - `rquickjs` 0.10.0 → 0.12.0 (#136) — bundled quickjs-ng advances to Unicode
    17.0.0 with regex/TypedArray security fixes; a new tripwire test pins the
    engine's Unicode behavior so a future bump that silently shifts `requires_js`
    generator output for non-ASCII input is caught.
  - `toml_edit` 0.22.27 → 0.25.12 (#90) — config round-trip output is
    byte-identical; also gains TOML 1.1 input acceptance, aligning it with the
    already-1.1 `toml` crate.
  - CI actions: `actions/setup-node` 5 → 6 (#87), `dorny/paths-filter` 3 → 4
    (#88).

### Security

- Bumped `anyhow` from 1.0.102 to 1.0.103 to clear **RUSTSEC-2026-0190**: an
  unsoundness in `Error::downcast_mut()` that could trigger undefined behavior
  when a mutable downcast follows an `Error::context` call. Lockfile-only change;
  the workspace already required `anyhow = "1"` (#154).
- Bumped the AWS SDK crates to clear Dependabot advisory **GHSA-g59m-gf8j-gjf5**
  (AWS SDK for Rust v1 region-parameter defense-in-depth, low severity):
  `aws-sdk-iam` 1.80.0 → 1.95.0 (direct pin) plus the transitive `aws-sdk-sts`
  → 1.103.0, `aws-sdk-ssooidc` → 1.100.0, and `aws-sdk-sso` → 1.98.0, all past
  their patched versions. Supersedes the partial Dependabot bump #135, which
  stopped at 1.90.0 — short of the 1.95.0 patch line (#157).

## [0.18.0] - 2026-06-14

### Added

- **Tab-accepts-top option**: set `popup.tab_accepts_top = true` to make the
  accept key (Tab) accept the top-ranked suggestion even when nothing has been
  navigated yet, instead of forwarding a literal tab to the shell. Restores the
  Fig/Kiro "type, glance, Tab" flow without the extra arrow-key press. Default
  `false` preserves the historical "navigate first, then accept" behavior. Only
  the `accept` action is affected: with the default bindings the `accept_and_enter`
  action (Enter) is a separate binding and still runs the command line, so a stray
  Enter never silently accepts the top suggestion. Hot-reloadable via `config.toml`
  or the TUI editor (#150).
- **Substring match mode** for the suggestion filter: set
  `suggest.match_mode = "substring"` to require the typed characters to appear
  contiguously (`cl` → `clone`/`include`, not `calendar`) instead of the
  default fuzzy subsequence matching. Space-separated words are matched as
  independent substrings. Configurable via `config.toml` or the TUI editor;
  requires a restart (#149).

## [0.17.0] - 2026-06-07

### Added

- **Real clap subcommands** for the CLI (`install`, `uninstall`, `status`,
  `validate-specs`, `doctor`, `config`, `config edit`), replacing the
  hand-rolled argument parser. Non-UTF-8 paths and argv are preserved
  end-to-end via `OsString`/`PathBuf`, and a `--log-level` value enum threads
  through tracing init (#132).
- **Adaptive TUI config editor**: 3/2/1-pane responsive layouts at width
  ≥100 / ≥60 / narrow, scrollable field list (PgUp/PgDn/Home/End), a `/`
  filter that narrows fields by key/help substring, and a collapsible preview
  panel (toggle `p`) (#143).
- **Static alias-file parser** that reads `.zshrc`/`.zprofile`/`.zshenv` and
  oh-my-zsh custom drop-ins directly, preferred over the zsh subprocess —
  quote-aware value scanning, dotted/hyphenated alias names, last-read-wins
  cross-file precedence (#141).
- **Query-aware Homebrew completion**: `brew_formulae_searchable` routes typed
  queries through `brew search <q>`, plus new `brew_casks_searchable` and
  `brew_packages_searchable` providers backing the cask/formula/union split on
  `install`, `search`, `edit`, `home`, and `abv` (#142).
- **macOS-native completion providers**: `defaults_keys` (read/write/delete/
  rename key args), colon-aware `chown_owner_group`, and `macos_applications`
  + `macos_bundle_identifiers`; `open`, `osascript`, and `codesign` specs now
  use native providers instead of deferred JS (#146).
- **History lane reservation**: up to 2 popup rows reserved for exact/prefix
  history matches before candidate ranking, so high-confidence history no
  longer vanishes when the popup saturates (#145).
- **OSC 7774 runtime diagnostic channel**: the shell emits structured
  diagnostics (`env_truncated`, `zle_hook_disabled`) the proxy logs instead of
  failing silently; the proxy strips the full GC-private OSC range
  (7770–7774). Budgeted env reporting (512 KiB total / 16 KiB per value) keeps
  snapshots under the parser cap (#137).
- **Hot-reloadable `popup.render_block_ms`** (was builder-time only, despite
  being documented and editor-exposed as live) (#138).
- **Deeper `doctor` shell-integration checks**: both managed blocks present,
  no duplicates, referenced `init.zsh`/`ghost-complete.zsh` exist and match
  the embedded version, legacy OSC 7770 migration warnings, source-path
  validation against the actual `.zshrc` block (#139).
- **Atomic, mode-preserving installs**: `.zshrc`, `init.zsh`, and
  `ghost-complete.zsh` written via tempfile + rename so a crash mid-write
  can't leave a truncated shell hook (#139).
- **Config/docs drift test battery** driven by `gc_config::all_field_paths()`,
  pinning the install template, TUI editor, and `CONFIGURATION.md` against the
  schema so new fields can't silently go undocumented (#147).

### Changed

- Shell prompt-marker suppression (`_gc_native_osc133`) now covers Kitty,
  WezTerm, and Rio, fixing double prompt markers; WezTerm is also detected via
  `WEZTERM_UNIX_SOCKET` in the `init.zsh` direct branch to match the Rust
  matrix (#137).
- `status` hides the full `js_commands` list behind `--verbose` (plain
  `status` prints a one-line summary), sparing users a ~17K-command flood on
  every check; the JSON contract is unchanged (#144).
- Overlay updates emit exactly one balanced DECSET 2026 sync frame around the
  whole clear+render+detail cycle (was multiple sync windows, with the detail
  box rendered entirely outside any frame) (#138).
- CI hardening: the coverage-regression check is now a **blocking** gate,
  `cargo-deny` runs alongside `cargo-audit`, a zsh/ZLE shell smoke gate runs
  on macOS, and release artifacts are extracted and smoke-tested
  (`--version`, `validate-specs`, `status --json`, `install --dry-run`) before
  publish. `toml` bumped 0.8 → 1 (#139).
- Documentation refresh: dropped the stale "keystroke required" README note
  (contradicted by the dynamic merge loop), replaced SECURITY.md's hard-coded
  version with a release-policy statement, and routed vulnerability reports
  through GitHub private reporting (#144).

### Fixed

- Accepted filesystem paths are now shell-escaped with quote-context awareness
  (unquoted, single-, double-quoted), and the escaped form is written into
  both the live buffer and the chaining predicted buffer so the next
  completion resolves the right directory (#145).
- `&` now splits command segments in the buffer parser: `sleep 1 & git `
  correctly parses `git` as the command instead of an argument to `sleep`
  (#137).
- `;` in OSC 7 CWD paths is percent-encoded before transmission, fixing silent
  path truncation (vte splits OSC parameters on `;`) (#137).
- User `.zshrc` blank lines and surrounding bytes are preserved on
  install/reinstall; managed blocks are spliced in place instead of trimming
  the whole file (#147).
- A poisoned `token_only` demotion mutex now recovers (warn + reset +
  `clear_poison`) instead of panicking and crashing the proxy on the keystroke
  path (#139).
- Buffer (`OSC 7772`) and env (`OSC 7773`) reports are gated on
  `GHOST_COMPLETE_ACTIVE`, so they no longer leak as raw OSC sequences to the
  terminal when the proxy is inactive (#137).

## [0.16.0] - 2026-05-13

### Added

- Native completion migration - Phase 1 (ux-10b): `json_path_extract`
  transforms, JSONPath `[*]` wildcard projection, Fig helper recovery in the
  converter, comma-list postProcess lowering, and status accounting for
  `requires_js_generators_lowered_to_transforms`.
- Native completion migration - Phase 2 (ux-11): static subprocess extraction
  lifts single-call Fig JS wrappers into native `script` / `script_template`
  plus transforms, marks them with `_static_extracted_subprocess`, and reports
  progress via `counters.static_extracted_subprocess` in
  `crates/gc-suggest/src/specs.rs` (#124, 062ce35).
- Migration precursor (ux-9b): `SpecResolutionCounters` exposed in
  `ghost-complete status --json` (schema 1.6) with `requires_js_total`,
  `requires_js_supported`, `requires_js_unsupported`,
  `lowered_to_transforms`, `static_extracted_subprocess`,
  `token_only_promoted`, `aws_sdk_dispatched`, and
  `native_provider_dispatched` counters.
- `ProviderCtx::params` (`Arc<BTreeMap<String, String>>`) and the matching
  `GeneratorSpec.params` field for spec-driven providers, plus a
  `params_hash()` helper for cache keys. Existing providers ignore the
  field; the channel is purely additive plumbing for ux-13/14.
- Deterministic `fig-converter` mode (`--deterministic`) that emits a
  reproducible `corpus-hash.txt` and cleans up tempdirs on hash mismatch.
- Binary-size CI gate now records `size.txt` as a workflow artifact and
  honours a `binary-size-allow-delta` PR label that lifts the per-PR
  delta budget from 2 MB to 5 MB.
- Native completion migration - Phase 5 (ux-14): native tool providers for
  Cargo, npm, Docker/Podman, kubectl, tmux, systemd, Homebrew, and macOS
  directory-service principals. The current corpus reports 582
  provider-dispatched generators, `status --json` exposes per-provider counts
  under `counters.native_provider_counts`, and the schema is bumped to 1.10.
- Install spec-mirror auto-refresh: the proxy detects when
  `~/.config/ghost-complete/specs/` was written by an older binary and
  silently overwrites it from the embedded archive on startup. A
  `.ghost-complete-version` stamp pins the writer version; user-curated
  `[paths] spec_dirs` overrides skip the refresh. `ghost-complete doctor`
  now reports the mirror state as a `[OK]`/`[WARN]` check so operators see
  whether their installed corpus matches the binary. The new module lives
  at `crates/gc-suggest/src/mirror.rs`; install and the proxy share a
  single writer. The auto-refresh now sha256-fingerprints each mirror
  file at write time and skips files the user has edited, surfacing them
  through the doctor check rather than silently overwriting them.
- Config editor (`ghost-complete config edit`) now exposes
  `popup.render_block_ms`, `suggest.providers.js_runtime`,
  `experimental.aws_sdk_provider`, `experimental.aws_sdk_fallback_to_cli`,
  and `experimental.brew_search_cap`, and introduces a `FieldType::U16`
  variant so `popup.feedback_dismiss_ms` (`u16` in the schema) no longer
  silently saturates when the user enters a value above `u16::MAX`.

### Changed

- `ghost-complete status --json` `counters.*` block now describes
  migration progress against the **embedded** corpus (the JSON shipped
  inside the binary) rather than the runtime-resolved view. A stale
  `~/.config/ghost-complete/specs/` mirror saved before ux-10b/ux-13/ux-14
  markers existed previously took filesystem precedence and silently
  zeroed out `lowered_to_transforms`, `aws_sdk_dispatched`, and the
  native-provider buckets — turning the marketing-facing counters into
  an operator-trust failure. Filesystem overrides still hot-patch
  individual broken specs at dispatch time; they no longer mask ship
  statistics. The mirrored top-level fields
  (`requires_js_generators_lowered_to_transforms`,
  `requires_js_generators_static_extracted`,
  `requires_js_generators_token_only`) now match `counters.*` and the
  loaded-view `spec_counts.*` block continues to reflect the user's
  resolved corpus. New `gc_suggest::embedded_corpus_counters()` helper
  is the canonical source for these counters.
- Embedded completion specs are now zstd-compressed at build time
  (level 19) into a single archive emitted by `gc-suggest/build.rs`.
  Stripped release binary drops by 91.60 MB on macOS arm64
  (103.41 MB → 11.81 MB). Decompression is lazy at first spec lookup —
  bodies leak into a `&'static str` cache so the warm path stays at
  tens of nanoseconds. First-touch latency for the largest embedded
  spec (`aws`, ~36 MB) is ~167 ms; every other spec decompresses in
  under 2 ms. Full-corpus cold decompress + parse (711 specs) completes
  in 183 ms. See `docs/plans/ux-12b-zstd-spec-compression/SPEC.md` and
  `benchmarks/v0.16.0-ux12b.md`.
- Release profile keeps `strip = "symbols"`. After zstd compression,
  `benchmarks/binary-size-baseline.txt` records a 21,383,696-byte stripped
  release binary for CI delta checks. The 110 MB absolute ceiling is unchanged.
- Diagnostic warnings emitted while loading specs
  (`removed N suggestion(s)`, `suggestion in <spec> has empty name`,
  `removed N generator(s) with invalid pipelines`) are now deduplicated
  and emitted at `debug!` level instead of `warn!`. Default verbosity now
  produces zero stderr lines for `status`, `status --json`, `doctor`,
  `validate-specs`, and `config`; opt-in via `RUST_LOG=gc_suggest=debug`
  still surfaces the per-spec details.
- `ghost-complete status` baseline labels in the embedded
  `docs/coverage-baseline.json` snapshot drop the speculative
  `v0.17.0`/`v0.18.0` future-version prefixes; the trend block now renders
  with the actual phase names so the v0.16.0 release output reads
  consistently.

## [0.15.0] - 2026-05-08

### Added

- Configurable popup width via new `[popup] min_width` and `[popup] max_width`
  config keys. Previously the popup was hard-clamped to 20–60 columns. The new
  defaults match the legacy bounds, but users on wide terminals can now bump
  `max_width` to give descriptions more room before truncation. Both keys are
  hot-reloadable.
- Description text now ends with a single-column ellipsis (`…`) when truncated,
  so users can tell the description was cut off rather than guessing.
- New adjacent description box (#116). Set `[popup] description_box = "side"`
  to render a wrapped description for the selected suggestion next to the
  main popup. The box appears when the inline description would be hidden or
  truncated — short descriptions that already fit in the popup row don't
  trigger an empty side box. The wrapped description is capped by
  `description_box_lines` and available rows. Falls back to a stacked-below
  box when there's no horizontal room, and to inline truncation when neither
  fits. Width (`description_box_max_width`), line cap
  (`description_box_lines`), and selection-change debounce
  (`description_box_debounce_ms`) are all configurable. Default mode is `off`
  for opt-in v1 rollout.

## [0.14.0] - 2026-05-08

### Added

- Prioritize current git branch (#117).

## [0.13.0] - 2026-05-06

### Added

- Add spec_cache section to config editor (#112).
- Add opt-in spec cache eviction for better memory usage (#110).

### Fixed

- Lazy spec loading drops idle memory from 333MB to ~2MB (#109).

### Changed

- Add local build makefile.

## [0.12.3] - 2026-05-05

### Fixed

- Defer popup trigger until display catches up (#107) (#108).

## [0.12.2] - 2026-05-04

### Changed

- Fixed automatic popup triggering (#105).

## [0.12.1] - 2026-05-04

### Fixed

- Cleaned up `install-local` warnings: tightened `doctor` and `status` output
  formatting, normalized spec validation messages, and re-ran the converter on
  a handful of specs (`br`, `j`, `kubecolor`, `lsof`, `micro`, `nativescript`,
  `remotion`, `sta`, `tns`) to drop spurious validator noise (#103).

## [0.12.0] - 2026-05-04

### Added

- **JS runtime for `requires_js` generators** — all 3,641 dynamic generators
  across 180 specs (aws being the dominant member) now contribute live
  suggestions instead of stalling at static-only completion. Three generator
  classes dispatch through a bounded QuickJS sandbox: `post_process`
  (script stdout fed to a JS `postProcess` body), `script_function`
  (JS produces argv, runner executes the shell, transforms apply), and
  `custom` (JS produces suggestions directly). Class B/C require an
  explicit `self_contained: true` opt-in on the spec. See
  [ADR 0006](docs/adr/0006-quickjs-runtime-foundation.md) and
  [`docs/JS_RUNTIME.md`](docs/JS_RUNTIME.md).
- **New `gc-jsrt` crate** — owns the `rquickjs` dependency and exposes a
  bounded JS evaluator. Sync `rquickjs::Runtime` on a dedicated worker
  thread; mpsc job queue + per-job oneshot reply. Sandbox: removes
  Node/Deno/Bun/Worker/`fetch`/`setTimeout`/`Buffer`/`require`/`process`
  globals, shadows `eval` and `Function` intrinsics with throwing
  closures, builds rquickjs with no module/native loader and no async
  surface. 8 MiB memory cap, 512 KiB max stack, 2 MiB GC threshold,
  wall-clock interrupt via `Runtime::set_interrupt_handler`. Output
  normalised through `JSON.stringify` then `serde_json::Value` with
  hard caps (1024 suggestions, 256-byte names, 1024-byte descriptions,
  256 KiB total). Workspace grows from 8 to 9 crates.
- **Fig-compatible host API** — `cwd`, `env`, `tokens`, `searchTerm`,
  `currentToken`, `previousToken` and `executeShellCommand` are
  installed on every JS evaluation, plus the legacy `fig` namespace
  surface. Unsupported subnamespaces (`fs`, `path`, `keychain`, `ipc`,
  `ui`) throw structured `UnsupportedHostApi` errors.
  `executeShellCommand` accepts argv arrays and `{command, args}`
  descriptors by default; shell-string form is denied unless the spec
  flips `allow_shell_command`. Recursion cap of 5 shell calls per
  evaluation prevents accidental fork-bombs.
- **`[suggest.providers] js_runtime` kill switch** — default `true`.
  When `false`, all three JS-backed generator classes are skipped at
  dispatch time, equivalent to pre-v0.12 behaviour. Surfaces in
  `status --json` under a top-level `js_runtime` block and in the
  status text view; `doctor` warns when the switch is off.
- **Stem-keyed spec store with alias index** — `SpecStore` now keys on
  filename stem (canonical id) plus a `HashMap<String, Arc<SpecEntry>>`
  alias map. The 709 spec files now address as 709 unique entries
  (pre-v0.12 the loader keyed on `CompletionSpec.name` and silently
  dropped one spec per ~6 stem/name collisions). 14 filename/name
  mismatches now surface as 8 non-conflicting aliases plus 6
  `AliasConflict` records (`DuplicateName`, `NameMatchesOtherStem`,
  `DirectoryPrecedence`). Stems take precedence over `name` aliases via
  two-pass registration; `kubectl.json` keeps the `kubectl` alias even
  when `kubecolor.json` (declared `name="kubectl"`) is processed first
  alphabetically.
- **`status` schema v1.2** — new top-level fields
  `commands_addressable`, `commands_(fully|partially|non)functional`,
  `requires_js_generators_(total|supported|unsupported)`,
  `requires_js_generators_supported_by_kind`,
  `command_alias_conflicts`, `command_alias_conflict_details`, and
  `js_runtime`. Status text mode gains Coverage / Dynamic generators /
  Command addressability / JS runtime sections — every metric mirrors a
  `status --json` field name so the two views stay aligned. Coverage
  baseline (`docs/coverage-baseline.json`) refreshed to 3,641 supported
  / 0 unsupported, 0 nonfunctional, 6 alias conflicts, 717 commands
  addressable.
- **`doctor` spec addressability + JS runtime checks** — new
  "Spec addressability" check lists each `AliasConflict` grouped by
  kind with kind-specific hints; "JS runtime" check warns when the kill
  switch is off; "Embedded specs" check verifies every `requires_js`
  generator in the loaded corpus has populated `js_runtime` metadata
  (non-empty `source`, plus the per-kind shape gates: `script` or
  `script_template` for `post_process`; `self_contained: true` for
  `script_function`/`custom`).
- **`validate-specs --strict`** — fails on shipped specs with
  `requires_js: true` but missing or empty `js_runtime.source`. The
  predicate matches the engine and doctor: `post_process` requires a
  script, `script_function`/`custom` require `self_contained: true`. A
  cross-surface property test
  (`validate_doctor_and_engine_predicates_agree`) iterates a 10-fixture
  matrix to lock parity between validate / doctor / engine.
- **Coverage-regression CI gate** — `scripts/check-coverage-regression.sh`
  fails when the unsupported `requires_js` generator count exceeds the
  baseline by more than the tolerance (default 0), or whenever any
  command is reported nonfunctional. Wired into `ci.yml` as
  `continue-on-error: true` for the initial rollout. 23 self-tests
  cover flag parsing, missing baseline / status-json, regression /
  improvement / nonfunctional / within-tolerance branches, and the
  env-var pathway. Documented in `docs/ci-gates.md`.
- **`scripts/count-spec-coverage.sh`** — repo-local jq-based counter
  whose results cross-check against `ghost-complete status --json`.
  Outputs `file_scan_*` keys (raw JSON walk independent of the
  structured `CompletionSpec` deserializer) to disambiguate from
  runtime-level counters.
- **ADR 0006** — records the `rquickjs` choice, sync-runtime decision,
  and sandbox model. `docs/JS_RUNTIME.md` is the ongoing reference doc
  (Class A/B/C distinctions, sandbox layers, timeout caveats,
  normalization, cache-key composition, kill switch, concurrency
  model). Linked from `docs/ARCHITECTURE.md`.

### Changed

- **Generator cache key partitioning** — single `HashMap` now stores
  both stdout strings and JS-processed suggestion vectors via a
  `CachedPayload` enum. New keyspaces: `CacheKey::Stdout` (stdout
  layer, shared across post-process bodies) and
  `CacheKey::JsProcessed{source_hash}` (per-JS-body suggestion layer).
  Two different post-process bodies on the same script never share
  cache entries; spawn cost is shared via the stdout layer. Custom
  dispatch cache key always includes cwd.
- **`GeneratorSpec.js_runtime`** is `Option<Arc<JsRuntimeSpec>>` — the
  dispatch path Arc-clones (pointer bump) instead of deep-cloning the
  embedded JS source on every keystroke. AWS specs ship multi-KB source
  bodies, so the saving is real on the hot path.
- **README + docs** — 9 crates instead of 8 (`gc-jsrt` added);
  Completion Specs paragraph mentions JS-backed generators; Known
  Limitations entry reframes `requires_js` from "partially functional"
  to a bounded sandbox. `docs/SPECS.md` replaces the "no JS runtime"
  non-goal with a pointer to `gc-jsrt`. `docs/COMPLETION_SPEC.md`
  rewrites the `requires_js` section for runtime-active state across
  all three `js_runtime.kind` variants.
- **Converter `js_runtime` emission** — `_custom` →
  `js_runtime.kind = "custom"`, `_scriptFunction` →
  `js_runtime.kind = "script_function"`, `_postProcess` →
  `js_runtime.kind = "post_process"` (when matcher cannot lower to
  declarative transforms). Native generator + transform mappings still
  win first. Re-converted all 709 specs; the legacy `js_source` field
  is no longer emitted (replaced by `js_runtime.source`). The static
  converter also now drops `null`/`undefined` entries in option `name`
  arrays (the Fig sparse-hole pattern that broke `next.json` /
  `pnpx.json` parses post-regen).
- **`run_script_full` returns structured `{stdout, stderr, exit_code}`**
  so `EngineShellRunner` can stop string-sniffing `anyhow` messages.
  Timeout-vs-hung classification corrected to surface
  `ShellRunError::Timeout`.
- **Binary size baseline** — bumped from 102 MB to ~104.78 MB to admit
  the `rquickjs` link cost. Absolute ceiling unchanged at 110 MB; per-PR
  delta budget unchanged at 2 MB. Embedded-spec heap budget unchanged.

### Fixed

- **`z.json` zoxide generator restored** — UX-9's converter regen
  overwrote the hand-curated `z.json` stub because upstream
  `@withfig/autocomplete` has no `z` spec. Restored the
  `zoxide query --list` generator with `split_lines` /
  `filter_empty` / `trim` transforms, 60-second TTL cache, folders
  template fallback, and `-` / `~` static suggestions. (Verified by
  walking the v0.11.0 → HEAD diff for every spec's
  script/script_template/generators count; `z` was the only casualty.)
- **`status` no longer double-counts `requires_js` generators** when
  `spec_dirs` overlap. `SpecStore::canonical_paths()` now drives the
  file-walk so the count is taken over resolved entries only. Default
  config reports 3,641 (was 7,282 with overlapping dirs).
- **`validate.rs` / `doctor.rs` walk both `args` and `extra_args`** —
  previously missed every `requires_js` generator on a non-first option
  arg (e.g., `OptionSpec.args[1..]`), so they slipped past
  `validate-specs --strict` even when missing `js_runtime`. Both PR-added
  regression tests now pass.
- **`apply_block_result` re-rank fix carries over** — the v0.11
  `current_word`-aware merge mirrors `try_merge_dynamic`'s
  empty-vs-non-empty branch on the JS dispatch path too, so
  high-priority JS suggestions don't drop after typing.

### Security

- **JS runtime sandboxing** — host JS executes inside `rquickjs` with
  no FS, network, child-process, or timer surface unless explicitly
  granted. `eval` and `Function` are shadowed with throwing closures so
  spec authors cannot escape into a fresh evaluation context.
  `executeShellCommand` rejects shell-string form unless the spec opts
  in via `allow_shell_command`; argv-form rejects NUL bytes for
  consistency with the existing script-generator rule. Wall-clock
  interrupt + memory cap bound runaway corpus JS even when a native
  call (regex backtracking, JSON parsing) is in flight.
- **`validate-specs --strict` rejects shipped specs with empty
  `js_runtime.source`** — closes a class where a malformed converter
  output (or a hand-edit mistake) would silently classify as
  "supported" but produce no suggestions at runtime.

## [0.11.0] - 2026-05-03

### Added

- Restored AWS CLI completion spec. 409 service subcommands (`s3`,
  `ec2`, `iam`, `lambda`, …) now offer static subcommand and flag
  completion — 17 139 subcommands, 99 537 options total. (Upstream
  `@withfig/autocomplete` ships 418 service `.js` files, but only 408
  of them are reachable via `loadSpec` from the top-level `aws.js`;
  the 9 unreferenced services — `alexaforbusiness`, `backupstorage`,
  `codestar`, `honeycode`, `macie`, `mobile`, `nimble`, `regions`,
  `worklink` — are deprecated AWS services that upstream stopped
  wiring up, so the converter does not see them.) The `--profile`
  option uses a native `split_lines + filter_empty + trim` transform
  on `aws configure list-profiles` with directory-keyed caching (5 min TTL). The
  remaining 1 843 dynamic generators (instance/region/bucket/role
  enumeration) ship as `requires_js: true` and stay deferred to the
  long-running requires-js plan; static completions work today.
- Popup navigation: PageUp, PageDown, Home, and End jump by page or to the
  ends while the popup is visible.
- Local-project completion providers for `make` targets, `npm run` scripts,
  and `cargo -p` workspace members. Pure Rust file parsers — no JS runtime,
  no `make`/`npm`/`cargo` shellout — with mtime-keyed caching (with
  directory-mtime + missing-path stamps for cargo workspace globs so newly
  created crates invalidate cleanly). Closes the empty-popup whiff for the
  most-screenshotted `make <TAB>` / `npm run <TAB>` / `cargo run -p <TAB>`
  demos. New native generator types `makefile_targets`, `npm_scripts`,
  `cargo_workspace_members`. See
  [ADR 0005](docs/adr/0005-local-project-providers.md).
- Surface static `args.suggestions` from completion specs as runtime
  candidates. New `SuggestionKind::EnumValue` (base priority 65) ranks them
  between subcommands (70) and environment variables (50). Affects ~54%
  of bundled specs; e.g. `git archive --format=` now suggests `tar`/`zip`,
  `tar --atime-preserve` suggests `replace`/`system`. See
  [ADR 0004](docs/adr/0004-static-arg-suggestions.md).
- Async suggestion feedback in the popup. While script generators are
  in-flight, the popup reserves an indicator row that surfaces a spinner
  (Loading), a per-provider partial-error label (`PartialError`, gated by
  the new `suggest.show_provider_errors` config), or stays Idle. Hard
  errors and empty results now expire on a `feedback_dismiss_ms` window
  rather than dismissing the popup. New config keys: `suggest.spinner`,
  `suggest.show_provider_errors`, `suggest.feedback_dismiss_ms`.
- OSC 7772 percent-encoded buffer framing between the shell and the
  proxy. Replaces the legacy OSC 7770 escape-sensitive framing (now
  logged once as deprecated) so embedded `\x1b\\` / `\x07` / NUL bytes
  in the command line buffer are reported losslessly. The shell hook
  emits OSC 7772 with a percent-encoded payload and an explicit cursor
  index. OSC 7770 stays parsed for one release for bash/fish parity and
  is scheduled for removal in v0.12.0. See
  [ADR 0003](docs/adr/0003-osc7772-buffer-framing.md).
- Post-install next-steps summary. The installer now prints a structured
  five-step summary (`doctor`, demo command, `Ctrl+/`, `config edit`)
  plus the resolved config + spec paths. The headline branches between a
  green "installed" and a yellow "partially installed" line based on
  whether `~/.zshrc` was actually written; both paths render through
  `sanitize_path` so a hostile `$HOME` cannot smuggle terminal escapes.

### Fixed

- Multi-word alias expansion for spec resolution. Aliases are now
  tokenised with `shlex` and stored as `Vec<String>`, so
  `alias gco='git checkout'` resolves the git spec, walks the
  `checkout` subtree, and dispatches `git_branches` / `git_tags` on
  the next positional. Chained aliases (`alias gcb='gco -b'`) recurse
  through a 16-hop guard with cycle detection. The on-disk alias
  cache schema bumps to `format_version: 2` and rejects v1 caches on
  load. Flag-prefix completion (`gco -<TAB>`) and the async
  `suggest_dynamic` path now share the same alias-target spec walk
  so all three entry points stay consistent.
- Terminal popup rendering robustness. Multiple races and edge cases
  hardened across the parser/overlay boundary: stale-screen-size
  recovery so the popup no longer pins to row 1 when CPR coordinates
  fall outside cached dimensions; cumulative `overlay_scroll_deficit`
  is reset on shell-side viewport scrolls and discarded outright when
  it grows past `cursor_row`; overlay write epoch ordering keeps
  ownership only after acknowledged writes; display-mutating CSI
  sequences mark the screen dirty so newer popup state survives stale
  render bytes losing races; bordered-popup `PartialError` demotion
  now repaints the displaced bottom border instead of leaving a
  stray `╰──╯`; `pending_failed` / `pending_empty_count` reset
  symmetrically on every Idle path.

### Changed

- Binary-size CI ceiling raised from 30 MB to 110 MB to admit the AWS
  spec. Per-PR delta budget unchanged at 2 MB. The release binary now
  measures ~102 MB; the embedded-spec heap test budget rose from 64 MiB
  to 128 MiB. zstd-compressing the embedded JSON corpus is the next
  size-reclaim work and would drop the binary back near the original
  ceiling — tracked as a separate spec, not bundled here so neither
  change becomes unreviewable.
- ADRs are now tracked in git under `docs/adr/`. The first three —
  `0001-pty-proxy-vs-plugin.md`, `0002-vte-vs-vt100.md`, and
  `0003-osc7772-buffer-framing.md` — capture historical and current
  architectural decisions; `0004` and `0005` ship alongside the
  features they describe in this release.

## [0.10.0] - 2026-04-26

### Corrected

- **Substring/slice misconversion.** The spec converter previously emitted
  `column_extract` for `.substring(0, N)` and `.slice(0, N)` patterns, which are
  byte-offset operations, not whitespace-delimited columns. Affected generators
  now correctly report as requires-JS until the converter gains a substring/slice
  lowering (tracked in `docs/phase-minus-1-followups.md`).
  Affected specs: chezmoi, git, pass, pre-commit.
- **JSON.parse silent fallback.** When `JSON.parse` appeared without a resolvable
  field access, the converter silently emitted `{type: "json_extract", name: "name"}`,
  producing wrong completions. These generators now report as requires-JS.
  Affected specs: docker, podman.

Generators affected by either correction are tagged in the embedded specs with
`_corrected_in: "v0.10.0"`. `ghost-complete doctor` surfaces the count and names
them under the new corrected-generator warning check so users know which specs
silently changed behaviour.

`git.json` and `cd.json` have related deferred work tracked in
`docs/phase-minus-1-followups.md`.

### Added

- **`ghost-complete status --json`** — emits a structured JSON report with
  `schema_version`, `spec_counts`, and `coverage_trend` (nullable). Suppresses
  the human-readable output when present. Adds `--baseline <path>` for
  overriding the default `docs/coverage-baseline.json` lookup.
- **Coverage baseline (`docs/coverage-baseline.json`)** — committed JSON file
  tracking per-release coverage numbers. Populated at each release per the
  process documented in `docs/SPECS.md`. `ghost-complete status` now prints
  a "Coverage trend" section sourced from this file.
- **`ghost-complete validate-specs --json`** — newline-delimited JSON output
  with one object per spec plus a trailing `{"summary":{...}}` row. Designed
  for `jq 'select(.ok == false)'` filtering.
- **Dotted-path `json_extract` / `json_extract_array` transform** — the spec
  converter now recognises `JSON.parse(x).foo.bar.map(...)` patterns and emits
  declarative `json_extract_array` transforms with dotted-path lookups, nested
  indices, and bracketed-key syntax (`foo['bar'].baz`). 14 generators across
  `expo`, `expo-cli`, `pnpx`, `react-native`, and `scarb` converted from
  `requires_js` to native transforms.
- **`suffix` transform** — appends a fixed literal to each suggestion's text.
  Modelled on Fig's `{name: \`${x}=\`}` template-literal pattern. Used to
  hand-port `docker service scale` from `requires_js` to a declarative
  pipeline.
- **`docs/SPECS.md`** — conversion pipeline reference, hand-port vs converter
  extension guide, and the `_corrected_in` lifecycle doc.
- **Native provider pipeline** — eight async Rust providers replace
  JS-backed generators for the corresponding commands: `ansible-doc`
  (module list), `arduino-cli` (FQBNs and port addresses via one shared
  `arduino-cli board list --format json` call), macOS `defaults` (domain
  list), `mamba` / `conda env list` (environment names), `multipass list`
  (all instances, plus four state-filtered variants: running, stopped,
  deleted, not-deleted), and `pandoc` (input and output formats). Every
  provider subprocess enforces a 2-second timeout and returns an empty
  `Vec` on spawn failure, timeout, non-zero exit, or parse error, so a
  broken tool never stalls completion. Dispatch is wired through a
  closed-for-the-crate `ProviderKind` enum with 12 variants.
- **Build-time embedded-spec minification** — `crates/gc-suggest/build.rs`
  (new) reads every `specs/*.json` at compile time, strips the
  runtime-unused `js_source` field from each generator, minifies the
  remaining JSON, and writes the compacted copy into `OUT_DIR` for
  `include_str!` to bake into the binary. The release binary shrank from
  ~47 MB to ~28.42 MB — no behaviour change, on-disk `specs/*.json`
  remain pretty-printed; only the binary-embedded copies are minified.
  Adds `serde_json` under `[build-dependencies]`.
- **Priority-based ranking** — new `priority` field (`0..=100`) parsed on
  `Subcommand` and `Option` spec entries; the `SuggestionKind` base table
  (Subcommand=70, Flag=30, History=10, etc.) is the implicit default. Bundled
  specs ship priority values harvested from upstream Fig (3,248 lines across
  67 specs via converter re-run) plus heuristic bumps across 101 specs (918
  subcommand and 1,265 flag values) covering 11 families: vcs, package_manager,
  container, kubernetes, cloud, build_tool, ssh_remote, shell_builtin,
  file_modifier, http_tools, editor.
- **Cursor-context classifier** — new `Context` enum (`PathPrefix`, `FlagPrefix`,
  `Redirect`, `GitCheckout`, `Cd`, `SpecArg`, ...) routes `suggest_sync` based on
  what the user is typing. Replaces the unconditional filesystem fallback —
  `defer_to_git_refs` and the always-on fs branch are removed.
- **Bounded-block render (`popup.render_block_ms`)** — new config field
  (default 80ms, clamped to ≤200ms) races a high-priority async generator
  (git refs, provider values) against `tokio::time::sleep` before the first
  paint. When the generator wins, branches/values land in the same frame as
  flags; when it doesn't, the sync paint goes out and results merge on
  arrival. Implemented via `prepare_trigger_with_block` + `apply_block_result`
  two-phase split that releases the `std::sync::Mutex` before awaiting,
  with monotonic `buffer_generation` + `spawned_generation` counters
  defending against stale completions arriving after the user types more
  characters.
- **`tools/spec-priority-audit/`** — heuristics-driven priority injection for
  ~109 commonly-used specs across 11 families. Idempotent (never overwrites
  existing priorities), schema-validated at heuristic load time, atomic
  per-spec writes via temp+rename, duplicate-family guard, schema-drift
  warnings. 16 black-box Node subprocess tests gated in CI alongside the
  fig-converter tests.
- **`z` (zoxide) spec fleshed out** — replaces the `{"name":"z"}` stub with
  a variadic `keywords` arg backed by a `zoxide query --list` script
  generator (60s TTL, global cache), a folders template fallback, and
  static suggestions for `-` and `~`.
- **`fig-converter` preserves the `priority` field** — converter now writes
  `priority` on `Subcommand` and `Option` entries when present in upstream
  Fig sources. `typeof number` guard drops malformed values (string, null)
  rather than emitting them into converted JSON.
- **`bench/priority_sort` Criterion group** — two cases on a 10k-candidate
  mixed-kind pool with explicit priority overrides every 50th item:
  `empty_query_10k` ~735µs, `fuzzy_query_10k` ~631µs. Both well under the
  `<1ms / 10k-candidate` target.

### Changed

- **`ghost-complete status`** output now includes a "Coverage trend (vs
  previous release)" section at the end. The delta renderer has three
  cases: `(baseline)` for single-row bootstrap (no prior release to
  compare against), `(unchanged)` when a metric matches the previous
  release exactly, and signed `(+N)` / `(-N)` deltas otherwise.
- **Suggestion sort tuple** — primary keys are now
  `(history_bucket, score, priority, alpha)`. `priority` (`0..=100`) is the
  new tiebreaker between fuzzy-score-equal candidates; the explicit history
  partition stays as the first key so frecency boosts can't push history
  above domain content. `SuggestionKind::sort_priority` removed in favor of
  `priority::effective(...)`.
- **Ranking dispatch** — `suggest_sync` now branches off
  `Context::classify(...)` (6-context dispatch) instead of running an
  unconditional filesystem fallback. Specs that drive args via generators
  or omit a `filepaths` template no longer leak filesystem candidates.
- **Bundled spec priorities (manual)** — `cargo build/run/test` bumped to
  92/90/90 (the daily-driver Rust workflow, not `install`); `cargo install`
  dropped to 75; `docker compose` bumped 82→88 (multi-container dev is
  daily for many users); dangerous flags (`--force`, `--hard`, `--rm-all`)
  demoted to 20–25 so they sink below safer alternatives.

### Fixed

- **`docker service scale` completions** — hand-ported to a declarative
  pipeline; the trailing `=` (required by `docker service scale SERVICE=N`
  syntax) is now produced by the `suffix` transform. Previously surfaced as
  `requires_js` and did not complete.
- **Filesystem leakage on argument completion** — `git checkout`,
  `cargo run`, `npm install`, `docker run`, `kubectl get`, `ssh`, and `cd`
  no longer surface stray plain-file suggestions when the spec drives args
  via generators or omits a `filepaths` template. Locked in by 8 golden
  snapshot tests in `gc-suggest`.
- **`apply_block_result` re-ranks against actual `current_word`** — the
  merge step previously called `fuzzy::rank("", all, max_visible * 5)`
  regardless of typed query. Two latent bugs: irrelevant items merged when
  current_word was non-empty, and high-priority items past alphabetic
  position ~50 were silently dropped on empty queries. Now mirrors
  `try_merge_dynamic`'s empty-vs-non-empty branch.
- **Container/cloud `-f` cross-semantic bug** — heuristic iter 1 wrongly
  bumped `-f` (`--format`/`--filename`/`--follow` in container/cloud
  contexts) to priority 22 alongside `--force`. Iter 2 surgically reverts
  86 entries across docker (44), podman (26), netlify (7), supabase (2),
  minikube (2), limactl (1), vercel (1), wrangler (1), firebase (1).
- **Three review-loop iterations on PR #85** addressed 60+ findings:
  `Priority(u8)` Deserialize clamps to `0..=100` on both ends (negative
  inputs no longer abort whole-spec parse); dismiss-then-spawn ordering
  fixed on the visible-popup empty-sync path; orphan `dynamic_task` aborted
  on Phase 2 keystroke cancel; spec-priority-audit gains pre-write
  unknown-spec abort, schema-drift warnings, and best-effort tmp cleanup
  on rename failure.

## [0.9.1] - 2026-04-20

### Fixed

- **`git checkout <TAB>`** — native git ref generators now consistently surface branches/tags/remotes above history and filesystem residuals. When refs are still pending, the sync pass suppresses commands/options/history (but preserves filesystem candidates so `git checkout <path>` still works). The dynamic merge's empty-query branch sorts by `SuggestionKind` priority, so refs land at the top instead of being appended after sync residuals. (#73)

### Changed

- **README** — embed demo videos from `assets/` as mp4 (h.264) with clickable poster images; point at v0.9.0 release assets for hosted playback.

## [0.9.0] - 2026-04-19

### Added

- **VSCode terminal support** — Ghost Complete now runs as a first-class PTY proxy inside VSCode's integrated terminal, plus **VSCodium, Cursor, Windsurf, Positron, and Trae** (all detected via `VSCODE_IPC_HOOK_CLI`). Capability profile: `Synchronized` (DECSET 2026 via xterm.js) + native OSC 133. Coexists with VSCode's own shell integration (OSC 633) — the proxy forwards editor sequences untouched so command decorations, sticky scroll, and "run recent command" continue to work. Previously `shell/init.zsh`'s allowlist did not match VSCode, so users got a plain interactive shell with no proxy; it is now a first-class supported target.
- **Zed terminal support** — first-class support for Zed's integrated terminal, detected via `ZED_TERM=true`. Same capability profile as Ghostty/Kitty (Synchronized + native OSC 133).
- **`supported_terminals()`** grows from 7 to 9. `Terminal::Zed` and `Terminal::VSCode` enum variants added to `gc-terminal`; `for_zed()` and `for_vscode()` test constructors available behind `test-utils`.

### Changed

- **`shell/ghost-complete.{zsh,bash,fish}`** — introduced a `_gc_native_osc133` helper that short-circuits OSC 7771 emission when the host terminal already injects native OSC 133 (Ghostty, Zed) or its own shell integration that emits OSC 133 alongside proprietary markers (VSCode, signalled by `VSCODE_INJECTION=1`). Eliminates redundant/conflicting prompt markers in editor-hosted terminals.
- **`shell/init.zsh`** — non-tmux branch now resets an inherited `GHOST_COMPLETE_ACTIVE` when the parent process is not `ghost-complete`. This fixes the `code .` flow: a user launching VSCode from a ghost-complete-managed shell now gets the proxy in VSCode's integrated terminal instead of short-circuiting on the leaked env var. Tmux branch: `$ZED_TERM` and `$VSCODE_IPC_HOOK_CLI` added to the supported-terminals allowlist.

## [0.8.2] - 2026-04-18

### Added

- Embedded specs auto-materialize to `~/.cache/ghost-complete/embedded-specs/` on first run when no user-installed specs are found (enables zero-config `cargo install ghost-complete` usage).
- Logging section in README and CONFIGURATION docs explaining `--log-level`, `--log-file`, `RUST_LOG`, and default log path.
- `publish = false` on the `ghost-complete` binary crate to prevent accidental publish to crates.io.
- `--version` output now includes the git short SHA and build timestamp.
- SBOM / build provenance attestation on release artifacts.
- `deny.toml` + `cargo-deny` license/bans gate.
- Linux CI tripwire to catch accidental Darwin-only regressions.
- End-to-end smoke test covering the keystroke → popup → dismiss lifecycle.
- `cargo audit` workflow — runs `rustsec/audit-check@v2` on pushes to master, on PRs that touch `Cargo.toml`/`Cargo.lock`, and on a weekly cron to catch advisories filed mid-week.
- Release CI gated on `cargo test` / `cargo clippy -D warnings` / `cargo fmt --check` — a red-on-master tag push can no longer ship a release.
- Optional `lefthook` pre-commit hooks (fmt + clippy) mirroring CI. Opt-in via `brew install lefthook && lefthook install`.

### Changed

- **Spec resolution/loading perf regression fix** — removed the eager `OptionsIndex` HashMap (rebuilt per-subcommand descent for zero benefit vs. linear scan over <200 options) and replaced the `chars().any(is_control)` precheck with a flat byte-scan `has_control_char` (catches C0 directly and C1 via a two-byte UTF-8 match). Restores pre-audit numbers: `spec_resolution/shallow` ~3.17µs (was 4.46µs), `spec_resolution/deep` ~1.60µs (was 2.61µs), `spec_loading/load_717` ~70.7ms (was 94.8ms).
- **`validate-specs` performs deep validation** — regex patterns, transform pipelines, and generator types are now checked (previously only top-level JSON parse). New `--strict` flag promotes warnings to a non-zero exit.
- **Spec load-path hardening** — `Arc<GeneratorSpec>` eliminates per-keystroke deep clones; `sanitize_spec_strings` scrubs control chars from every user-visible string field; unknown generator types log a `warn!` on load; `validate_spec_generators` is iterative (no stack recursion on deep subcommand chains).
- **Suggest-engine perf** — in-place transform pipeline (no vec clone per stage); `Vec<Arc<str>>` for `$PATH`; `HashSet<&str>` in `try_merge_dynamic`; `Vec<char>` for trigger chars.
- **Frecency hardening** — merge-on-save (two-terminal union, max of decayed scores), schema-version envelope, 1e18 score clamp, `$XDG_STATE_HOME` respected with one-shot migration.
- **History loading** — tail-read for files >2MiB; (mtime,size) fingerprints replace mtime-only; zsh multi-line entry merge; strict-UTF-8 line validation (invalid lines skipped with `debug!` log instead of silently corrupted with U+FFFD).
- **Alias cache** now tracks every file the fast path reads (`.zsh_aliases`/`.aliases`/`.bash_aliases`), `.*.local` overrides, and a depth-bounded recursive mtime walk over `~/.oh-my-zsh/custom` to catch in-place drop-in edits (directory mtime alone misses them). Cry-wolf log dropped to `debug`.
- **CPR queue** — soft/hard caps with drop-oldest in `gc-parser`.
- **Popup trigger fingerprint guard** — fixes re-trigger-on-unchanged dismissing a visible popup with no re-render.
- **Terminal detection** — `GHOSTTY_RESOURCES_DIR` now requires an existing directory (parity with socket-based detection); a stale env var no longer misidentifies the terminal.
- **Config TUI editor** — array field round-trip, `Event::Resize` redraw, unsaved-change quit confirmation, mtime compare-and-swap on save, panic hook that restores the terminal, bounded backup suffix loop, `NotFound`-tolerant pre-backup path.
- **CLI** — `status --strict` flag + non-zero exit; `config` dump preserves comments via `toml_edit::DocumentMut`; `doctor` exercises the real `resolve_spec_dirs` + `SpecStore::load_from_dirs` chain and FAILs with an actionable message when zero specs load.
- **Build provenance** — `build.rs` emits git short SHA + UTC build timestamp into `--version` (tolerant of missing `.git`); `CARGO_HOME` path remap in `.cargo/config.toml`; release-artifact subject-path globs narrowed.
- Bash and fish shell integration scripts are idempotent when sourced multiple times (mirrors existing zsh behavior).
- Bash DEBUG trap chains with any pre-existing user trap instead of overwriting silently.
- Generator-drop message promoted from `debug` to `warn` so the proxy path surfaces broken pipelines at the same level as `validate-specs`.
- Workspace `tokio` features narrowed to the minimum required set (`rt-multi-thread`, `macros`, `io-util`, `fs`, `process`, `signal`, `time`, `sync`). Drops unused `net` feature and its `socket2` + `parking_lot` transitive deps.
- Clippy cleanup — collapse redundant match guards in overlay types and TUI editor; swap `map_or(true, …)` for `is_none_or(…)`; sync documented MSRV to `1.86` across AGENTS.md / CONTRIBUTING.md / Cargo.toml.

### Fixed

- **`SIGTERM` / `SIGHUP` no longer skip frecency flush** — PTY proxy now registers `SIGTERM` and `SIGHUP` listeners alongside `SIGWINCH` and breaks out to the existing cleanup block. Previously external `kill` or terminal hangup aborted tokio tasks before `flush_frecency()` / `config_watcher_handle.shutdown()` could run, losing accumulated frecency state on every non-EOF exit.
- **`RUST_LOG` now respected; empty `$SHELL` treated as missing** — `init_tracing` prefers `RUST_LOG` and falls back to `--log-level` only when the env var is unset or invalid. `resolve_default_shell` falls back to `/bin/zsh` when `$SHELL` is empty (previously propagated a cryptic `ENOENT` from the PTY spawn).
- Documentation drift: `docs/IMPLEMENTATION_PLAN.md` references now point to `docs/ARCHITECTURE.md`; MSRV documented as `1.75` corrected to `1.86`; crate count documented as `7` corrected to `8`; `theme.border` field added to the config table; spec counts synced to `709` specs / `180` requires_js.
- `rust_out` stray binary removed from repo root and added to `.gitignore`.
- Prior audit docs (`AUDIT_FINDINGS.md`, `AUDIT_RESOLUTION_PLAN.md`) marked archived.

### Security

- **`$HISTFILE` validated** — must canonicalize under canonicalized `$HOME` (catches symlinks escaping `$HOME`) and the basename must match a known history-filename pattern. On rejection, log a `warn!` and fall back to `~/.zsh_history` (which itself re-validates). Blocks arbitrary file reads via env var (e.g. `HISTFILE=/etc/passwd`).
- **Script generator argv rejects NUL bytes** — the only char that truncates argv on Unix. `substitute_template` substitutes empty for NUL-containing tokens at template time; `run_script` rejects any argv element containing NUL as defense-in-depth. Removed misleading shell-metacharacter warning — the exec path uses an argv array (not `sh -c`), so `|`, `;`, `&`, backtick, `$` are inert literals.
- **Spec text ANSI injection** — external spec `name` / `description` / subcommand / option fields are C0/CSI/OSC-stripped at load time (`sanitize_spec_strings`). Blocks terminal-escape injection via malicious user-installed specs; mirrors the render-side sanitizer for defense in depth.
- **Spec JSON depth cap** — external spec JSON rejected above 32 levels (flat byte-scan preflight, `check_json_depth`) to prevent stack overflow from nested-subcommand DoS (serde_json's default 128 was far above our deepest real-world spec at 15). `validate_spec_generators` is also iterative so a future cap relaxation cannot re-introduce the overflow.

## [0.8.1] - 2026-04-17

### Fixed

- **Ctrl+L 5s hang under z4h** — replaced the `cpr_pending: u8` counter with a FIFO request queue tagged by origin (`Ours` / `Shell`). Terminals respond to `CSI 6n` in request order, so popping the head dispatches each response to the correct owner without timing heuristics. Eliminates the class of races where overlapping CPRs collapsed into one flag, our-pending-then-shell starved the shell, and the 500ms-vs-5s expiry mismatch stalled redraws.
- **RPROMPT misalignment under p10k + z4h** — `_gc_report_buffer` is now chained into `zle-line-pre-redraw` directly instead of via `add-zle-hook-widget`. The hook-widget dispatcher renames `$WIDGET` to `azhw:zle-line-pre-redraw`, which broke z4h's `_zsh_highlight()` guard and caused syntax highlighting to run during prompt rendering — inflating the width measurement p10k uses for RPROMPT alignment. The chaining installer is idempotent and preserves any existing `zle-line-pre-redraw` widget identity.
- **Write-failure recovery for CPR requests** — `rollback_cpr` removes a pending `Ours` entry by token if the `CSI 6n` write fails, so a dropped request doesn't permanently steal a response slot. Stale `Ours` entries older than 30s are pruned once per proxy iteration as a leak guard against terminals that silently drop requests.

### Changed

- **Parser CPR API** — `cpr_pending` / `increment_cpr_pending` / `claim_cpr_response` removed in favor of `enqueue_cpr` / `claim_next_cpr` / `rollback_cpr` / `prune_stale_cpr` / `cpr_queue_len` with a `VecDeque<CprEntry>` backing store. `CprOwner` and `CprToken` are re-exported from `gc-parser`. `CSI 6n` auto-enqueues a `Shell` entry in the `vte::Perform` impl.
- **Proxy CPR dispatch** — extracted into `dispatch_cpr_response` helper; Task A pops the queue head and matches on owner, Task B enqueues `Ours` with a token and rolls back on write failure.
- **Shell integration test coverage** — new `tests/shell/test_zle_chaining.zsh` asserts the ZLE wrapper preserves `$WIDGET` and is idempotent across both install branches (chain and direct-install).

## [0.8.0] - 2026-04-12

### Added

- **Config TUI editor** — `ghost-complete config edit` opens an interactive terminal UI for editing configuration.

## [0.7.1] - 2026-04-12

### Fixed

- **Security hardening** — ANSI escape sequences in suggestion text are sanitized (prevents terminal injection via spec output). Shell-injecting paths in `.zshrc` init blocks are properly quoted. OSC 7 CWD reports rejected on path traversal; OSC 7770 buffer reports rejected on non-UTF-8 data. CPR response validation added. Terminal detection hardened — env var sanitization, stale socket detection, `TERM_PROGRAM` sanitization.
- **Crash resistance** — mutex poison recovery across all 12 proxy lock sites (a panic in one task no longer cascades). Narrow-terminal layouts no longer panic; defense-in-depth guard in `compute_layout`. Saturating arithmetic for u16 overflow, zero-dimension clamping, bounds-safe `spec_dirs` indexing, `byte_to_char_offset` clamping, double-panic recovery.
- **Unicode correctness** — wide characters (CJK, emoji) use `unicode-width` terminal column width with a CJK early-wrap branch. Cursor restore clamped after terminal resize. CPR count desync race eliminated.
- **Tokenizer parity** — `#` comments, FD redirects (`2>&1`), heredocs (`<<EOF`), here-strings (`<<<"x"`), and nested command substitution (`$(echo $(date))`) are now parsed correctly.
- **Stuck loading indicator** — dynamic popup spinner no longer gets stuck on empty/stale/disconnected generator results.
- **Orphaned generator tasks** — async generator tasks are aborted via `JoinHandle` on dismiss, preventing leaked tasks from older triggers.
- **Config robustness** — TOCTOU-safe load, atomic `create_new` for default config writes, hot-reload warnings on restart-required fields, non-UTF-8 config paths rejected with warning, unknown-key warnings via two-pass TOML load.
- **Overlay regressions** — `GUTTER_COLS` constant prevents nerd-font gutter math drift, loading+border deficit formula corrected, `scroll_offset` resets on deselect.
- **Install/doctor polish** — backup overwrite guard, unreadable entry counting, `doctor` is `multi_terminal`-aware, embedded spec counts, root-user guard, uninstall cleanup note.

### Changed

- **Performance** — script-generator stdout bounded at 1 MiB with concurrent stderr drain (prevents runaway memory). Suggestion cache uses LRU eviction via `CACHE_SWEEP_THRESHOLD`. Alias loading moved to async (`AliasStore` + `RwLock`). Frecency record/flush writes moved out of mutex hold. Full async git migration via `tokio::process` (no more blocking threads for git context). Regex patterns precompiled at spec load time.
- **Refactor** — `format_item` extracted into 6 helpers, `suggest_sync` branch logic into 8 helpers. `GUTTER_COLS` / `DESC_GAP_COLS` / `TRAILING_PAD_COLS` named constants replace magic numbers. `resolve_spec_dirs` deduplicated into dedicated module. `ThemeConfig::validate()` provides shape-only pre-load checks. Theme overrides now use `Option<String>` with a `ResolvedTheme` struct.
- **README** — centered header, tightened status language, renamed "What is this?" to "Overview", added star-history.com timeline.

## [0.7.0] - 2026-04-11

### Added

- **`auto_trigger` config flag** — disables all automatic triggers (debounce, auto_chars, CWD change) when set to `false`. Only manual keybinding (Ctrl+/) works. Hot-reloadable — toggling false while the popup is visible dismisses it and clears stale state.

### Fixed

- **CPR response forwarding for Atuin and other PTY programs** — ghost-complete was consuming all Cursor Position Report responses, starving programs like Atuin/crossterm that send their own CSI 6n requests. Now tracks pending CPR count and only consumes responses to its own requests, forwarding the rest through the PTY.

## [0.6.1] - 2026-04-04

### Fixed

- **Prevent recursive launch and enable per-pane proxy in tmux** — fixes recursive ghost-complete spawning and enables independent proxy instances per tmux pane.

## [0.6.0] - 2026-04-04

### Added

- **Frecency recording wired in production** — frecency scoring (added in v0.2.3) now records accepted completions. Every Tab/Enter acceptance calls `record_frecency()`, so the frecency database is no longer always empty.
- **Exponential decay algorithm** — replaced linear decay (`freq * 1/(1+t/168h)`) with exponential decay using single-number compression (`stored_score / 2^(t/72h)`). Full usage history compressed into one `f64` per entry. Half-life shortened from 1 week to 3 days.
- **Context-aware frecency keys** — argument completions keyed as `command\0kind\0text` so `--help` under `git` doesn't pollute `docker`. History items always keyed without command scope for consistency.
- **Frecency boosts all suggestion types** — files, flags, branches, subcommands all benefit from frecency boosting, not just history entries. Re-sorts after boosting while preserving history-comes-last ordering.

### Changed

- **Atomic frecency persistence** — writes via tmp+rename, batch saves every 3 accepts, flush on proxy shutdown. Prunes to 1000 entries on save.
- **Schema migration** — old `{frequency, last_used_secs}` format auto-migrated to `{stored_score, reference_secs}` on load.
- **Mutex poison recovery** — `FrecencyDb` lock uses poison recovery so a best-effort subsystem never crashes the proxy.

## [0.5.0] - 2026-04-03

### Added

- **Ctrl+A through Ctrl+Z keybindings** — full alphabet of ctrl keybindings now supported for custom actions.
- **CWD tracking via OSC 7** — filesystem completions now use the shell's actual working directory for accurate path resolution.
- **Rounded border on completion popup** — popup uses rounded Unicode box-drawing characters for a cleaner look.

### Fixed

- **Hardened keybindings, OSC 7 encoding, and border rendering** — fixes for edge cases in keybinding dispatch, percent-encoding in OSC 7 CWD URIs, and border character rendering.

## [0.4.1] - 2026-04-01

### Changed

- **Batch update 5 dependencies** — routine dependency bumps.

## [0.4.0] - 2026-03-31

### Added

- **Kitty, WezTerm, Alacritty, Rio terminal support** — Ghost Complete now supports 7 terminals on macOS. Kitty, WezTerm, and Rio have full parity with Ghostty (DECSET 2026 + OSC 133). Alacritty uses DECSET 2026 with shell integration prompt detection (no native OSC 133).
- **tmux detection for new terminals** — Kitty (`KITTY_WINDOW_ID`), WezTerm (`WEZTERM_UNIX_SOCKET`), and Alacritty (`ALACRITTY_SOCKET`) are now detected inside tmux sessions.

### Removed

- **`min_width` and `max_width` popup config fields** — popup width is now auto-sized. Existing configs with these fields continue to parse without error (silently ignored).
- **`generator_timeout_ms` suggest config field** — generator timeout is now hardcoded. Existing configs with this field continue to parse without error (silently ignored).
- **`max_history_entries` suggest config field** — replaced by `max_history_results` in v0.2.2. Existing configs with this field continue to parse without error (silently ignored).

### Changed

- **Experimental gate removed for known terminals** — all 7 supported terminals work without `[experimental] multi_terminal = true`. The flag now only applies to unknown/unlisted terminals.
- **Init block rewritten** — `.zshrc` init block detects Kitty via `KITTY_WINDOW_ID` before the `TERM_PROGRAM` case (Kitty reports `TERM_PROGRAM=xterm-kitty`). Supported terminals auto-exec without a config gate.
- **`known_term_programs()` renamed to `supported_terminals()`** — returns display names for all 7 terminals instead of `TERM_PROGRAM` values.

## [0.3.0] - 2026-03-28

### Added

- **Multi-terminal support (experimental)** — Ghost Complete now runs on **iTerm2** and **Terminal.app** in addition to Ghostty. Disabled by default; enable with `multi_terminal = true` under `[experimental]` in config.toml. Terminal detection is automatic via `TERM_PROGRAM` allowlist.
- **New `gc-terminal` crate** — encapsulates terminal detection, capability profiling, and render strategy selection. `TerminalProfile` struct with `RenderStrategy` and `PromptDetection` enums provides type-safe terminal abstraction.
- **OSC 7771 prompt boundary protocol** — terminal-agnostic prompt detection emitted by shell integration scripts alongside OSC 133. Works on all terminals regardless of native semantic prompt support.
- **tmux-in-iTerm2 support** — proxy auto-starts in tmux sessions launched from iTerm2 via `ITERM_SESSION_ID` detection.

### Changed

- **Rendering pipeline** — popup rendering conditionally uses DECSET 2026 synchronized output on Ghostty, falls back to pre-render buffer strategy (single `write()` atomicity) on iTerm2 and Terminal.app.
- **Init block** — `.zshrc` init block now uses a `case` statement: Ghostty always auto-execs, iTerm2/Terminal.app auto-exec only when `multi_terminal = true` is set in config (checked via grep at shell startup).
- **`doctor` command** — `check_ghostty()` replaced with `check_terminal()` that reports detected terminal name, render strategy, and prompt detection method. Lists all supported terminals on failure.
- **Shell integration scripts** — zsh, bash, and fish scripts now emit both OSC 133 and OSC 7771 markers for cross-terminal compatibility.

## [0.2.5] - 2026-03-25

### Added

- **`ghost-complete install --dry-run`** — previews what would be installed without writing any files. Shows the exact shell blocks needed for manual configuration.

### Changed

- **Graceful fallback for read-only .zshrc** — when `.zshrc` is not writable (e.g. nix-darwin/home-manager), install now prints colored manual instructions with the exact shell blocks instead of failing with an error. Only `PermissionDenied` triggers the fallback; other write errors propagate normally.
- **Install deploys zsh integration only** — bash and fish shell scripts are no longer deployed during install (not actively supported). Uninstall still cleans up legacy bash/fish scripts from prior installs.
- **Updated CLI help text** — `--help` output reflects zsh-only shell support.

## [0.2.4] - 2026-03-22

### Fixed

- **Popup suppressed during shell history navigation** — up/down arrow keys for history recall no longer trigger the debounce auto-suggest. A `debounce_suppressed` flag gates the debounce path, set on arrow up/down when the popup is hidden and cleared on printable input or manual trigger.
- **Spawned shell inherits parent working directory** — `CommandBuilder` was not inheriting the parent process's CWD, causing the shell to start in `$HOME`. This broke terminal multiplexers (e.g. cmux) that rely on restoring the working directory when reopening sessions. The current directory is now explicitly passed to `CommandBuilder`.

## [0.2.3] - 2026-03-16

### Added

- **Environment variable completion** — typing `$` in argument position suggests environment variables (`$HOME`, `$PATH`, etc.). Pre-filtered by typed prefix.
- **SSH host completion** — `ssh` arguments suggest hosts parsed from `~/.ssh/config`. Mtime-cached, skips wildcards, handles multiple hosts per line.
- **Shell alias resolution** — aliases like `alias g=git` are resolved before spec lookup, so `g push` uses the git spec. Reads dotfiles first (`.zsh_aliases`, `.aliases`, `.bash_aliases`), falls back to non-interactive subprocess with 2-second timeout.
- **Frecency scoring infrastructure** — commands scored by `frequency × recency` (half-life ~1 week). JSON persistence at `~/.config/ghost-complete/frecency.json` with batched saves and pruning to 1000 entries. Recording hook not yet wired — scoring is read-only in this release.
- **Config hot-reload** — watches `config.toml` via `notify` crate. Debounced (200ms), multi-stage validation (parse → theme → styles → keybindings). Invalid edits logged and ignored.
- **Loading indicator** — dimmed `...` footer row in popup when async script generators are pending.
- **Nerd Font icons** in popup gutter — terminal, chevron, flag, file, folder, branch, tag, link, history icons replace single-letter indicators.
- **`display_text()` helper** in `gc-overlay/src/util.rs` — shared basename extraction for consistent width calculation and rendering.
- **Test builders** — `make_visible_handler()` / `make_selected_handler()` in handler tests.

### Changed

- **Trailing space after accept** — accepting a non-directory suggestion appends a space so the user can immediately type the next argument. Skipped for `=`-terminated flags, history entries, and directories.
- **Single spec resolution** per trigger — previously resolved the spec tree 3 times (suggest_sync, has_script_generators, suggest_dynamic). Now `SyncResult` carries pre-resolved generators.
- **Dynamic merge re-ranking** — when async generators return, merged results are re-ranked against the current query.
- **History mtime refresh** — re-reads `~/.zsh_history` when file mtime changes instead of loading once at startup.
- **Unicode-width for popup sizing** — uses `unicode-width` crate for correct CJK/emoji terminal column width (2 columns per fullwidth character).
- **Light theme preset** — distinct colors for light terminal backgrounds (`fg:#1e1e2e bg:#dce0e8` selection, `fg:#d20f39` match highlight).
- **Basename in popup width** — width calculated from displayed basename, not full path.
- **Graceful mutex handling** — all long-lived async task locks use `match` with `tracing` logging on poison. `.unwrap()` retained in `spawn_blocking` I/O tasks where panic = correct termination.
- **Frecency error logging** — corrupt JSON logged at `warn`, unreadable file at `debug`, directory creation failure at `warn`.

### Fixed

- **Loading indicator stale on empty results** — Task E is now always notified when generators finish, even on empty or error results.
- **Description padding with non-ASCII text** — description column padding uses `unicode-width` character width, not byte length.

## [0.2.2] - 2026-03-15

### Added

- **`max_history_results` config field** — controls how many history entries appear in the popup (default: 5). Set to `0` to disable history entirely, which also skips loading `$HISTFILE` at startup. Replaces the binary `providers.history` toggle with a single numeric knob.

### Changed

- **`providers.history` removed from config** — replaced by `max_history_results`. Existing configs with `providers.history` continue to parse without error (the field is silently ignored).
- **History display cap** — history entries in the popup are now capped to `max_history_results` (default 5) after fuzzy scoring, regardless of how many slots remain in `max_results`. Previously, history could fill all remaining popup slots.

## [0.2.1] - 2026-03-15

### Changed

- **History entries insert full command on accept** — selecting a history entry from the popup now replaces the entire command buffer with the full historical command (e.g., `tmux source ~/.config/tmux/tmux.conf`), not just the first word (`tmux`).
- **Buffer-wide history matching** — history entries are fuzzy-matched against the full typed buffer at any word position, not just at command position. Typing `git push` surfaces `git push origin main` from history.
- **History suppressed in compound commands** — history entries no longer appear after pipe (`|`), chain (`&&`, `||`), or semicolon (`;`) operators. Full commands don't make sense as pipe/chain segments.
- **History result cap** — history results are capped to remaining `max_results` slots after main suggestions, preventing unbounded combined result sets.

### Added

- **`is_first_segment` field on `CommandContext`** — tracks whether the cursor is in the first command segment (before any `|`, `&&`, `||`, `;`). Used to gate history suggestions.

## [0.2.0] - 2026-03-14

### Added

- **706 Fig-compatible completion specs (34 → 706)** — converted from @withfig/autocomplete using offline Node.js converter (`tools/fig-converter/`). All specs embedded into the binary via `include_str!`. ~450 pure static, ~190 with script generators, ~66 with `requires_js` (static portions functional).
- **Script generators with async execution** — specs can define shell commands as generators (e.g., `["brew", "list", "-1"]`). Commands execute asynchronously with configurable timeout (default 5s). Results merge into the popup without resetting user's cursor position.
- **Transform pipeline** — composable output transforms for script generators: `split_lines`, `filter_empty`, `trim`, `skip_first`, `dedup`, `split_on(delim)`, `skip(n)`, `take(n)`, `regex_extract(pattern, groups)`, `json_extract(fields)`, `column_extract(cols)`, `error_guard(pattern)`. Validated at spec load time.
- **Generator result caching** — in-memory TTL cache for script generator results. Configurable per-generator with `cache_by_directory` option for CWD-scoped caching.
- **`ghost-complete status` subcommand** — shows loaded spec count, fully/partially functional breakdown, and lists commands requiring JS generators.
- **`ghost-complete doctor` subcommand** — health checks for shell integration, Ghostty detection, config validation (including all theme fields), and spec loading.
- **`ghost-complete config` subcommand** — dumps resolved configuration as TOML for debugging.
- **Scroll-to-make-room popup rendering** — popup always renders below the cursor. When near the bottom of the viewport, the terminal is scrolled to create space instead of rendering above. Scroll deficit persists across dismiss/re-trigger cycles. Popup dismissed on terminal resize.
- **Theme expansion** — three new theme fields: `match_highlight` (style for fuzzy-matched characters), `item_text` (style for non-selected rows), `scrollbar` (scrollbar track/thumb style). All configurable via `[theme]` in config.
- **Theme presets** — four built-in presets selectable via `preset = "dark"` in config: `dark` (default), `light`, `catppuccin`, `material-darker`.
- **Hex truecolor support** — `fg:#RRGGBB` and `bg:#RRGGBB` style tokens in theme configuration.
- **Fuzzy match character highlighting** — matched characters in popup items are visually highlighted using the `match_highlight` theme style.
- **Scrollbar indicator** — scrollable popup lists display a scrollbar when content exceeds the visible area.
- **ghost-complete self-completion spec** — autocomplete for ghost-complete's own subcommands and options.
- **claude and codex completion specs** — added specs for AI CLI tools.
- **Criterion benchmarks** — benchmark suites for `gc-suggest` (fuzzy ranking, spec loading, spec resolution, transform pipeline, engine) and `gc-parser` (VT parse throughput). Manually-triggered CI workflow for benchmark runs.

### Changed

- **`generator_timeout_ms` config option** — global timeout for shell command generators (default 5000ms).
- **`script_template` support** — generators can use `{current_token}` substitution in command arguments.
- **Binary size reduced from 104MB to 25MB** — dropped 11 oversized/niche specs: `aws` (53MB), `gcloud` (22MB), `hub` (deprecated), `fin`, `northflank`, `cl`, `commercelayer`, `sfdx`, `twilio`, `doppler`, `mongocli`.
- **`item_text` default changed from `dim` to empty** — non-selected rows now render with no extra styling by default.

### Fixed

- **Item text color bleed** — style is now reset before rendering descriptions, preventing `item_text` color from bleeding into description text.
- **Scroll deficit lost on dismiss** — scroll deficit now persists across dismiss/re-trigger cycles so the popup doesn't jump.
- **Doctor validates all theme fields** — `doctor` now checks all 5 theme fields (`selected`, `description`, `match_highlight`, `item_text`, `scrollbar`), not just the original 2.

## [0.1.4] - 2026-03-12

### Fixed

- **Popup rendering artifacts from long suggestions** — suggestion text (history URLs, deep paths) was written to the render buffer without truncation, overflowing past the popup's declared width. `clear_popup` only erased `layout.width` columns, leaving ghost characters on screen until a terminal resize. Text is now truncated to fit within the popup boundary.
- **Redundant path prefix in filesystem completions** — directory/file suggestions now display only the last path component (e.g., `2023-rust/` instead of `Desktop/coding/project/2023-rust/`), since the user already typed the prefix.

## [0.1.3] - 2026-03-10

### Added

- **16 new completion specs (18 → 34 total)** — tmux (85 subcommands), rustup (36 subcommands), node (57 options), wget, rsync, find, chmod, kill, killall, zip, unzip, ln, man, mvn, gradle, gradlew
- **tmux-in-Ghostty support** — ghost-complete now activates inside tmux sessions launched from Ghostty. Uses a PPID-based guard instead of `GHOST_COMPLETE_ACTIVE` env var to avoid inheritance through tmux. Adds tmux version logging at proxy startup.

### Fixed

- **Init block firing in non-Ghostty terminals** — the `.zshrc` init block now checks `TERM_PROGRAM == "ghostty"` before exec'ing ghost-complete, so VS Code integrated terminal, iTerm2, Terminal.app, etc. are no longer affected

## [0.1.2] - 2026-03-02

### Changed

- **Default trigger keybinding changed from Ctrl+Space to Ctrl+/** — Ctrl+Space (`0x00`) conflicts with tmux's prefix key, preventing the trigger from working inside tmux sessions. Ctrl+/ (`0x1F`) is distinct and unused by tmux or readline defaults. Users who prefer the old binding can set `trigger = "ctrl+space"` in their config.

### Added

- **`ctrl+/` key name** — now recognized by the keybinding parser alongside existing key names

## [0.1.1] - 2026-03-02

### Fixed

- **Multi-byte UTF-8 crash** — typing non-ASCII characters (e.g., `ą`, `ś`) no longer panics and kills the terminal session. Tokenizer rewritten to iterate over characters instead of raw bytes; cursor offset conversion from character to byte boundaries added throughout.
- **History suggestions polluting top results** — history completions now always sort after non-history suggestions, preserving score order within each group
- **`cd` showing files instead of directories** — spec resolution now takes priority over the `looks_like_path` heuristic, so `cd Desktop/` correctly filters to directories only
- **Accidental suggestion insertion on fast typing** — popup no longer auto-selects the first item. Tab and Enter with no selection forward the keystroke to the shell instead of inserting the top suggestion.

### Added

- **`../` parent directory shortcut for `cd`** — shown as the first suggestion when the current word is empty, with support for chaining (`../../`). Hidden at `/` and `$HOME` boundaries.

## [0.1.0] - 2026-03-01

### Added

- **PTY proxy engine** — transparent proxy between terminal and shell using `portable-pty` and `tokio`
- **VT parser** — escape sequence tracking via `vte` crate for cursor position, prompt boundaries (OSC 133), and CWD (OSC 7)
- **Command buffer reconstruction** — detects current command, argument position, pipes, and redirects
- **Suggestion engine** with providers:
  - Filesystem completions
  - `$PATH` command completions
  - Shell history completions
  - Git context completions (branches, remotes, tags, files)
  - Fig-compatible JSON spec completions
- **Fuzzy ranking** via `nucleo` (<1ms on 10k candidates)
- **ANSI popup rendering** with synchronized output (DECSET 2026), cursor save/restore, above/below positioning
- **18 completion specs**: brew, cargo, cd, curl, docker, gh, git, grep, jq, kubectl, make, npm, pip, pip3, python, python3, ssh, tar
- **Debounce-based auto-trigger** — configurable delay (default 150ms) after typing pauses
- **Manual trigger** via Ctrl+/ (works in zsh, bash, and fish)
- **Configurable keybindings** — accept, dismiss, navigate, trigger actions with fail-fast validation
- **Theme customization** — SGR-based style strings for selected item and description
- **TOML configuration** at `~/.config/ghost-complete/config.toml`
- **Install/uninstall CLI** — idempotent `.zshrc` management, spec deployment, shell script installation
- **Shell integration** for zsh (full), bash (Ctrl+/), and fish (Ctrl+/)
- **`validate-specs` subcommand** with colored output and item counts

[0.19.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.19.0
[0.18.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.18.0
[0.17.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.17.0
[0.16.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.16.0
[0.15.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.15.0
[0.14.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.14.0
[0.13.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.13.0
[0.12.3]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.12.3
[0.12.2]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.12.2
[0.12.1]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.12.1
[0.12.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.12.0
[0.11.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.11.0
[0.10.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.10.0
[0.9.1]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.9.1
[0.9.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.9.0
[0.8.2]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.8.2
[0.8.1]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.8.1
[0.8.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.8.0
[0.7.1]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.7.1
[0.7.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.7.0
[0.6.1]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.6.1
[0.6.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.6.0
[0.5.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.5.0
[0.4.1]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.4.1
[0.4.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.4.0
[0.3.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.3.0
[0.2.5]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.2.5
[0.2.4]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.2.4
[0.2.3]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.2.3
[0.2.2]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.2.2
[0.2.1]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.2.1
[0.2.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.2.0
[0.1.4]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.1.4
[0.1.3]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.1.3
[0.1.2]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.1.2
[0.1.1]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.1.1
[0.1.0]: https://github.com/StanMarek/ghost-complete/releases/tag/v0.1.0
