// Phase 0 Correctness Oracle — Fig-API-free safe subset.
//
// Compares per-generator JS semantics (run in a locked-down `node:vm`) against
// a Rust transform pipeline (invoked via the `run-transforms` example binary)
// for every generator in `shape-inventory.json` with `has_fig_api_refs: false`.
//
// Inputs:
//   tools/fig-converter/docs/shape-inventory.json
//   specs/*.json  (for js_source lookup)
//   tools/fig-converter/correctness-audit/fixtures/<shape_id>.json
//
// Outputs:
//   tools/fig-converter/docs/oracle-results.json
//   tools/fig-converter/docs/oracle-results.md
//
// Invoke via `npm run oracle:changed` from `tools/fig-converter/`. This runs
// at TEST time only — never from runtime or release builds.
//
// Sandbox: plan §0.2 is prescriptive; DO NOT WIDEN. No `setTimeout`, `require`,
// `fetch`, `fs`, etc. A generator that needs any of those throws inside the
// VM and is classified `oracle_error: js_exception`.
//
// Rust helper: we build `cargo build --release --example run-transforms` ONCE
// at oracle startup and spawn the resulting binary per-invocation. That keeps
// end-to-end cost to ~10ms per spawn instead of ~1s per `cargo run`.
//
// Security note: `execFileSync` (not `execSync`) is used for the build step
// so no shell is invoked — args are a fixed array and no user input is
// interpolated. `spawnSync` likewise passes `[]` for argv; the only dynamic
// input is the JSON payload on stdin, which the Rust side parses with serde.

import { execFileSync, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync } from 'node:fs';
import { readFile, readdir, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);

// Layout — resolved relative to this source file so callers can run `node
// src/oracle.js` from anywhere.
const REPO_ROOT = resolve(__dirname, '..', '..', '..');
const SPECS_DIR = resolve(REPO_ROOT, 'specs');
const DOCS_DIR = resolve(__dirname, '..', 'docs');
const AUDIT_DIR = resolve(__dirname, '..', 'correctness-audit');
const FIXTURES_DIR = resolve(AUDIT_DIR, 'fixtures');
const RUST_BIN_REL = 'target/release/examples/run-transforms';

const VM_TIMEOUT_MS = 1000;

// Walk a loaded spec object and yield every `generators[i]` entry along with
// the dotted path that `shape-inventory.json` uses. Matches the walker in
// `scripts/run-spike.mjs`.
export function* walkGenerators(obj, path) {
  if (Array.isArray(obj)) {
    for (let i = 0; i < obj.length; i++) {
      yield* walkGenerators(obj[i], `${path}[${i}]`);
    }
  } else if (obj !== null && typeof obj === 'object') {
    for (const [k, v] of Object.entries(obj)) {
      if (k === 'generators' && Array.isArray(v)) {
        for (let i = 0; i < v.length; i++) {
          const g = v[i];
          if (g && typeof g === 'object') {
            yield { path: `${path}/generators[${i}]`, gen: g };
            yield* walkGenerators(g, `${path}/generators[${i}]`);
          }
        }
      } else {
        yield* walkGenerators(v, `${path}/${k}`);
      }
    }
  }
}

// Build a Map<generator_id, {js_source, basename}>. Every spec file is read
// once; walking is linear in spec size.
export async function buildGeneratorMap(specsDir = SPECS_DIR) {
  const files = (await readdir(specsDir)).filter(f => f.endsWith('.json')).sort();
  const map = new Map();
  for (const file of files) {
    const basename = file.replace(/\.json$/, '');
    const spec = JSON.parse(await readFile(join(specsDir, file), 'utf8'));
    for (const { path, gen } of walkGenerators(spec, '')) {
      if (typeof gen.js_source === 'string') {
        map.set(`${basename}:${path}`, { js_source: gen.js_source, basename });
      }
    }
  }
  return map;
}

// Freshly-minted sandbox per VM call (per plan §0.2). A single shared context
// would let a misbehaving generator pollute globals for later runs.
//
// `console` is bound to a no-op shim (not the real `console`) so that a
// generator's `console.error(e)` on a caught exception doesn't pollute oracle
// stderr with stack traces for every one of 1,038 generators. The sandbox
// still satisfies `typeof console === "object"` and the `.log/.error/.warn`
// surface every generator expects to exist.
const SILENT_CONSOLE = Object.freeze({
  log: () => {}, error: () => {}, warn: () => {}, info: () => {},
  debug: () => {}, trace: () => {}, dir: () => {}, assert: () => {},
  group: () => {}, groupEnd: () => {},
});

function makeSandbox() {
  return vm.createContext({
    JSON, Math, Array, Object, String, Number, Boolean,
    Map, Set, WeakMap, WeakSet, Promise, RegExp, Error,
    parseInt, parseFloat, isFinite, isNaN,
    Buffer, console: SILENT_CONSOLE,
    process: Object.freeze({ env: { ...process.env } }),
  });
}

// Execute `js_source` against `input` in an isolated vm context. `js_source`
// is wrapped as `(<source>)(INPUT)` — specs almost always carry either an
// arrow or a bare `function(...)`. Returns `{ outcome: 'ok', value }` or
// `{ outcome: 'error', kind, message }`. Promise returns are awaited.
//
// `kind` may be `js_timeout` or `js_exception`. The caller maps these to
// `oracle_error` classes.
export async function runJsGenerator(js_source, input) {
  const ctx = makeSandbox();
  ctx.__INPUT__ = input;
  const wrapped = `(${js_source})(__INPUT__)`;
  let value;
  try {
    value = vm.runInContext(wrapped, ctx, { timeout: VM_TIMEOUT_MS });
  } catch (e) {
    // Distinguish timeout (vm.runInContext throws a specific error) from
    // arbitrary runtime exceptions. The `code` property on timeout errors is
    // `ERR_SCRIPT_EXECUTION_TIMEOUT`.
    if (e && e.code === 'ERR_SCRIPT_EXECUTION_TIMEOUT') {
      return { outcome: 'error', kind: 'js_timeout', message: e.message };
    }
    return { outcome: 'error', kind: 'js_exception', message: `${e.name || 'Error'}: ${e.message}` };
  }

  // Unwrap Promises if the source was async. runInContext does not await.
  if (value && typeof value.then === 'function') {
    try {
      value = await value;
    } catch (e) {
      return { outcome: 'error', kind: 'js_exception', message: `${e.name || 'Error'}: ${e.message}` };
    }
  }

  return { outcome: 'ok', value };
}

// Invoke the prebuilt Rust helper with a `{transforms, input}` payload.
// Returns `{ outcome: 'ok', value: [{text, description, ...}, ...] }` on
// success, or `{ outcome: 'error', kind, message }` if the helper errored.
export function runRustPipeline(rustBin, transforms, input) {
  const payload = JSON.stringify({ transforms, input });
  const proc = spawnSync(rustBin, [], { input: payload, encoding: 'utf8' });
  if (proc.error) {
    return { outcome: 'error', kind: 'rust_spawn_error', message: proc.error.message };
  }
  if (proc.status !== 0) {
    // The binary writes `{error: "..."}` to stdout on failure; parse it if
    // possible, otherwise pass the raw stderr as the message.
    let parsed = null;
    try { parsed = JSON.parse(proc.stdout || '{}'); } catch { /* empty */ }
    const msg = (parsed && parsed.error) || proc.stderr || `exit ${proc.status}`;
    return { outcome: 'error', kind: 'rust_exception', message: msg };
  }
  let parsed;
  try {
    parsed = JSON.parse(proc.stdout);
  } catch (e) {
    return { outcome: 'error', kind: 'rust_exception', message: `stdout parse: ${e.message}` };
  }
  if (parsed.error) {
    return { outcome: 'error', kind: 'rust_exception', message: parsed.error };
  }
  return { outcome: 'ok', value: parsed.output || [] };
}

// Compare JS array of suggestion-objects to Rust array of suggestion-objects.
// JS fields: name, description, ... — the Rust `text` field is equivalent to
// JS `name`. Rust-side `kind`/`source`/`score`/`match_indices` are metadata
// with no JS counterpart and are ignored.
//
// Returns `{ equal: true }` on match, `{ equal: false, reason }` otherwise.
//
// Ordering assumption (IMPORTANT for fixture authors):
//
//   The oracle does strict index-by-index comparison. It assumes BOTH the JS
//   generator and the Rust transform pipeline produce outputs in the SAME
//   order for a given input. The 8 current fixtures don't exercise this —
//   their outputs are either empty or single-item — but multi-item fixtures
//   added later could flake if one side's ordering isn't deterministic (e.g.
//   hash/map-iteration order, sort stability differences, parallelism).
//
//   Guidance for T3/T5 authors adding multi-item fixtures:
//     1. Prefer inputs where both pipelines produce the SAME ordering. This
//        is almost always the case when the JS source uses plain `.map(...)`
//        over a deterministic iteration source (split lines, array of known
//        shape) — which matches how the Rust `split_lines` / `regex_extract`
//        chain behaves.
//     2. If an input naturally produces unordered output (hash/map iteration,
//        set enumeration, JSON.parse + `Object.entries`), split the fixture
//        into single-item inputs instead of one multi-item input. Single-item
//        outputs sidestep ordering entirely.
//     3. DO NOT silently switch to unordered comparison by default. If an
//        unordered-comparison mode ever becomes necessary, add an explicit
//        fixture-level opt-in flag (e.g. `unordered: true` in the fixture
//        JSON) so the oracle's default behaviour stays deterministic and
//        reviewers see the opt-in in diffs.
//
//   This comment is documentation only — the `unordered` flag is NOT
//   implemented here. Implement it when (and only when) a fixture actually
//   requires it.
export function compareResults(jsValue, rustValue) {
  if (!Array.isArray(jsValue)) {
    return { equal: false, reason: `js returned non-array: ${typeof jsValue} ${JSON.stringify(jsValue)?.slice(0, 80)}` };
  }
  if (!Array.isArray(rustValue)) {
    return { equal: false, reason: `rust returned non-array` };
  }
  if (jsValue.length !== rustValue.length) {
    return { equal: false, reason: `length mismatch: js=${jsValue.length}, rust=${rustValue.length}` };
  }
  for (let i = 0; i < jsValue.length; i++) {
    const j = jsValue[i];
    const r = rustValue[i];
    if (!j || typeof j !== 'object') {
      return { equal: false, reason: `js[${i}] is not an object: ${JSON.stringify(j)?.slice(0, 80)}` };
    }
    if (!r || typeof r !== 'object') {
      return { equal: false, reason: `rust[${i}] is not an object` };
    }
    const jName = typeof j.name === 'string' ? j.name : undefined;
    const rText = typeof r.text === 'string' ? r.text : undefined;
    if (jName !== rText) {
      return { equal: false, reason: `item[${i}] name/text mismatch: js=${JSON.stringify(jName)} rust=${JSON.stringify(rText)}` };
    }
    // Only compare description when BOTH sides have one. Rust emits `null`
    // when unset; normalize that to `undefined` for the comparison.
    const jDesc = typeof j.description === 'string' ? j.description : undefined;
    const rDesc = typeof r.description === 'string' ? r.description : undefined;
    if (jDesc !== undefined && rDesc !== undefined && jDesc !== rDesc) {
      return { equal: false, reason: `item[${i}] description mismatch: js=${JSON.stringify(jDesc)} rust=${JSON.stringify(rDesc)}` };
    }
  }
  return { equal: true };
}

// Load the fixture for a shape, or return `null` if absent.
async function loadFixture(shapeId) {
  const p = join(FIXTURES_DIR, `${shapeId}.json`);
  try {
    const raw = await readFile(p, 'utf8');
    return JSON.parse(raw);
  } catch (e) {
    if (e.code === 'ENOENT') return null;
    throw e;
  }
}

// Ensure the Rust helper binary exists and is usable; build once if needed.
function ensureRustHelper({ skipBuild = false } = {}) {
  const rustBin = resolve(REPO_ROOT, RUST_BIN_REL);
  if (!skipBuild) {
    console.log('Building Rust helper binary (release)...');
    // execFileSync (not execSync) — no shell is invoked, argv is a fixed
    // array, so no command injection surface regardless of REPO_ROOT.
    execFileSync(
      'cargo',
      ['build', '--release', '--example', 'run-transforms', '-p', 'gc-suggest'],
      { cwd: REPO_ROOT, stdio: 'inherit' },
    );
  }
  if (!existsSync(rustBin)) {
    throw new Error(`Rust helper missing at ${rustBin}`);
  }
  return rustBin;
}

// Aggregate per-input results into a single generator outcome.
//
//   - any pass + no fail + no oracle_error => pass
//   - any fail                             => fail
//   - no pass + all oracle_error same class => oracle_error (with that class)
//   - no pass + mixed oracle_error classes  => oracle_error (first class seen)
//
// Multi-error aggregation (fixing issue I2):
//
//   When a generator errors on 2+ inputs, `exception` and `exception_class`
//   still surface the FIRST error (backward-compatible — existing consumers
//   that only read those two fields keep working). But we ALSO:
//
//     1. Suffix `(+N more errors)` to the `exception` message whenever the
//        generator had >1 error, so a reader eyeballing `exception` alone
//        knows there's more than one data point.
//     2. When the errors span multiple distinct classes, add an
//        `exception_classes` sibling field containing every distinct class
//        (insertion-ordered) so disposition authors can see the full picture
//        in `oracle-results.json`. Homogeneous-class runs omit this field to
//        keep the JSON diff small.
//
//   This is a pure additive change on the JSON schema — no existing field is
//   removed or renamed.
export function summarizeInputs(inputResults) {
  const passes = inputResults.filter(r => r.kind === 'pass');
  const fails = inputResults.filter(r => r.kind === 'fail');
  const errors = inputResults.filter(r => r.kind === 'oracle_error');

  if (fails.length > 0) {
    return { outcome: 'fail', diff_summary: fails[0].reason };
  }
  if (passes.length > 0 && errors.length === 0) {
    return { outcome: 'pass' };
  }
  // Both the "some passed, some errored" branch and the "all errored" branch
  // share the same aggregation logic — keep the paths unified.
  if (errors.length > 0) {
    const first = errors[0];
    const distinctClasses = [];
    for (const e of errors) {
      if (!distinctClasses.includes(e.exception_class)) {
        distinctClasses.push(e.exception_class);
      }
    }
    let exception = first.exception;
    const extra = errors.length - 1;
    if (extra > 0) {
      exception = `${exception} (+${extra} more error${extra === 1 ? '' : 's'})`;
    }
    const summary = {
      outcome: 'oracle_error',
      exception,
      exception_class: first.exception_class,
    };
    if (distinctClasses.length > 1) {
      summary.exception_classes = distinctClasses;
    }
    return summary;
  }
  return { outcome: 'oracle_error', exception: 'no inputs in fixture', exception_class: 'missing_fixture' };
}

export async function runOracle({ skipBuild = false } = {}) {
  // ----- Load inputs
  const inventoryPath = join(DOCS_DIR, 'shape-inventory.json');
  const inventory = JSON.parse(await readFile(inventoryPath, 'utf8'));

  const safeShapes = inventory.shapes.filter(s => s.has_fig_api_refs === false);
  const safeGenTotal = safeShapes.reduce((n, s) => n + s.count, 0);
  console.log(`Safe subset: ${safeShapes.length} shapes, ${safeGenTotal} generators.`);

  const genMap = await buildGeneratorMap();
  console.log(`Loaded ${genMap.size} generator sources from specs/.`);

  const rustBin = ensureRustHelper({ skipBuild });

  // ----- Preload fixtures (one per shape).
  const fixtureByShape = new Map();
  for (const shape of safeShapes) {
    const fx = await loadFixture(shape.shape_id);
    if (fx) fixtureByShape.set(shape.shape_id, fx);
  }
  console.log(`Loaded ${fixtureByShape.size} fixtures.`);

  // ----- Run every safe generator through the oracle.
  const results = [];
  const perShapeCounts = new Map();

  function bumpShape(shapeId, key) {
    if (!perShapeCounts.has(shapeId)) {
      perShapeCounts.set(shapeId, { pass: 0, fail: 0, oracle_error: 0 });
    }
    perShapeCounts.get(shapeId)[key]++;
  }

  let processed = 0;
  for (const shape of safeShapes) {
    const fixture = fixtureByShape.get(shape.shape_id);
    for (const generatorId of shape.generator_ids) {
      processed++;
      if (processed % 100 === 0) {
        process.stdout.write(`  [${processed}/${safeGenTotal}]\n`);
      }

      const sourceEntry = genMap.get(generatorId);
      if (!sourceEntry) {
        results.push({
          generator_id: generatorId,
          shape_id: shape.shape_id,
          outcome: 'oracle_error',
          exception: 'source_missing: js_source not found in specs/',
          exception_class: 'source_missing',
        });
        bumpShape(shape.shape_id, 'oracle_error');
        continue;
      }

      if (!fixture) {
        results.push({
          generator_id: generatorId,
          shape_id: shape.shape_id,
          outcome: 'oracle_error',
          exception: `missing_fixture: no fixture for shape ${shape.shape_id}`,
          exception_class: 'missing_fixture',
        });
        bumpShape(shape.shape_id, 'oracle_error');
        continue;
      }

      const inputResults = [];
      for (const input of fixture.inputs) {
        const jsResult = await runJsGenerator(sourceEntry.js_source, input);
        if (jsResult.outcome === 'error') {
          inputResults.push({ kind: 'oracle_error', exception_class: jsResult.kind, exception: `${jsResult.kind}: ${jsResult.message}` });
          continue;
        }
        const rustResult = runRustPipeline(rustBin, fixture.expected_transforms, input);
        if (rustResult.outcome === 'error') {
          inputResults.push({ kind: 'oracle_error', exception_class: rustResult.kind, exception: `${rustResult.kind}: ${rustResult.message}` });
          continue;
        }
        const cmp = compareResults(jsResult.value, rustResult.value);
        if (cmp.equal) {
          inputResults.push({ kind: 'pass' });
        } else {
          inputResults.push({ kind: 'fail', reason: cmp.reason });
        }
      }

      const summary = summarizeInputs(inputResults);
      const entry = {
        generator_id: generatorId,
        shape_id: shape.shape_id,
        outcome: summary.outcome,
      };
      if (summary.outcome === 'fail') entry.diff_summary = summary.diff_summary;
      if (summary.outcome === 'oracle_error') {
        entry.exception = summary.exception;
        entry.exception_class = summary.exception_class;
        if (summary.exception_classes) {
          entry.exception_classes = summary.exception_classes;
        }
      }
      results.push(entry);
      bumpShape(shape.shape_id, summary.outcome);
    }
  }

  // ----- Write oracle-results.json
  const summary = {
    pass: results.filter(r => r.outcome === 'pass').length,
    fail: results.filter(r => r.outcome === 'fail').length,
    oracle_error: results.filter(r => r.outcome === 'oracle_error').length,
  };

  const resultsJson = {
    schema_version: '1.0',
    safe_subset_size: safeGenTotal,
    ran_at: new Date().toISOString().replace(/\.\d{3}Z$/, 'Z'),
    results,
    summary,
  };

  mkdirSync(DOCS_DIR, { recursive: true });
  const resultsPath = join(DOCS_DIR, 'oracle-results.json');
  await writeFile(resultsPath, JSON.stringify(resultsJson, null, 2) + '\n', 'utf8');
  console.log(`Wrote ${resultsPath}`);

  // ----- Write oracle-results.md
  const md = generateMarkdown(resultsJson, safeShapes, perShapeCounts, fixtureByShape);
  const mdPath = join(DOCS_DIR, 'oracle-results.md');
  await writeFile(mdPath, md, 'utf8');
  console.log(`Wrote ${mdPath}`);

  console.log('\n=== SUMMARY ===');
  console.log(`  pass         : ${summary.pass}`);
  console.log(`  fail         : ${summary.fail}`);
  console.log(`  oracle_error : ${summary.oracle_error}`);

  return resultsJson;
}

function generateMarkdown(resultsJson, safeShapes, perShapeCounts, fixtureByShape) {
  const { safe_subset_size, ran_at, summary, results } = resultsJson;
  const top20 = [...safeShapes].sort((a, b) => b.count - a.count).slice(0, 20);
  const lines = [];
  lines.push('# Oracle Results — Phase 0 Correctness Audit');
  lines.push('');
  lines.push('_Canonical machine-readable data: [oracle-results.json](./oracle-results.json)_');
  lines.push('');
  lines.push(`- **Safe subset size**: ${safe_subset_size}`);
  lines.push(`- **Ran at**: ${ran_at}`);
  lines.push('');
  lines.push('## Summary');
  lines.push('');
  lines.push('| Outcome | Count | % of safe subset |');
  lines.push('|---------|-------|-------------------|');
  lines.push(`| pass | ${summary.pass} | ${((summary.pass / safe_subset_size) * 100).toFixed(1)}% |`);
  lines.push(`| fail | ${summary.fail} | ${((summary.fail / safe_subset_size) * 100).toFixed(1)}% |`);
  lines.push(`| oracle_error | ${summary.oracle_error} | ${((summary.oracle_error / safe_subset_size) * 100).toFixed(1)}% |`);
  lines.push('');
  lines.push('## Fixture Coverage (Top 20 Shapes)');
  lines.push('');
  lines.push('| shape_id | count | fixtured | verdict | example_spec |');
  lines.push('|----------|-------|----------|---------|--------------|');
  for (const shape of top20) {
    const fixtured = fixtureByShape.has(shape.shape_id) ? 'yes' : 'skipped';
    lines.push(`| \`${shape.shape_id}\` | ${shape.count} | ${fixtured} | \`${shape.verdict}\` | ${shape.example_spec} |`);
  }
  lines.push('');
  lines.push('## Per-shape outcomes');
  lines.push('');
  lines.push('| shape_id | pass | fail | oracle_error | fixtured |');
  lines.push('|----------|------|------|--------------|----------|');
  const byCount = [...safeShapes].sort((a, b) => b.count - a.count);
  for (const shape of byCount) {
    const c = perShapeCounts.get(shape.shape_id) || { pass: 0, fail: 0, oracle_error: 0 };
    const fixtured = fixtureByShape.has(shape.shape_id) ? 'yes' : 'no';
    lines.push(`| \`${shape.shape_id}\` | ${c.pass} | ${c.fail} | ${c.oracle_error} | ${fixtured} |`);
  }
  lines.push('');
  const failExamples = results.filter(r => r.outcome === 'fail').slice(0, 10);
  if (failExamples.length > 0) {
    lines.push('## Sample `fail` diffs (first 10)');
    lines.push('');
    for (const f of failExamples) {
      lines.push(`- \`${f.generator_id}\` (shape: \`${f.shape_id}\`) — ${f.diff_summary}`);
    }
    lines.push('');
  }
  lines.push('## Notes');
  lines.push('');
  lines.push('- Coverage target: top-20 shapes by count. Shapes outside the top 20 or with verdicts other than `existing_transforms` are skipped; see `../correctness-audit/oracle-errors.md` for auto-dispositions.');
  lines.push('- Fixtures intentionally target shapes where the existing transform pipeline can plausibly reproduce the JS semantics. Shapes that require a new transform (dotted JSON paths, Object.entries-over-hash, conditional split) are flagged in the shape inventory and left for follow-up work.');
  return lines.join('\n') + '\n';
}

// Direct-invocation entrypoint: `node src/oracle.js`
const isMain = import.meta.url === `file://${process.argv[1]}`;
if (isMain) {
  runOracle().catch(err => {
    console.error('Oracle error:', err);
    process.exit(1);
  });
}
