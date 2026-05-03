# UX-9 Phase 0 fixtures

Tiny spec fixtures introduced in UX-9 Phase 0 for the runtime to consume in
later phases.

| File | Phase that activates it | What it exercises |
|------|-------------------------|-------------------|
| `static_only.json` | Phase 0 | A fully functional non-JS command (subcommands + options, no generators). Counts as `commands_fully_functional`. |
| `partial_unsupported_js.json` | Phase 0 | One arg with a `requires_js: true` generator and no `js_runtime` metadata. The runtime drops the generator at resolution time; the static surface still completes. Counts as `commands_partially_functional` and contributes 1 to `requires_js_generators_unsupported`. |
| `name_mismatch.json` | Phase 1 | The JSON `name` (`alias-target`) does not match the file stem (`name_mismatch`). Phase 1 keys SpecStore on the file stem so the spec stays addressable as `name_mismatch`. |
| `duplicate_name_a.json` / `duplicate_name_b.json` | Phase 1 | Two files both declare `name: "duplicate"`. With today's name-keyed HashMap loader one of them is silently dropped — Phase 1 surfaces this as a `command_alias_conflicts` warning and keeps both addressable via their file stems. |
| `parked/post_process_supported.json` | Phase 2 → activated in Phase 4 | Parked until Phase 2 lands the `js_runtime` field on `GeneratorSpec` (`deny_unknown_fields` would currently reject it). Demonstrates a `post_process` JS runtime generator. |
| `parked/custom_unsupported.json` | Phase 2 → activated in Phase 5 | Parked for the same reason. Demonstrates a `custom` JS runtime generator (no `script`, JS function returns suggestions directly). |

Phase 0 deliberately does NOT load any of these fixtures into the runtime —
they exist so later phases have a stable corpus to write tests against. Tests
that load the parked fixtures must be gated until Phase 2 changes the
schema; loading them today produces a serde `unknown field js_runtime` error.
