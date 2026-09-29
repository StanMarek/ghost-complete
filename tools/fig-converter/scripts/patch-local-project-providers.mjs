#!/usr/bin/env node
/**
 * In-place rewrite for local-project providers.
 *
 * The full-spec regen (`npm run convert`) drops hand-curated `priority`
 * fields from `specs/{make,npm,cargo}.json` because the upstream Fig
 * source no longer carries them. We can't accept that loss right now —
 * those priorities back the ranking-and-suppression contract tested in
 * `golden_specs.rs`.
 *
 * This script does a minimal surgical pass instead: walk the existing
 * spec files, find generators that match a known requires_js pattern
 * for one of our new local-project providers, and replace each with
 * the native `{ type: "..." }` form plus optional `cache`. Other spec
 * data outside the replaced generator — including hand-curated
 * priorities elsewhere in the file — survives untouched. Run once
 * after the converter + provider code lands; the same path will handle
 * future local-project-provider additions.
 *
 * Usage: node scripts/patch-local-project-providers.mjs [spec ...]
 */

import { readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(__dirname, '..', '..', '..');

/** The generator's JS body: `js_runtime.source` in current specs, the
 *  legacy `js_source` field in specs from older converter releases. */
function jsSource(g) {
  if (typeof g.js_runtime?.source === 'string') return g.js_runtime.source;
  if (typeof g.js_source === 'string') return g.js_source;
  return null;
}

/** Native replacement carrying the original `cache`, with keys in the
 *  converter's sorted order so the rewrite serializes like a regen. */
function nativeReplacement(type, g) {
  const out = {};
  if (g.cache) out.cache = g.cache;
  out.type = type;
  return out;
}

function isCargoWorkspaceMembersGenerator(g) {
  if (!Array.isArray(g.script)) return false;
  if (g.script[0] !== 'cargo' || g.script[1] !== 'metadata') return false;
  if (!g.script.includes('--no-deps')) return false;
  const source = jsSource(g);
  if (source === null) return false;
  if (/\.dependencies\b/.test(source)) return false;
  return /JSON\.parse[\s\S]*\.packages[\s\S]*\.map\s*\(/.test(source);
}

/** npm's `bash -c` package.json walker + `scripts` extractor, which
 *  pnpm/yarn/bun/nr/rushx/meteor reuse verbatim. The `.scripts` check
 *  keeps their `.dependencies` extractors (same walker) on the JS path. */
function packageJsonScripts(g) {
  if (!Array.isArray(g.script)) return null;
  if (g.script[0] !== 'bash' || g.script[1] !== '-c') return null;
  const body = typeof g.script[2] === 'string' ? g.script[2] : '';
  if (!/package\.json/.test(body)) return null;
  const source = jsSource(g);
  if (source === null) return null;
  if (!/JSON\.parse[\s\S]*\.scripts/.test(source)) return null;
  return nativeReplacement('npm_scripts', g);
}

/** Recognizers — each takes a generator object, returns the replacement
 *  generator if it matches, otherwise null. Order matters only when two
 *  recognizers might both fire (none today). */
export const RECOGNIZERS = {
  make: [
    (g) => {
      if (!g.requires_js) return null;
      const source = jsSource(g);
      if (source === null) return null;
      if (!/make\s+-qp/.test(source)) return null;
      return nativeReplacement('makefile_targets', g);
    },
  ],
  npm: [packageJsonScripts],
  pnpm: [packageJsonScripts],
  yarn: [packageJsonScripts],
  bun: [packageJsonScripts],
  nr: [packageJsonScripts],
  rushx: [packageJsonScripts],
  meteor: [packageJsonScripts],
  cargo: [
    (g) => {
      if (!isCargoWorkspaceMembersGenerator(g)) return null;
      return nativeReplacement('cargo_workspace_members', g);
    },
  ],
};

export function rewriteGenerators(node, recognizers, stats) {
  if (!node || typeof node !== 'object') return;
  if (Array.isArray(node)) {
    for (const child of node) rewriteGenerators(child, recognizers, stats);
    return;
  }
  if (Array.isArray(node.generators)) {
    node.generators = node.generators.map((gen) => {
      for (const recognize of recognizers) {
        const replacement = recognize(gen);
        if (replacement) {
          stats.rewrites++;
          return replacement;
        }
      }
      return gen;
    });
  }
  for (const value of Object.values(node)) rewriteGenerators(value, recognizers, stats);
}

export async function patchSpec(specName) {
  const path = resolve(REPO_ROOT, 'specs', `${specName}.json`);
  const original = await readFile(path, 'utf8');
  const spec = JSON.parse(original);
  const stats = { rewrites: 0 };
  rewriteGenerators(spec, RECOGNIZERS[specName], stats);
  if (stats.rewrites === 0) {
    console.log(`${specName}: no matching generators (already patched?)`);
    return;
  }
  // Match the converter's output style: 2-space indent, trailing newline.
  await writeFile(path, JSON.stringify(spec, null, 2) + '\n', 'utf8');
  console.log(`${specName}: rewrote ${stats.rewrites} generators`);
}

// CLI entry-point: only run when invoked directly (not when imported by tests).
// Patches the specs named on the command line, or every spec with a
// recognizer when none are named. Each spec is patched in its own
// try/catch so a failure in one doesn't leave the disk in a partially-
// rewritten state with no diagnostic for the others. We still exit
// non-zero if any patch failed.
if (import.meta.url === `file://${process.argv[1]}`) {
  const requested = process.argv.slice(2);
  const unknown = requested.filter((spec) => !RECOGNIZERS[spec]);
  if (unknown.length > 0) {
    console.error(`no recognizers for: ${unknown.join(', ')}`);
    process.exit(1);
  }
  const specs = requested.length > 0 ? requested : Object.keys(RECOGNIZERS);
  let failures = 0;
  for (const spec of specs) {
    try {
      await patchSpec(spec);
    } catch (err) {
      failures++;
      console.error(`${spec}: patch failed — ${err.message}`);
    }
  }
  if (failures > 0) {
    process.exitCode = 1;
  }
}
