# Spec priority audit

Hand-curated priority bumps for bundled completion specs. The bundled
specs ship with a layer of explicit `priority` overrides on top of:

1. **Upstream Fig priorities** — picked up automatically by re-running
   the Fig converter (`tools/fig-converter/`). These reflect Fig's own
   ranking intent for subcommands the upstream community curated.
2. **Heuristic bumps** — applied by `apply.mjs` against the curated
   `heuristics.json` ruleset. Maps each spec family (vcs / package
   manager / container / kubernetes / cloud / build tool / ssh / shell
   builtin / http / editor) to a per-subcommand and per-flag table.
3. **Manual edits** — anywhere a maintainer needs to override a
   heuristic value or add a per-spec quirk.

## Sequencing

If you ever re-run the Fig converter and the heuristic together, do
them in this order:

```bash
# 1. Refresh from upstream Fig
npm --prefix tools/fig-converter run convert -- --output specs

# 2. Apply heuristic on top (never overwrites converter output)
node tools/spec-priority-audit/apply.mjs

# 3. Hand-tune anything specific
$EDITOR specs/<your-spec>.json
```

`apply.mjs` is idempotent: it only writes a `priority` field when one
isn't already present AND the new value differs from the kind base
(70 for subcommands, 30 for flags — writing those would be a no-op for
ordering).

## Tweaking the ruleset

Edit `heuristics.json` then re-run `apply.mjs`. The script never
overwrites existing `priority` values, so:

- To **lower** a value already set by the heuristic, edit
  `heuristics.json` AND remove the field from the affected `specs/*.json`
  before re-running. `git checkout HEAD -- specs/<spec>.json` is the
  fastest reset.
- To **add** a new family, append an entry to `families` in
  `heuristics.json` and list the spec basenames under `specs`.

## Dry run

```bash
node tools/spec-priority-audit/apply.mjs --dry-run
```

Reports what would change without writing.
