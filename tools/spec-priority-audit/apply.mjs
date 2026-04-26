#!/usr/bin/env node
/**
 * apply.mjs
 *
 * One-shot audit script that walks every spec under `specs/` and assigns
 * `priority` overrides to subcommands and options whose names match a
 * curated allowlist in `heuristics.json`. Existing priority values are
 * NEVER overwritten — converter output and manual review take precedence
 * over the heuristic. The script is idempotent: a second run is a no-op.
 *
 * Usage:
 *   node tools/spec-priority-audit/apply.mjs           # writes specs/*.json that gained a new priority value
 *   node tools/spec-priority-audit/apply.mjs --dry-run # report only, no writes
 *
 * Categorisation is by spec filename (e.g. `git.json` → vcs family). Each
 * family carries a per-subcommand and per-flag map of {name -> priority}.
 * Recursion descends into nested subcommands so e.g. `git remote add` picks
 * up the same `add: 90` bump as a top-level `add`.
 */

import { readFile, writeFile, readdir, rename, unlink } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { parseArgs } from 'node:util';

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = join(__dirname, '..', '..');
const SPECS_DIR = join(REPO_ROOT, 'specs');
const HEURISTICS_PATH = join(__dirname, 'heuristics.json');

// Subcommand kind base = 70; flag kind base = 30. Writing a value equal
// to the kind base is a no-op for ordering — the ranker can't tell them
// apart from suggestions that omit `priority`. Skip those writes so the
// emitted JSON stays minimal.
const SUBCOMMAND_KIND_BASE = 70;
const FLAG_KIND_BASE = 30;

function isPlainObject(v) {
  return v !== null && typeof v === 'object' && !Array.isArray(v);
}

function validatePriority(value, family, kind, key) {
  if (!Number.isInteger(value) || value < 0 || value > 100) {
    throw new Error(
      `heuristics.json: family "${family}" ${kind} "${key}" has invalid priority ${JSON.stringify(value)} (expected integer in [0, 100])`
    );
  }
}

function validateHeuristics(parsed) {
  if (!isPlainObject(parsed)) {
    throw new Error('heuristics.json: top-level value must be an object');
  }
  if (!isPlainObject(parsed.families)) {
    throw new Error('heuristics.json: "families" must be an object');
  }
  for (const [familyName, family] of Object.entries(parsed.families)) {
    if (familyName.startsWith('_')) continue;
    if (!isPlainObject(family)) {
      throw new Error(`heuristics.json: family "${familyName}" must be an object`);
    }
    if (!Array.isArray(family.specs)) {
      throw new Error(`heuristics.json: family "${familyName}" specs must be an array`);
    }
    for (const s of family.specs) {
      if (typeof s !== 'string') {
        throw new Error(`heuristics.json: family "${familyName}" specs must contain only strings`);
      }
    }
    for (const scope of ['subcommands', 'flags']) {
      const map = family[scope];
      if (map === undefined) continue;
      if (!isPlainObject(map)) {
        throw new Error(`heuristics.json: family "${familyName}" ${scope} must be an object`);
      }
      for (const [key, value] of Object.entries(map)) {
        validatePriority(value, familyName, scope, key);
      }
    }
  }
}

async function loadHeuristics() {
  const raw = await readFile(HEURISTICS_PATH, 'utf8');
  const parsed = JSON.parse(raw);
  validateHeuristics(parsed);
  const families = parsed.families;

  // Build a fast lookup: specName -> { subcommands, flags, family }.
  const specToRules = new Map();
  for (const [familyName, family] of Object.entries(families)) {
    if (familyName.startsWith('_')) continue;
    const rules = {
      subcommands: family.subcommands ?? {},
      flags: family.flags ?? {},
      family: familyName,
    };
    for (const specName of family.specs) {
      const existing = specToRules.get(specName);
      if (existing) {
        throw new Error(
          `heuristics.json: spec "${specName}" listed in multiple families: "${existing.family}" and "${familyName}"`
        );
      }
      specToRules.set(specName, rules);
    }
  }
  return specToRules;
}

function shouldSetSubcommand(currentPriority, newPriority) {
  if (currentPriority !== undefined) return false;        // never overwrite
  if (newPriority === SUBCOMMAND_KIND_BASE) return false; // pointless write
  return true;
}

function shouldSetFlag(currentPriority, newPriority) {
  if (currentPriority !== undefined) return false;
  if (newPriority === FLAG_KIND_BASE) return false;
  return true;
}

function applySubcommandRules(subcommand, rules, stats) {
  if (!subcommand || typeof subcommand !== 'object') return;
  const name = typeof subcommand.name === 'string' ? subcommand.name : null;
  if (name && Object.hasOwn(rules.subcommands, name)) {
    const newPriority = rules.subcommands[name];
    if (shouldSetSubcommand(subcommand.priority, newPriority)) {
      subcommand.priority = newPriority;
      stats.subcommandsBumped += 1;
    }
  }
  for (const opt of subcommand.options ?? []) applyOptionRules(opt, rules, stats);
  for (const sub of subcommand.subcommands ?? []) applySubcommandRules(sub, rules, stats);
}

function applyOptionRules(option, rules, stats) {
  if (!option || typeof option !== 'object') return;
  const names = Array.isArray(option.name) ? option.name : [option.name];
  let bestPriority;
  for (const n of names) {
    if (typeof n !== 'string') {
      console.warn(`schema drift: non-string option name encountered: ${JSON.stringify(n)}`);
      continue;
    }
    if (Object.hasOwn(rules.flags, n)) {
      const candidate = rules.flags[n];
      if (bestPriority === undefined || candidate < bestPriority) {
        bestPriority = candidate;
      }
    }
  }
  if (bestPriority !== undefined && shouldSetFlag(option.priority, bestPriority)) {
    option.priority = bestPriority;
    stats.flagsBumped += 1;
  }
}

async function processSpec(filePath, specName, rules, dryRun) {
  const stats = { subcommandsBumped: 0, flagsBumped: 0 };
  let spec;
  try {
    const raw = await readFile(filePath, 'utf8');
    spec = JSON.parse(raw);
  } catch (err) {
    throw new Error(`${specName}: ${err.message}`, { cause: err });
  }

  if (rules.subcommands && Object.hasOwn(rules.subcommands, spec.name)) {
    const newPriority = rules.subcommands[spec.name];
    if (shouldSetSubcommand(spec.priority, newPriority)) {
      spec.priority = newPriority;
      stats.subcommandsBumped += 1;
    }
  }
  for (const opt of spec.options ?? []) applyOptionRules(opt, rules, stats);
  for (const sub of spec.subcommands ?? []) applySubcommandRules(sub, rules, stats);

  if (!dryRun && (stats.subcommandsBumped > 0 || stats.flagsBumped > 0)) {
    const output = JSON.stringify(spec, null, 2) + '\n';
    const tmp = `${filePath}.tmp`;
    try {
      await writeFile(tmp, output, 'utf8');
      await rename(tmp, filePath);
    } catch (err) {
      try {
        await unlink(tmp);
      } catch (cleanupErr) {
        if (cleanupErr.code !== 'ENOENT') throw cleanupErr;
      }
      throw new Error(`${specName}: ${err.message}`, { cause: err });
    }
  }
  return stats;
}

async function main() {
  const { values } = parseArgs({ options: { 'dry-run': { type: 'boolean' } } });
  const dryRun = values['dry-run'] === true;

  const specToRules = await loadHeuristics();
  const entries = await readdir(SPECS_DIR, { withFileTypes: true });

  // Detect unknown spec names BEFORE any write: any heuristic spec with no
  // matching file on disk is a bug in heuristics.json. Aborting first means
  // the run is all-or-nothing — no partial-success-masquerading-as-failure.
  const basenamesOnDisk = new Set();
  for (const entry of entries) {
    if (!entry.isFile() || !entry.name.endsWith('.json')) continue;
    basenamesOnDisk.add(entry.name.slice(0, -5));
  }
  const unmatched = [];
  for (const heuristicName of specToRules.keys()) {
    if (!basenamesOnDisk.has(heuristicName)) unmatched.push(heuristicName);
  }
  if (unmatched.length > 0) {
    console.error(`Unknown spec names in heuristics.json (no matching specs/<name>.json):`);
    for (const name of unmatched) console.error(`  - ${name}`);
    process.exit(1);
  }

  const totals = { specsTouched: 0, subcommandsBumped: 0, flagsBumped: 0, specsConsidered: 0 };

  for (const entry of entries) {
    if (!entry.isFile() || !entry.name.endsWith('.json')) continue;
    const specName = entry.name.slice(0, -5); // strip .json
    const rules = specToRules.get(specName);
    if (!rules) continue;
    totals.specsConsidered += 1;
    const stats = await processSpec(join(SPECS_DIR, entry.name), entry.name, rules, dryRun);
    if (stats.subcommandsBumped > 0 || stats.flagsBumped > 0) {
      totals.specsTouched += 1;
      totals.subcommandsBumped += stats.subcommandsBumped;
      totals.flagsBumped += stats.flagsBumped;
      console.log(
        `${entry.name.padEnd(28)}  +${stats.subcommandsBumped} subcommand, +${stats.flagsBumped} flag`
      );
    }
  }

  console.log('\n--- Audit Summary ---');
  console.log(`Specs in heuristic families: ${totals.specsConsidered}`);
  console.log(`Specs modified:              ${totals.specsTouched}`);
  console.log(`Subcommand priorities set:   ${totals.subcommandsBumped}`);
  console.log(`Flag priorities set:         ${totals.flagsBumped}`);
  if (dryRun) console.log('(dry-run: no files written)');
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
