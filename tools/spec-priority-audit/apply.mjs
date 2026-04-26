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
 *   node tools/spec-priority-audit/apply.mjs           # writes specs/*.json
 *   node tools/spec-priority-audit/apply.mjs --dry-run # report only, no writes
 *
 * Categorisation is by spec filename (e.g. `git.json` → vcs family). Each
 * family carries a per-subcommand and per-flag map of {name -> priority}.
 * Recursion descends into nested subcommands so e.g. `git remote add` picks
 * up the same `add: 90` bump as a top-level `add`.
 */

import { readFile, writeFile, readdir } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { parseArgs } from 'node:util';

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = join(__dirname, '..', '..');
const SPECS_DIR = join(REPO_ROOT, 'specs');
const HEURISTICS_PATH = join(__dirname, 'heuristics.json');

// Subcommand kind base = 70; option kind base = 30. Writing a value equal
// to the kind base is a no-op for ordering — the ranker can't tell them
// apart from suggestions that omit `priority`. Skip those writes so the
// emitted JSON stays minimal.
const SUBCOMMAND_KIND_BASE = 70;
const FLAG_KIND_BASE = 30;

async function loadHeuristics() {
  const raw = await readFile(HEURISTICS_PATH, 'utf8');
  const parsed = JSON.parse(raw);
  const families = parsed.families ?? {};

  // Build a fast lookup: specName -> { subcommands, flags }.
  const specToRules = new Map();
  for (const [familyName, family] of Object.entries(families)) {
    if (familyName.startsWith('_')) continue;
    const rules = {
      subcommands: family.subcommands ?? {},
      flags: family.flags ?? {},
    };
    for (const specName of family.specs ?? []) {
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
    if (typeof n === 'string' && Object.hasOwn(rules.flags, n)) {
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

async function processSpec(filePath, rules, dryRun) {
  const stats = { subcommandsBumped: 0, flagsBumped: 0 };
  const raw = await readFile(filePath, 'utf8');
  const spec = JSON.parse(raw);

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
    await writeFile(filePath, output, 'utf8');
  }
  return stats;
}

async function main() {
  const { values } = parseArgs({ options: { 'dry-run': { type: 'boolean' } } });
  const dryRun = values['dry-run'] === true;

  const specToRules = await loadHeuristics();
  const entries = await readdir(SPECS_DIR, { withFileTypes: true });
  const totals = { specsTouched: 0, subcommandsBumped: 0, flagsBumped: 0, specsConsidered: 0 };

  for (const entry of entries) {
    if (!entry.isFile() || !entry.name.endsWith('.json')) continue;
    const specName = entry.name.slice(0, -5); // strip .json
    const rules = specToRules.get(specName);
    if (!rules) continue;
    totals.specsConsidered += 1;
    const stats = await processSpec(join(SPECS_DIR, entry.name), rules, dryRun);
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
