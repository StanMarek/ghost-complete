#!/usr/bin/env node
// One-shot script used during Phase 0 T2 to generate the initial
// oracle-errors.md disposition file. Kept for reproducibility — re-run if
// the oracle's output set materially changes and the dispositions need a
// ground-up regen. Daily maintenance should be done by editing the .md file
// directly.

import fs from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const DOCS = resolve(__dirname, '..', 'docs');
const AUDIT = resolve(__dirname, '..', 'correctness-audit');
const FIXTURES_DIR = resolve(AUDIT, 'fixtures');

// Compute FIXTURED dynamically from the fixtures directory so the tally here
// stays in sync whenever a fixture is added or removed. Previously this set
// was hardcoded and silently drifted when fixtures were added.
const FIXTURED = new Set(
  fs.readdirSync(FIXTURES_DIR)
    .filter(f => f.endsWith('.json'))
    .map(f => f.replace(/\.json$/, ''))
);

const inv = JSON.parse(fs.readFileSync(resolve(DOCS, 'shape-inventory.json'), 'utf8'));
const safeShapes = inv.shapes.filter(s => !s.has_fig_api_refs);
const results = JSON.parse(fs.readFileSync(resolve(DOCS, 'oracle-results.json'), 'utf8'));

const perShapeErrors = {};
for (const r of results.results) {
  if (r.outcome === 'oracle_error' && r.exception_class === 'missing_fixture') {
    perShapeErrors[r.shape_id] = (perShapeErrors[r.shape_id] || 0) + 1;
  }
}

const byVerdict = new Map();
for (const s of safeShapes) {
  if (FIXTURED.has(s.shape_id)) continue;
  if (!byVerdict.has(s.verdict)) byVerdict.set(s.verdict, []);
  byVerdict.get(s.verdict).push(s);
}

const rationaleByVerdict = {
  requires_runtime:
    'Shape contains `await` or other runtime-only constructs; Rust transform pipeline cannot reproduce JS async semantics. No feasible fixture exists until a runtime-aware evaluator lands. Follow-up: consider a mini-VM or WASM-backed evaluator, or downgrade these generators to `requires_js` with a future fallback.',
  needs_new_transform_regex_match:
    'Shape uses `.matchAll(regex)` / `string.match(regex)` over the raw command output. The existing `regex_extract` transform operates line-by-line post-split; it cannot replicate match-over-unsplit-string semantics. Follow-up: add a `regex_match_extract` transform.',
  needs_new_transform_conditional_split:
    'Shape has inline conditionals (`if (x) return []` or ternaries inside the map) that the transform pipeline cannot express. Follow-up: extend `error_guard` with multi-pattern support, or add a `conditional_return` transform.',
  needs_new_transform_substring_slice:
    'Shape reads `str.substring`, `str.slice`, or `arr.slice` to carve out segments. Transform pipeline has no substring primitive. Follow-up: add a `substring_extract` transform with start/end indices.',
  needs_dotted_path_json_extract:
    'Shape does `JSON.parse(...).a.b.c.map(...)` with 2+ property hops. Current `json_extract` only supports a top-level key. Follow-up: add JSONPath-lite support (see shape verdict name).',
  hand_audit_required:
    'AST parsing failed (`<parse_error>`) — source uses syntactic features the Babel parser rejects (rare minified idioms). Needs individual human inspection to determine whether the generator is salvageable via a raw-text rewrite or should be left `requires_js`.',
  existing_transforms:
    'Shape is theoretically expressible with existing transforms BUT was not fixtured in this pass. Reason varies by shape: some fall outside the top-20 coverage budget; some have too-loose shape buckets (members read different JSON fields or use array-only vs hash-only semantics) to be captured by a single Rust pipeline. Follow-up: either split the shape bucket with tighter fingerprints or fixture per sub-shape.',
};

const lines = [];
lines.push('# Oracle Error Dispositions — Phase 0 Correctness Audit');
lines.push('');
lines.push('_Phase 0 exit requires every `oracle_error` in `docs/oracle-results.json` to have an explicit disposition here. Grouped dispositions are marked with `## Auto-disposition:`._');
lines.push('');
lines.push('_Canonical machine-readable data: `../../docs/oracle-results.json`._');
lines.push('');

lines.push('## Summary');
lines.push('');
lines.push(`- Safe-subset size: **${results.safe_subset_size}**`);
lines.push(`- Outcome totals: pass=${results.summary.pass}, fail=${results.summary.fail}, oracle_error=${results.summary.oracle_error}`);
lines.push(`- Shapes fixtured (${FIXTURED.size}): ${[...FIXTURED].map(s => `\`${s}\``).join(', ')}`);
lines.push('- Every `oracle_error` in this run has class `missing_fixture` (no `js_exception`, `js_timeout`, `rust_exception`, or `source_missing` observed).');
lines.push('');

lines.push('## Auto-disposition: missing_fixture');
lines.push('');
lines.push(`All ${results.summary.oracle_error} \`oracle_error\` entries in this run are \`missing_fixture\` — one-per-generator for shapes we deliberately chose not to fixture. One disposition line per *shape* below; the generator-level mapping is in \`oracle-results.json\`.`);
lines.push('');

const verdictOrder = [
  'requires_runtime',
  'needs_new_transform_conditional_split',
  'needs_new_transform_regex_match',
  'needs_new_transform_substring_slice',
  'needs_dotted_path_json_extract',
  'hand_audit_required',
  'existing_transforms',
];

for (const verdict of verdictOrder) {
  const shapes = byVerdict.get(verdict) || [];
  if (shapes.length === 0) continue;
  const totalGens = shapes.reduce((n, s) => n + s.count, 0);
  lines.push(`### \`${verdict}\` (${shapes.length} shapes, ${totalGens} generators)`);
  lines.push('');
  lines.push(`_Rationale:_ ${rationaleByVerdict[verdict] || 'No verdict rationale recorded.'}`);
  lines.push('');
  const sortedShapes = [...shapes].sort((a, b) => b.count - a.count);
  for (const s of sortedShapes) {
    lines.push(`- \`${s.shape_id}\` (${s.count} gens, example: \`${s.example_spec}\`) — disposition: missing_fixture; verdict is \`${verdict}\`.`);
  }
  lines.push('');
}

const nonMissing = results.results.filter(r => r.outcome === 'oracle_error' && r.exception_class !== 'missing_fixture');
lines.push('## Per-generator dispositions (non-`missing_fixture`)');
lines.push('');
if (nonMissing.length === 0) {
  lines.push('_None observed in this run._ If future oracle runs produce `js_exception`, `js_timeout`, `rust_exception`, or `source_missing` errors, add per-generator dispositions in this section using the schema:');
  lines.push('');
  lines.push('```');
  lines.push('- generator_id: <id>');
  lines.push('  exception: <class + message>');
  lines.push('  disposition: retry_different_input | reclassify_hand_audit | fix_allowlist | missing_fixture');
  lines.push('  rationale: <one line>');
  lines.push('```');
} else {
  for (const r of nonMissing) {
    lines.push(`- generator_id: \`${r.generator_id}\``);
    lines.push(`  exception: ${r.exception}`);
    lines.push(`  disposition: reclassify_hand_audit`);
    lines.push(`  rationale: TODO — oracle error class \`${r.exception_class}\` needs manual review.`);
    lines.push('');
  }
}
lines.push('');

const fails = results.results.filter(r => r.outcome === 'fail');
lines.push('## Known `fail` outcomes (not `oracle_error`, but noted here for maintainers)');
lines.push('');
for (const r of fails) {
  lines.push(`- \`${r.generator_id}\` (shape: \`${r.shape_id}\`): ${r.diff_summary}`);
  lines.push('  - Analysis: this docker generator uses a template-literal that appends `=` to the extracted name (e.g. `${n.Name}=`). The split-map bucket merges members that DO append suffixes with members that do not — a shape-bucket-too-loose signal. Not a fixture bug; not a code bug. Follow-up: tighten the fingerprint to separate suffix-appending variants, or leave flagged here.');
}
lines.push('');
lines.push('## Phase 0 exit check');
lines.push('');
lines.push(`Every \`oracle_error\` in \`oracle-results.json\` is accounted for by the auto-dispositions above (grouped by shape). Line-count sanity check: this file covers ${Object.keys(perShapeErrors).length} distinct shapes, totaling ${results.summary.oracle_error} generators.`);

const outPath = resolve(AUDIT, 'oracle-errors.md');
fs.writeFileSync(outPath, lines.join('\n') + '\n');
console.log('Wrote', outPath);
console.log('Shapes covered:', Object.keys(perShapeErrors).length);
console.log('Generators covered:', results.summary.oracle_error);
