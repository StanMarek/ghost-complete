# 0004. Surface static `args.suggestions` at runtime

- **Status:** Accepted
- **Date:** 2026-04-28
- **Supersedes:** —
- **Superseded by:** —

## Context

384 of 709 bundled Fig specs (~54%) carry `args.suggestions` arrays — static
enum-like completions such as `git archive --format=tar|zip`,
`tar --atime-preserve replace|system`,
`curl --request GET|POST|PUT|DELETE|...`. Until this change,
`ArgSpec.suggestions` was deserialized as `Option<serde_json::Value>` and
discarded at runtime: users typing `git archive --format=` got nothing where
the spec author had explicitly enumerated the valid options.

The bundled spec corpus already encodes these enumerations — surfacing them is
purely a runtime gap, not a missing data problem.

## Decision

Replace `ArgSpec.suggestions: Option<serde_json::Value>` with a
strongly-typed `Vec<SuggestionEntry>`, where:

```rust
pub enum SuggestionEntry {
    Plain(String),
    Object(SuggestionObject),
}
pub struct SuggestionObject {
    pub name: Vec<String>,           // alias array; serde untags string-or-array
    pub description: Option<String>,
    pub kind: Option<String>,        // Fig "type" field; mapped to SuggestionKind
    pub priority: Option<Priority>,
    pub hidden: bool,
}
```

Add `SuggestionKind::EnumValue` at base priority **65**, slotting between
`Subcommand`/`ProviderValue` (70) and `EnvVar` (50). `key_tag()` returns
`"enum"` so its frecency bucket is independent — accepting `tar` as a
`--format` value does not boost `tar` typed elsewhere as a subcommand.

`SpecResolution` gains `static_suggestions: Vec<Suggestion>`. The engine
appends these to the candidate set **outside** the `suppress_commands` guard:
static suggestions are values for an arg slot, not commands, so they must
surface even when the user is filling a flag argument or past `--`.

`validate_arg_generators` runs at load time and:
- Drops entries where every name is empty/whitespace (with a warning).
- Drops `hidden: true` entries silently (the spec author's explicit signal).
- Warns once per spec on unknown `type` strings (no per-keystroke trace flood).

## Consequences

### Positive

- **Spec coverage.** ~54% of bundled specs now contribute completion data they
  previously withheld. `git archive --format=`, `tar --atime-preserve`,
  `curl --request`, and similar all surface tab-complete candidates at the
  right slot.
- **Strongly-typed deserialization.** Replacing `Option<serde_json::Value>`
  with `Vec<SuggestionEntry>` is strictly cheaper at parse time (no nested
  `Value` enum + heap-allocated `Map`/`Number`/`Array` per node) and rejects
  malformed shapes at deserialize time instead of at first-use.
- **Documented priority slot.** `EnumValue` at 65 is pinned by
  `types::kind_invariants::enum_value_contract` (exact value) and
  `priority::base_priorities_are_in_documented_order` (relative ordering against
  neighbours), with `docs/COMPLETION_SPEC.md` as the human-readable reference.
  Future drift requires a deliberate test edit.
- **Memory budget gate.** A new test `embedded_specs_under_memory_budget`
  walks the full `CompletionSpec` heap (including `args.suggestions`) and
  asserts the total stays under a fixed budget. Regression detection without
  external tooling. (The budget started at 64 MiB; `ux-8` raised it to
  128 MiB to admit the AWS spec, which alone contributes ~67 MiB of
  description text. zstd-compressing the embedded corpus is the queued
  reclaim path.)

### Negative

- **One enum variant added.** `SuggestionKind` is not `#[non_exhaustive]`, so
  adding `EnumValue` is a breaking change for any external `match` site over
  the enum. The internal `kind_icon` match in `gc-overlay::render` was
  extended in the same change. No public-API consumers exist outside the
  workspace.
- **Modest memory increase.** Strongly-typed parsing of the 384 specs with
  `suggestions` arrays adds ~600 KB to the spec heap. Comfortably under the
  budget (still well under after the `ux-8` 128 MiB bump).
- **Plan-deviation: the `if !preceding_flag_has_args` guard.** Wiring static
  suggestions surfaced a latent bug: `resolve_spec` was unconditionally
  collecting positional-arg generators even when filling a flag's argument
  (e.g. `pip install -r ` was mixing `-r`'s filepaths template with pip's
  positional package-name generators). The guard was added to skip positional
  collection in that case. Out of scope for the original spec but a strict
  semantic improvement that test coverage now pins.

### Neutral

- **Visibility note.** `SuggestionEntry`, `SuggestionObject`, every field on
  both, and `ArgSpec.suggestions` ship as `pub(crate)` (the Decision block
  above shows `pub` for schema clarity); the source-level docstrings carry
  the rationale.
- **`insertValue`, `displayName`, `replaceValue`, `icon`, `isDangerous`.**
  Deserialized but ignored in v1 — the engine has no cursor-positioning
  insertion API to honor `insertValue`. Tracked separately for v2.
- **Nested `generators` inside a Suggestion.** Fig's schema allows it; we
  don't. Tracked as a follow-up if a real spec needs it.
- **`deprecated` flag.** Entry is kept; no popup styling distinguishes it.
  Acceptable v1.

## Alternatives considered

- **Defer the feature.** Rejected — ~54% of bundled specs were carrying the
  data the user already expected to see. Closing the gap was the highest-ratio
  UX win available without writing any new spec data.
- **Reuse `SuggestionKind::ProviderValue` instead of adding `EnumValue`.**
  Rejected. `ProviderValue` is for *dynamic* native-provider output (npm pkg
  list, kubectl namespaces, etc.). Sharing the bucket would conflate frecency
  boosts: accepting `tar` as a static `--format` value would boost any
  *dynamic* provider value with the same string. Separate kind = separate
  bucket = correct behaviour.
- **Add a `static_suggestions: bool` flag on `Suggestion` and route through
  existing kinds.** Rejected. The kind / source / priority dimensions are
  already three orthogonal axes; adding a fourth would couple the ranker to
  the source pathway. New variant is the smaller surface change.
- **Keep `Option<serde_json::Value>` and lazy-parse at resolve time.**
  Rejected. Resolve runs on every keystroke; lazy-parse would re-deserialize
  the same `Value` ~hundreds of times during interactive completion.
  Strongly-typed parse-once is strictly cheaper and rejects malformed input
  earlier.

## References

- `crates/gc-suggest/src/specs.rs` — `SuggestionEntry`, `SuggestionObject`,
  `collect_static_suggestions`, validation
- `crates/gc-suggest/src/types.rs` — `SuggestionKind::EnumValue`, `key_tag`,
  `base_priority`
- `crates/gc-suggest/src/engine.rs::try_suggest_from_spec` — unconditional
  candidate extension
- `docs/COMPLETION_SPEC.md` — schema documentation + priority table
- `crates/gc-suggest/tests/static_suggestions_proptest.rs` — invariant fuzzer
- Fig schema reference: <https://fig.io/api/interfaces/Suggestion>
