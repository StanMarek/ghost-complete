# UX-9 Phase 0 fixtures

Tiny spec fixtures introduced in UX-9 Phase 0 for the runtime to consume in
later phases.

| File | Phase that activates it | What it exercises |
|------|-------------------------|-------------------|
| `static_only.json` | Phase 0 | A fully functional non-JS command (subcommands + options, no generators). Counts as `commands_fully_functional`. |
| `partial_unsupported_js.json` | Phase 0 | One arg with a `requires_js: true` generator and no `js_runtime` metadata. The runtime drops the generator at resolution time; the static surface still completes. Counts as `commands_partially_functional` and contributes 1 to `requires_js_generators_unsupported`. |
| `name_mismatch.json` | Phase 1 | The JSON `name` (`alias-target`) does not match the file stem (`name_mismatch`). Phase 1 keys SpecStore on the file stem so the spec stays addressable as `name_mismatch`. |
| `duplicate_name_a.json` / `duplicate_name_b.json` | Phase 1 | Two files both declare `name: "duplicate"`. With today's name-keyed HashMap loader one of them is silently dropped — Phase 1 surfaces this as a `command_alias_conflicts` warning and keeps both addressable via their file stems. |
| `post_process_supported.json` | Phase 2 schema, runtime path activated in Phase 4 | Demonstrates a `post_process` JS runtime generator. The schema accepts it as of Phase 2; Phase 4 wires the runtime dispatch. |
| `custom_unsupported.json` | Phase 2 schema, runtime path activated in Phase 5 | Demonstrates a `custom` JS runtime generator (no `script`, JS function returns suggestions directly). The schema accepts it as of Phase 2; Phase 5 wires the runtime dispatch. |

Phase 0 deliberately did NOT load the runtime-JS fixtures into the runtime —
they existed so later phases would have a stable corpus to write tests
against. UX-9 Phase 2 lands the `js_runtime` field on `GeneratorSpec`, so the
two fixtures formerly in `parked/` are now top-level and parse cleanly under
`deny_unknown_fields`. Phases 4 and 5 add the runtime dispatch that actually
honours them.
