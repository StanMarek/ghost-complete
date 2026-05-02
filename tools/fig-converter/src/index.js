/**
 * index.js
 *
 * Full Fig → Ghost Complete spec conversion pipeline.
 *
 * Usage: node src/index.js --output <dir> [--specs <name1,name2,...>]
 *
 * Pipeline per spec:
 *   1. Load spec via dynamic import from @withfig/autocomplete
 *   2. Convert static structure (static-converter.js)
 *   3. Resolve loadSpec references (inline sub-specs)
 *   4. Process generators:
 *      a. Native generator map check (git branch → git_branches)
 *      b. script + postProcess → pattern match → transforms
 *      c. script + splitOn → transforms: ["split_lines"]
 *      d. script (function) → requires_js: true
 *      e. custom generators → requires_js: true
 *   5. Write JSON to output directory
 *
 * Batched orchestration:
 *   Single-process conversion of the full @withfig/autocomplete corpus (~705
 *   specs) exceeds Node's heap even at --max-old-space-size=8192 because the
 *   ESM dynamic import cache retains every spec module. The orchestrator
 *   therefore splits the spec list into batches (default 30) and spawns a
 *   fresh Node subprocess per batch; each child exits after its batch,
 *   freeing its entire module cache. Small explicit --specs runs (≤ batch
 *   size) stay in-process for snappy debugging.
 *
 *   Workers are invoked via the internal --batch-worker flag. Workers emit
 *   exactly one line of JSON on stdout ({totals, errors}); progress goes to
 *   stderr. See §3 of docs/phase-minus-1-followups.md.
 */

import { readdir, mkdir, writeFile } from 'node:fs/promises';
import { join, basename, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';
import { spawn } from 'node:child_process';
import { convertSpec } from './static-converter.js';
import { matchPostProcess } from './post-process-matcher.js';
import { matchNativeFromJsSource, matchNativeGenerator } from './native-map.js';

const BUILD_DIR = join(
  import.meta.dirname,
  '..',
  'node_modules',
  '@withfig',
  'autocomplete',
  'build'
);

/**
 * Load a Fig spec by name, returning both the spec and any load error.
 * Never throws — a failed dynamic import becomes `{ spec: null, error }`.
 *
 * @param {string} specName - e.g., 'git', 'aws/s3'
 * @returns {{ spec: object|null, error: string|null }}
 */
async function loadFigSpecWithError(specName) {
  const specPath = join(BUILD_DIR, `${specName}.js`);
  try {
    const mod = await import(specPath);
    return { spec: mod.default ?? null, error: null };
  } catch (err) {
    return { spec: null, error: err && err.message ? err.message : String(err) };
  }
}

/**
 * Thin wrapper preserving the legacy null-on-failure contract used by the
 * `resolveLoadSpecs` loader injection point. Callers that need the error
 * detail should use `loadFigSpecWithError` directly.
 *
 * @param {string} specName - e.g., 'git', 'aws/s3'
 * @returns {object|null} The spec's default export, or null if not found
 */
async function loadFigSpec(specName) {
  const { spec } = await loadFigSpecWithError(specName);
  return spec;
}

/**
 * Resolve loadSpec references by inlining the referenced sub-spec.
 * Walks the converted spec tree and replaces _loadSpec markers with actual content.
 *
 * A cycle guard tracks the set of loadSpec targets already on the current
 * resolution path (threaded via `visited`). When a target is already visited,
 * a single `console.warn` is emitted with the would-be cycle chain and the
 * load is skipped. The `_loadSpec` marker is still stripped so the subcommand
 * remains in the cleaned output (carrying whatever static content it had).
 *
 * @param {object} spec - The converted spec (from static-converter)
 * @param {string} specName - The parent spec name (used in the cycle warning)
 * @param {Set<string>} visited - Targets already resolved on this path; callers
 *   should pass `new Set([specName])` at the top level so direct self-refs are
 *   caught. Must not be mutated by the caller across sibling invocations — a
 *   fresh branch is forked for each loadSpec descent.
 * @param {(name: string) => Promise<object|null>} loader - Injectable loader
 *   for tests. Defaults to `loadFigSpec` (dynamic import from BUILD_DIR).
 * @returns {object} The spec with loadSpec references resolved
 */
export async function resolveLoadSpecs(spec, specName, visited = new Set([specName]), loader = loadFigSpec) {
  if (!spec || typeof spec !== 'object') return spec;

  if (spec.subcommands && Array.isArray(spec.subcommands)) {
    for (let i = 0; i < spec.subcommands.length; i++) {
      const sub = spec.subcommands[i];
      if (sub._loadSpec) {
        const loadSpec = sub._loadSpec;
        delete sub._loadSpec;

        const targetName =
          typeof loadSpec === 'string'
            ? loadSpec
            : typeof loadSpec === 'object' && loadSpec && typeof loadSpec.specName === 'string'
              ? loadSpec.specName
              : null;

        if (typeof loadSpec === 'function') {
          // Dynamic function — can't resolve statically
          sub.requires_js = true;
        } else if (targetName !== null) {
          if (visited.has(targetName)) {
            // Cycle detected — emit a single warning and skip the load.
            // The subcommand keeps its existing content; we still recurse
            // into it below so inner loadSpecs further down still resolve.
            const cyclePath = [...visited, targetName].join(' → ');
            console.warn(
              `[converter] loadSpec cycle detected in "${specName}": "${cyclePath}" already resolved; skipping to avoid infinite recursion`
            );
          } else {
            const loaded = await loader(targetName);
            if (loaded) {
              const converted = convertSpec(loaded);
              // Merge the loaded spec into this subcommand
              if (converted.subcommands) sub.subcommands = converted.subcommands;
              if (converted.options) sub.options = converted.options;
              if (converted.args) sub.args = converted.args;

              // Descend into the freshly-inlined sub-spec with a forked
              // visited set so sibling loadSpecs to the same target don't
              // false-positive on each other.
              const nextVisited = new Set(visited);
              nextVisited.add(targetName);
              await resolveLoadSpecs(sub, specName, nextVisited, loader);
              continue;
            }
          }
        }
      }

      // Recurse into subcommands with the current visited set (no new frame
      // was pushed — this branch didn't cross a loadSpec boundary).
      await resolveLoadSpecs(sub, specName, visited, loader);
    }
  }

  return spec;
}

/**
 * Process all generators in a spec tree, applying the full conversion pipeline.
 * Mutates the spec in place.
 *
 * @param {object} spec - The converted spec
 * @param {string} specName - The spec name (for native generator matching)
 */
function processGenerators(spec, specName) {
  walkGenerators(spec, (generators) => {
    for (let i = 0; i < generators.length; i++) {
      generators[i] = processGenerator(generators[i], specName);
    }
  });
}

/**
 * Walk a spec tree and call the callback for each generators array found.
 */
function walkGenerators(obj, callback) {
  if (!obj || typeof obj !== 'object') return;

  if (obj.generators && Array.isArray(obj.generators)) {
    callback(obj.generators);
  }

  // Walk args
  if (obj.args) {
    if (Array.isArray(obj.args)) {
      for (const arg of obj.args) walkGenerators(arg, callback);
    } else {
      walkGenerators(obj.args, callback);
    }
  }

  // Walk subcommands
  if (obj.subcommands && Array.isArray(obj.subcommands)) {
    for (const sub of obj.subcommands) walkGenerators(sub, callback);
  }

  // Walk options
  if (obj.options && Array.isArray(obj.options)) {
    for (const opt of obj.options) {
      if (opt.args) {
        if (Array.isArray(opt.args)) {
          for (const arg of opt.args) walkGenerators(arg, callback);
        } else {
          walkGenerators(opt.args, callback);
        }
      }
    }
  }
}

/**
 * Process a single generator through the conversion pipeline.
 *
 * Priority order:
 * 1. Native generator map (git branch → git_branches)
 * 2. script + postProcess → pattern match → transforms
 * 3. script + splitOn → transforms
 * 4. script (function) → requires_js
 * 5. custom → requires_js
 * 6. Template-only → pass through
 *
 * @param {object} gen - Intermediate generator from static-converter
 * @param {string} specName - The spec name
 * @returns {object} Final Ghost Complete generator
 */
function processGenerator(gen, specName) {
  if (!gen || typeof gen !== 'object') return gen;

  // Case: custom async generator — try a native rewrite first, then
  // fall back to requires_js. This is the seam that catches the
  // upstream `make` generators (they're `custom: async (f, a) => ...`
  // bodies that shell out to `make -qp` plus a Makefile parse), so
  // there is no `script` array to key on at all. The strip-on-rewrite
  // contract holds: a successful native match returns the bare native
  // gen (plus optional cache), dropping `_custom`, `_customSource`,
  // `requires_js`, `js_source`, `script`, and `script_template` along
  // with every other internal marker.
  if (gen._custom) {
    const native = matchNativeFromJsSource(specName, gen._customSource);
    if (native) {
      const result = { ...native };
      if (gen.cache) result.cache = gen.cache;
      return result;
    }
    const result = { requires_js: true };
    if (gen._customSource) result.js_source = gen._customSource;
    return result;
  }

  // Case: script is a function — same native-first / requires_js
  // fallback as `_custom`. Currently no native maps fire here, but
  // the seam is symmetric so a future spec migration can reuse it
  // without re-plumbing.
  if (gen._scriptFunction) {
    const native = matchNativeFromJsSource(specName, gen._scriptSource);
    if (native) {
      const result = { ...native };
      if (gen.cache) result.cache = gen.cache;
      return result;
    }
    const result = { requires_js: true };
    if (gen._scriptSource) result.js_source = gen._scriptSource;
    return result;
  }

  // Case: has a script array — check native map first
  if (gen.script && Array.isArray(gen.script)) {
    const nativeGen = matchNativeGenerator(specName, gen.script, gen._postProcessSource);
    if (nativeGen) {
      // Native generator takes priority — emit native type plus optional cache
      const result = { ...nativeGen };
      if (gen.cache) result.cache = gen.cache;
      return result;
    }

    // Case: script + postProcess → pattern match
    if (gen._postProcessSource) {
      const match = matchPostProcess(gen._postProcessSource);
      const result = { script: gen.script };

      if (match.requires_js) {
        result.requires_js = true;
        if (match.js_source) result.js_source = match.js_source;
        // Propagate the _corrected_in marker set by the matcher for the
        // specific bug-class paths (substring/slice, JSON.parse
        // unresolvable-field). Passed through cleanGenerator via its
        // allowlist so it survives into the final JSON on disk.
        if (match._corrected_in) result._corrected_in = match._corrected_in;
      } else {
        result.transforms = match.transforms;
      }

      if (gen.cache) result.cache = gen.cache;
      return result;
    }

    // Case: script + splitOn → simple split transform
    if (gen._splitOn !== undefined) {
      const result = {
        script: gen.script,
        transforms: ['split_lines', 'filter_empty'],
      };
      if (gen.cache) result.cache = gen.cache;
      return result;
    }

    // Case: script with no postProcess or splitOn — just split by default
    const result = {
      script: gen.script,
      transforms: ['split_lines', 'filter_empty'],
    };
    if (gen.cache) result.cache = gen.cache;
    return result;
  }

  // Case: template-only generator
  if (gen.template) {
    return { template: gen.template };
  }

  // Unknown shape — pass through, stripping internal markers
  return cleanGenerator(gen);
}

/**
 * Generator fields that start with `_` but are intentionally preserved into
 * the final JSON on disk. These are persistent format extensions (see plan
 * §-1.4) — allowlisted rather than ad-hoc'd so the pattern scales as future
 * extensions are embraced. Keep this set intentionally small; do NOT add a
 * matching allowlist to `cleanSpec`, which must keep stripping underscore
 * keys on spec roots to prevent accidental leakage of future internal markers.
 */
const GENERATOR_FIELD_ALLOWLIST = new Set(['_corrected_in']);

/**
 * Remove internal markers (prefixed with _) from a generator, except those
 * on the format-extension allowlist. Exported for focused unit tests.
 */
export function cleanGenerator(gen) {
  const result = {};
  for (const [key, value] of Object.entries(gen)) {
    if (!key.startsWith('_') || GENERATOR_FIELD_ALLOWLIST.has(key)) {
      result[key] = value;
    }
  }
  return result;
}

/**
 * Clean the entire spec tree of internal markers.
 */
function cleanSpec(spec) {
  if (!spec || typeof spec !== 'object') return spec;

  const result = {};
  for (const [key, value] of Object.entries(spec)) {
    if (key.startsWith('_')) continue;

    if (key === 'subcommands' && Array.isArray(value)) {
      result.subcommands = value.map(cleanSpec).filter(Boolean);
    } else if (key === 'options' && Array.isArray(value)) {
      result.options = value.map(cleanSpec).filter(Boolean);
    } else if (key === 'args') {
      if (Array.isArray(value)) {
        result.args = value.map(cleanSpec).filter(Boolean);
      } else if (typeof value === 'object') {
        result.args = cleanSpec(value);
      } else {
        result.args = value;
      }
    } else if (key === 'generators' && Array.isArray(value)) {
      result.generators = value.map(g => cleanGenerator(g)).filter(Boolean);
    } else if (key === 'suggestions' && Array.isArray(value)) {
      result.suggestions = value;
    } else {
      result[key] = value;
    }
  }
  return result;
}

/**
 * Convert a single Fig spec to Ghost Complete JSON.
 *
 * Returns null on load failure to preserve the legacy contract used by the
 * test suite and by the `runConversionBatch` caller (which probes
 * `loadFigSpecWithError` separately to recover the underlying error detail).
 *
 * @param {string} specName - The spec name (filename without .js)
 * @returns {{ spec: object, stats: object } | null}
 */
export async function convertSingleSpec(specName) {
  const figSpec = await loadFigSpec(specName);
  if (!figSpec || typeof figSpec !== 'object') return null;

  // Step 1: Static conversion
  let spec = convertSpec(figSpec);

  // Step 2: Resolve loadSpec references
  spec = await resolveLoadSpecs(spec, specName);

  // Step 3: Process generators
  processGenerators(spec, specName);

  // Step 4: Clean internal markers
  spec = cleanSpec(spec);

  // Collect stats
  const stats = collectStats(spec);

  return { spec, stats };
}

/**
 * Collect statistics about a converted spec.
 */
function collectStats(spec) {
  const stats = {
    subcommands: 0,
    options: 0,
    generators: 0,
    nativeGenerators: 0,
    transformGenerators: 0,
    requiresJsGenerators: 0,
  };

  function walk(obj) {
    if (!obj || typeof obj !== 'object') return;

    if (obj.subcommands && Array.isArray(obj.subcommands)) {
      stats.subcommands += obj.subcommands.length;
      for (const sub of obj.subcommands) walk(sub);
    }

    if (obj.options && Array.isArray(obj.options)) {
      stats.options += obj.options.length;
      for (const opt of obj.options) {
        if (opt.args) {
          if (Array.isArray(opt.args)) {
            for (const a of opt.args) walk(a);
          } else {
            walk(opt.args);
          }
        }
      }
    }

    if (obj.args) {
      const args = Array.isArray(obj.args) ? obj.args : [obj.args];
      for (const arg of args) walk(arg);
    }

    if (obj.generators && Array.isArray(obj.generators)) {
      for (const gen of obj.generators) {
        stats.generators++;
        if (gen.type) stats.nativeGenerators++;
        else if (gen.transforms) stats.transformGenerators++;
        else if (gen.requires_js) stats.requiresJsGenerators++;
      }
    }
  }

  walk(spec);
  return stats;
}

/**
 * Get the list of all available spec names.
 */
export async function listSpecNames() {
  const entries = await readdir(BUILD_DIR);
  return entries
    .filter(f => f.endsWith('.js') && !f.startsWith('@') && !f.startsWith('.'))
    .map(f => f.replace(/\.js$/, ''));
}

/**
 * Empty totals shape shared by worker and orchestrator. Kept as a function
 * (not a constant) so each caller gets a fresh, independently-mutable object.
 */
function makeEmptyTotals() {
  return {
    converted: 0,
    failed: 0,
    subcommands: 0,
    options: 0,
    generators: 0,
    nativeGenerators: 0,
    transformGenerators: 0,
    requiresJsGenerators: 0,
  };
}

/**
 * Run the per-spec conversion loop over a list of spec names, writing each
 * resulting JSON to `outputDir` (unless `dryRun`). Per-spec failures are
 * recorded in `errors` and do not abort the batch. This is the shared worker
 * body — used both in-process (small --specs runs) and inside each
 * subprocess (--batch-worker mode).
 *
 * @param {object} opts
 * @param {string[]} opts.specNames
 * @param {string|null} opts.outputDir - Absolute path, or null for dry run.
 * @param {boolean} [opts.dryRun=false]
 * @returns {Promise<{totals: object, errors: Array<{spec: string, error: string}>}>}
 */
export async function runConversionBatch({ specNames, outputDir, dryRun = false }) {
  const totals = makeEmptyTotals();
  const errors = [];

  for (const specName of specNames) {
    try {
      // Probe the load path first so we can surface the underlying import
      // error to the caller. `convertSingleSpec` itself still returns null
      // on a missing default export (legacy contract the test suite relies
      // on), so we keep that as a distinct fallback.
      const { spec: figSpec, error: loadError } = await loadFigSpecWithError(specName);
      if (loadError) {
        errors.push({ spec: specName, error: `failed to load spec: ${loadError}` });
        totals.failed++;
        continue;
      }
      if (!figSpec || typeof figSpec !== 'object') {
        errors.push({
          spec: specName,
          error: 'spec module had no usable default export',
        });
        totals.failed++;
        continue;
      }

      const result = await convertSingleSpec(specName);
      if (!result) {
        // Defensive: load probe succeeded but convertSingleSpec still nulled.
        // Preserve the legacy message so behaviour is unchanged.
        errors.push({ spec: specName, error: 'Failed to load spec' });
        totals.failed++;
        continue;
      }

      const { spec, stats } = result;

      if (outputDir && !dryRun) {
        const outputPath = join(outputDir, `${specName}.json`);
        // Ensure subdirectories exist (for specs like aws/s3)
        const dir = join(outputDir, ...specName.split('/').slice(0, -1));
        if (dir !== outputDir) {
          await mkdir(dir, { recursive: true });
        }
        await writeFile(outputPath, JSON.stringify(spec, null, 2) + '\n');
      }

      totals.converted++;
      totals.subcommands += stats.subcommands;
      totals.options += stats.options;
      totals.generators += stats.generators;
      totals.nativeGenerators += stats.nativeGenerators;
      totals.transformGenerators += stats.transformGenerators;
      totals.requiresJsGenerators += stats.requiresJsGenerators;
    } catch (err) {
      errors.push({ spec: specName, error: err.message });
      totals.failed++;
    }
  }

  return { totals, errors };
}

/**
 * Merge a per-batch result into the running orchestrator aggregate. Mutates
 * `agg` in place.
 */
function mergeTotals(agg, batchTotals) {
  for (const key of Object.keys(agg)) {
    agg[key] += batchTotals[key] || 0;
  }
}

/**
 * Print the final conversion summary to stdout. This is the human/CI-readable
 * block and its format is considered stable — downstream docs reference it.
 */
function printSummary(totals, errors) {
  console.log(`\n--- Conversion Summary ---`);
  console.log(`Converted:          ${totals.converted}`);
  console.log(`Failed:             ${totals.failed}`);
  console.log(`Subcommands:        ${totals.subcommands}`);
  console.log(`Options:            ${totals.options}`);
  console.log(`Generators total:   ${totals.generators}`);
  console.log(`  Native (Rust):    ${totals.nativeGenerators}`);
  console.log(`  Transform:        ${totals.transformGenerators}`);
  console.log(`  Requires JS:      ${totals.requiresJsGenerators}`);

  if (errors.length > 0) {
    console.log(`\n--- Errors (${errors.length}) ---`);
    for (const { spec, error } of errors.slice(0, 20)) {
      console.log(`  ${spec}: ${error}`);
    }
    if (errors.length > 20) {
      console.log(`  ... and ${errors.length - 20} more`);
    }
  }
}

/**
 * Split an array into contiguous batches of up to `size` elements. Last
 * batch may be smaller. `size` is assumed ≥ 1 (caller validates).
 */
function chunk(arr, size) {
  const out = [];
  for (let i = 0; i < arr.length; i += size) {
    out.push(arr.slice(i, i + size));
  }
  return out;
}

/**
 * Spawn a worker subprocess for one batch and await its JSON-on-stdout
 * result. Progress (and any console.warn/error from the child) is inherited
 * to this process's stderr. Returns a best-effort result object even when
 * the child exits non-zero: in that failure case, the batch's specs are all
 * reported as failed, and up to 500 chars of stderr tail are forwarded to
 * this process's stderr for operator context.
 */
function runWorkerBatch({ batch, outputDir, dryRun, heapMb }) {
  return new Promise((resolvePromise) => {
    // Use `--flag=value` form for string args so values that happen to start
    // with `-` (e.g. the `-` spec from @withfig/autocomplete) or paths with
    // unusual characters are not misparsed as flags by the worker's
    // parseArgs. `--batch-worker` and `--dry-run` are booleans and pass
    // plain.
    const args = [
      fileURLToPath(import.meta.url),
      '--batch-worker',
      `--specs=${batch.join(',')}`,
    ];
    if (outputDir) {
      args.push(`--output=${outputDir}`);
    }
    if (dryRun) {
      args.push('--dry-run');
    }

    const child = spawn(process.execPath, args, {
      // stderr inherits so progress + warnings stream live. stdout is piped
      // so we can capture the final JSON line. stdin is closed.
      stdio: ['ignore', 'pipe', 'pipe'],
      env: {
        ...process.env,
        NODE_OPTIONS: `${process.env.NODE_OPTIONS ?? ''} --max-old-space-size=${heapMb}`.trim(),
      },
    });

    const STDOUT_BUF_CAP = 4096;
    let stdoutBuf = '';
    let stderrTail = '';
    const STDERR_TAIL_CAP = 500;

    child.stdout.on('data', (buf) => {
      stdoutBuf = (stdoutBuf + buf.toString('utf8')).slice(-STDOUT_BUF_CAP);
    });

    // Mirror stderr live so the user sees progress in real time, but also
    // keep a rolling tail for failure diagnostics.
    child.stderr.on('data', (buf) => {
      const s = buf.toString('utf8');
      process.stderr.write(s);
      stderrTail = (stderrTail + s).slice(-STDERR_TAIL_CAP);
    });

    child.on('error', (err) => {
      // Spawn failed outright (e.g., execPath missing). Treat the whole
      // batch as failed and move on.
      process.stderr.write(
        `[converter] failed to spawn worker: ${err.message}\n`
      );
      const totals = makeEmptyTotals();
      totals.failed = batch.length;
      resolvePromise({
        totals,
        errors: batch.map((spec) => ({
          spec,
          error: `worker spawn failed: ${err.message}`,
        })),
      });
    });

    child.on('close', (code) => {
      if (code !== 0) {
        process.stderr.write(
          `[converter] worker exited with code ${code}. Last stderr:\n${stderrTail}\n`
        );
        const totals = makeEmptyTotals();
        totals.failed = batch.length;
        resolvePromise({
          totals,
          errors: batch.map((spec) => ({
            spec,
            error: `worker exited ${code}`,
          })),
        });
        return;
      }

      // Parse the last non-empty line of stdout as the result JSON. The
      // worker emits exactly one line, but defensively take the last.
      const lines = stdoutBuf.split('\n').filter((l) => l.trim().length > 0);
      const last = lines[lines.length - 1];
      if (!last) {
        process.stderr.write(
          `[converter] worker produced no stdout; marking batch failed\n`
        );
        const totals = makeEmptyTotals();
        totals.failed = batch.length;
        resolvePromise({
          totals,
          errors: batch.map((spec) => ({ spec, error: 'worker stdout empty' })),
        });
        return;
      }

      try {
        const parsed = JSON.parse(last);
        resolvePromise({
          totals: { ...makeEmptyTotals(), ...(parsed.totals || {}) },
          errors: Array.isArray(parsed.errors) ? parsed.errors : [],
        });
      } catch (err) {
        process.stderr.write(
          `[converter] could not parse worker stdout as JSON: ${err.message}\n`
        );
        const totals = makeEmptyTotals();
        totals.failed = batch.length;
        resolvePromise({
          totals,
          errors: batch.map((spec) => ({
            spec,
            error: `worker stdout not JSON: ${err.message}`,
          })),
        });
      }
    });
  });
}

// --- CLI entry point ---

async function main() {
  const { values } = parseArgs({
    options: {
      output: { type: 'string', short: 'o' },
      specs: { type: 'string', short: 's' },
      'dry-run': { type: 'boolean' },
      'batch-size': { type: 'string' },
      'batch-worker': { type: 'boolean' },
    },
  });

  const outputDir = values.output ? resolve(values.output) : null;
  const isDryRun = values['dry-run'] || false;
  const isWorker = values['batch-worker'] || false;

  if (!outputDir && !isDryRun) {
    console.error('Usage: node src/index.js --output <dir> [--specs name1,name2] [--dry-run] [--batch-size N]');
    process.exit(1);
  }

  if (outputDir) {
    await mkdir(outputDir, { recursive: true });
  }

  // Determine which specs to process
  let specNames;
  if (values.specs) {
    specNames = values.specs.split(',').map((s) => s.trim()).filter(Boolean);
  } else {
    specNames = await listSpecNames();
  }

  // --- Worker mode: do one batch in-process and emit JSON on stdout ---
  if (isWorker) {
    console.error(`[worker] converting ${specNames.length} specs`);
    const { totals, errors } = await runConversionBatch({
      specNames,
      outputDir,
      dryRun: isDryRun,
    });
    // EXACTLY one line of JSON on stdout; nothing else.
    process.stdout.write(JSON.stringify({ totals, errors }) + '\n');
    return;
  }

  // --- Orchestrator mode ---
  const batchSizeRaw = values['batch-size'];
  const batchSize = batchSizeRaw !== undefined ? Number.parseInt(batchSizeRaw, 10) : 30;
  if (!Number.isFinite(batchSize) || batchSize < 1) {
    console.error(`Invalid --batch-size ${batchSizeRaw}; must be a positive integer`);
    process.exit(1);
  }

  const heapMb = Number.parseInt(process.env.CONVERT_WORKER_HEAP_MB ?? '2048', 10);
  if (!Number.isFinite(heapMb) || heapMb < 128) {
    console.error(`Invalid CONVERT_WORKER_HEAP_MB=${process.env.CONVERT_WORKER_HEAP_MB}; must be an integer ≥ 128`);
    process.exit(1);
  }

  console.log(`Converting ${specNames.length} specs...`);

  // Fast path: small explicit --specs runs stay in-process. Keeps
  // iteration snappy and avoids subprocess startup tax when the user is
  // actively debugging a handful of specs.
  if (specNames.length <= batchSize) {
    const { totals, errors } = await runConversionBatch({
      specNames,
      outputDir,
      dryRun: isDryRun,
    });
    printSummary(totals, errors);
    return;
  }

  // Batched path: spawn one Node subprocess per batch. Each child exits
  // after its batch, freeing its ESM module cache. Sequential on purpose
  // (parallelism is a separate plan — this fix is "don't OOM").
  const batches = chunk(specNames, batchSize);
  const aggregateTotals = makeEmptyTotals();
  const aggregateErrors = [];

  for (let i = 0; i < batches.length; i++) {
    const batch = batches[i];
    const first = batch[0];
    const last = batch[batch.length - 1];
    process.stderr.write(
      `[batch-${i + 1}/${batches.length}] converting ${batch.length} specs: ${first}..${last}\n`
    );
    const { totals, errors } = await runWorkerBatch({
      batch,
      outputDir,
      dryRun: isDryRun,
      heapMb,
    });
    mergeTotals(aggregateTotals, totals);
    for (const e of errors) aggregateErrors.push(e);
  }

  printSummary(aggregateTotals, aggregateErrors);
}

// Run main only when executed directly (not imported)
if (import.meta.url === `file://${process.argv[1]}`) {
  main().catch(err => {
    console.error('Fatal error:', err);
    process.exit(1);
  });
}
