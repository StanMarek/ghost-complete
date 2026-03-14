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
 */

import { readdir, mkdir, writeFile } from 'node:fs/promises';
import { join, basename, resolve } from 'node:path';
import { parseArgs } from 'node:util';
import { convertSpec } from './static-converter.js';
import { matchPostProcess } from './post-process-matcher.js';
import { matchNativeGenerator } from './native-map.js';

const BUILD_DIR = join(
  import.meta.dirname,
  '..',
  'node_modules',
  '@withfig',
  'autocomplete',
  'build'
);

/**
 * Load a Fig spec by name.
 * @param {string} specName - e.g., 'git', 'aws/s3'
 * @returns {object|null} The spec's default export, or null if not found
 */
async function loadFigSpec(specName) {
  const specPath = join(BUILD_DIR, `${specName}.js`);
  try {
    const mod = await import(specPath);
    return mod.default;
  } catch {
    return null;
  }
}

/**
 * Resolve loadSpec references by inlining the referenced sub-spec.
 * Walks the converted spec tree and replaces _loadSpec markers with actual content.
 *
 * @param {object} spec - The converted spec (from static-converter)
 * @param {string} specName - The parent spec name (for resolving relative paths)
 * @returns {object} The spec with loadSpec references resolved
 */
async function resolveLoadSpecs(spec, specName) {
  if (!spec || typeof spec !== 'object') return spec;

  if (spec.subcommands && Array.isArray(spec.subcommands)) {
    for (let i = 0; i < spec.subcommands.length; i++) {
      const sub = spec.subcommands[i];
      if (sub._loadSpec) {
        const loadSpec = sub._loadSpec;
        delete sub._loadSpec;

        if (typeof loadSpec === 'string') {
          // Simple string reference — load and inline
          const loaded = await loadFigSpec(loadSpec);
          if (loaded) {
            const converted = convertSpec(loaded);
            // Merge the loaded spec into this subcommand
            if (converted.subcommands) sub.subcommands = converted.subcommands;
            if (converted.options) sub.options = converted.options;
            if (converted.args) sub.args = converted.args;
          }
        } else if (typeof loadSpec === 'object' && loadSpec.specName) {
          // Object with specName property
          const loaded = await loadFigSpec(loadSpec.specName);
          if (loaded) {
            const converted = convertSpec(loaded);
            if (converted.subcommands) sub.subcommands = converted.subcommands;
            if (converted.options) sub.options = converted.options;
            if (converted.args) sub.args = converted.args;
          }
        } else if (typeof loadSpec === 'function') {
          // Dynamic function — can't resolve statically
          sub.requires_js = true;
        }
      }

      // Recurse into subcommands
      await resolveLoadSpecs(sub, specName);
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

  // Case: custom async generator → requires_js
  if (gen._custom) {
    const result = { requires_js: true };
    if (gen._customSource) result.js_source = gen._customSource;
    return result;
  }

  // Case: script is a function → requires_js
  if (gen._scriptFunction) {
    const result = { requires_js: true };
    if (gen._scriptSource) result.js_source = gen._scriptSource;
    return result;
  }

  // Case: has a script array — check native map first
  if (gen.script && Array.isArray(gen.script)) {
    const nativeGen = matchNativeGenerator(specName, gen.script);
    if (nativeGen) {
      // Native generator takes priority — emit type-only generator
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
 * Remove internal markers (prefixed with _) from a generator.
 */
function cleanGenerator(gen) {
  const result = {};
  for (const [key, value] of Object.entries(gen)) {
    if (!key.startsWith('_')) {
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

// --- CLI entry point ---

async function main() {
  const { values } = parseArgs({
    options: {
      output: { type: 'string', short: 'o' },
      specs: { type: 'string', short: 's' },
      'dry-run': { type: 'boolean' },
    },
  });

  const outputDir = values.output ? resolve(values.output) : null;
  const isDryRun = values['dry-run'] || false;

  if (!outputDir && !isDryRun) {
    console.error('Usage: node src/index.js --output <dir> [--specs name1,name2] [--dry-run]');
    process.exit(1);
  }

  // Create output directory
  if (outputDir) {
    await mkdir(outputDir, { recursive: true });
  }

  // Determine which specs to convert
  let specNames;
  if (values.specs) {
    specNames = values.specs.split(',').map(s => s.trim());
  } else {
    specNames = await listSpecNames();
  }

  console.log(`Converting ${specNames.length} specs...`);

  const totals = {
    converted: 0,
    failed: 0,
    subcommands: 0,
    options: 0,
    generators: 0,
    nativeGenerators: 0,
    transformGenerators: 0,
    requiresJsGenerators: 0,
  };

  const errors = [];

  for (const specName of specNames) {
    try {
      const result = await convertSingleSpec(specName);
      if (!result) {
        errors.push({ spec: specName, error: 'Failed to load spec' });
        totals.failed++;
        continue;
      }

      const { spec, stats } = result;

      // Write to file
      if (outputDir) {
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

  // Print summary
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

// Run main only when executed directly (not imported)
if (import.meta.url === `file://${process.argv[1]}`) {
  main().catch(err => {
    console.error('Fatal error:', err);
    process.exit(1);
  });
}
