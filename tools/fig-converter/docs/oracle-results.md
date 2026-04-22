# Oracle Results — Phase 0 Correctness Audit

_Canonical machine-readable data: [oracle-results.json](./oracle-results.json)_

- **Safe subset size**: 1038
- **Ran at**: 2026-04-22T19:30:53Z

## Summary

| Outcome | Count | % of safe subset |
|---------|-------|-------------------|
| pass | 439 | 42.3% |
| fail | 1 | 0.1% |
| oracle_error | 598 | 57.6% |

## Fixture Coverage (Top 20 Shapes)

| shape_id | count | fixtured | verdict | example_spec |
|----------|-------|----------|---------|--------------|
| `parse-map` | 174 | yes | `existing_transforms` | fly.json |
| `unknown` | 73 | skipped | `needs_new_transform_conditional_split` | bazel.json |
| `split-map` | 63 | yes | `existing_transforms` | docker.json |
| `unknown-2` | 58 | skipped | `requires_runtime` | meteor.json |
| `empty` | 50 | yes | `existing_transforms` | arduino-cli.json |
| `parse-map-2` | 50 | yes | `existing_transforms` | elm-json.json |
| `match-map` | 50 | skipped | `needs_new_transform_regex_match` | flutter.json |
| `arr` | 44 | skipped | `existing_transforms` | ng.json |
| `keys-map` | 38 | skipped | `needs_new_transform_conditional_split` | chezmoi.json |
| `parse-map-3` | 35 | yes | `existing_transforms` | cargo.json |
| `unknown-3` | 28 | yes | `existing_transforms` | conda.json |
| `entries-sort-map` | 22 | yes | `existing_transforms` | fly.json |
| `unknown-4` | 22 | skipped | `requires_runtime` | systemctl.json |
| `filter-map` | 19 | skipped | `existing_transforms` | eslint.json |
| `empty-2` | 18 | yes | `existing_transforms` | bosh.json |
| `arr-2` | 17 | skipped | `existing_transforms` | asdf.json |
| `arr-3` | 16 | skipped | `existing_transforms` | bun.json |
| `empty-3` | 16 | skipped | `requires_runtime` | bun.json |
| `arr-arr` | 16 | skipped | `needs_new_transform_conditional_split` | kubecolor.json |
| `parse-error` | 15 | skipped | `hand_audit_required` | dotnet.json |

## Per-shape outcomes

| shape_id | pass | fail | oracle_error | fixtured |
|----------|------|------|--------------|----------|
| `parse-map` | 174 | 0 | 0 | yes |
| `unknown` | 0 | 0 | 73 | no |
| `split-map` | 62 | 1 | 0 | yes |
| `unknown-2` | 0 | 0 | 58 | no |
| `empty` | 50 | 0 | 0 | yes |
| `parse-map-2` | 50 | 0 | 0 | yes |
| `match-map` | 0 | 0 | 50 | no |
| `arr` | 0 | 0 | 44 | no |
| `keys-map` | 0 | 0 | 38 | no |
| `parse-map-3` | 35 | 0 | 0 | yes |
| `unknown-3` | 28 | 0 | 0 | yes |
| `entries-sort-map` | 22 | 0 | 0 | yes |
| `unknown-4` | 0 | 0 | 22 | no |
| `filter-map` | 0 | 0 | 19 | no |
| `empty-2` | 18 | 0 | 0 | yes |
| `arr-2` | 0 | 0 | 17 | no |
| `arr-3` | 0 | 0 | 16 | no |
| `empty-3` | 0 | 0 | 16 | no |
| `arr-arr` | 0 | 0 | 16 | no |
| `parse-error` | 0 | 0 | 15 | no |
| `trim-split-filter-map` | 0 | 0 | 15 | no |
| `keys-map-2` | 0 | 0 | 14 | no |
| `parse-map-4` | 0 | 0 | 14 | no |
| `from` | 0 | 0 | 12 | no |
| `split-map-2` | 0 | 0 | 11 | no |
| `unknown-5` | 0 | 0 | 10 | no |
| `split-map-3` | 0 | 0 | 8 | no |
| `split-map-4` | 0 | 0 | 8 | no |
| `unknown-6` | 0 | 0 | 8 | no |
| `parse-map-5` | 0 | 0 | 7 | no |
| `trim-split-filter-map-2` | 0 | 0 | 7 | no |
| `unknown-7` | 0 | 0 | 7 | no |
| `from-map` | 0 | 0 | 7 | no |
| `split-map-5` | 0 | 0 | 6 | no |
| `arr-4` | 0 | 0 | 6 | no |
| `trim-split-map` | 0 | 0 | 5 | no |
| `from-map-2` | 0 | 0 | 5 | no |
| `unknown-8` | 0 | 0 | 5 | no |
| `empty-4` | 0 | 0 | 4 | no |
| `arr-5` | 0 | 0 | 4 | no |
| `arr-6` | 0 | 0 | 4 | no |
| `keys-map-3` | 0 | 0 | 4 | no |
| `includes` | 0 | 0 | 3 | no |
| `filter` | 0 | 0 | 3 | no |
| `arr-7` | 0 | 0 | 3 | no |
| `parse-map-6` | 0 | 0 | 3 | no |
| `keys-map-4` | 0 | 0 | 2 | no |
| `unknown-9` | 0 | 0 | 2 | no |
| `parse-map-7` | 0 | 0 | 2 | no |
| `arr-8` | 0 | 0 | 2 | no |
| `slice-some-map` | 0 | 0 | 1 | no |
| `empty-5` | 0 | 0 | 1 | no |
| `parse-map-8` | 0 | 0 | 1 | no |
| `parse-map-9` | 0 | 0 | 1 | no |
| `map` | 0 | 0 | 1 | no |
| `startswith-keys-map` | 0 | 0 | 1 | no |
| `values-map` | 0 | 0 | 1 | no |
| `isnan-isinteger` | 0 | 0 | 1 | no |
| `arr-9` | 0 | 0 | 1 | no |
| `filter-map-2` | 0 | 0 | 1 | no |
| `unknown-10` | 0 | 0 | 1 | no |
| `trim-split-map-2` | 0 | 0 | 1 | no |
| `map-2` | 0 | 0 | 1 | no |
| `map-3` | 0 | 0 | 1 | no |
| `map-4` | 0 | 0 | 1 | no |
| `split-filter-map` | 0 | 0 | 1 | no |
| `arr-arr-2` | 0 | 0 | 1 | no |
| `map-5` | 0 | 0 | 1 | no |
| `keys-reduce` | 0 | 0 | 1 | no |
| `trim-slice-split-filter-map` | 0 | 0 | 1 | no |
| `arr-10` | 0 | 0 | 1 | no |
| `map-6` | 0 | 0 | 1 | no |
| `split-slice-map` | 0 | 0 | 1 | no |

## Sample `fail` diffs (first 10)

- `docker:/subcommands[51]/subcommands[7]/args/generators[0]` (shape: `split-map`) — item[0] name/text mismatch: js="item1=" rust="item1"

## Notes

- Coverage target: top-20 shapes by count. Shapes outside the top 20 or with verdicts other than `existing_transforms` are skipped; see `../correctness-audit/oracle-errors.md` for auto-dispositions.
- Fixtures intentionally target shapes where the existing transform pipeline can plausibly reproduce the JS semantics. Shapes that require a new transform (dotted JSON paths, Object.entries-over-hash, conditional split) are flagged in the shape inventory and left for follow-up work.
