#!/usr/bin/env node
// Classify specs whose generators carry `_corrected_in` by correction reason,
// for the CHANGELOG under Phase -1.4. Mirrors the doctor walk in
// `count_corrected_generators_in_spec`.
//
//   substring/slice: js_source matches /\.(?:substring|slice)\s*\(/
//   json-parse:      contains `JSON.parse` AND does NOT match the above
//
// A spec may appear in both buckets. Unknown js_source shapes exit non-zero.
import { readdir, readFile } from 'node:fs/promises';
import { join, dirname, resolve, basename } from 'node:path';
import { fileURLToPath } from 'node:url';

const SPECS_DIR = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..', '..', 'specs');
const SUB = /\.(?:substring|slice)\s*\(/;
const classify = (s) =>
  typeof s !== 'string' ? 'unknown'
  : SUB.test(s) ? 'substring-slice'
  : s.includes('JSON.parse') ? 'json-parse' : 'unknown';

function collect(spec) {
  const out = [];
  const walk = (args) => {
    for (const a of Array.isArray(args) ? args : args ? [args] : []) {
      for (const g of a?.generators ?? []) if (g?._corrected_in) out.push(g);
    }
  };
  const visit = (n) => { walk(n.args); for (const o of n.options ?? []) walk(o.args); };
  visit(spec);
  const stack = [...(spec.subcommands ?? [])];
  while (stack.length) { const s = stack.pop(); visit(s); stack.push(...(s.subcommands ?? [])); }
  return out;
}

const files = (await readdir(SPECS_DIR)).filter(f => f.endsWith('.json')).sort();
const buckets = { 'substring-slice': new Set(), 'json-parse': new Set() };
let total = 0;
for (const file of files) {
  for (const gen of collect(JSON.parse(await readFile(join(SPECS_DIR, file), 'utf8')))) {
    total++;
    const reason = classify(gen.js_source);
    if (reason === 'unknown') {
      console.error(`UNEXPECTED: ${file} has unrecognised js_source:\n${JSON.stringify(gen, null, 2)}`);
      process.exit(2);
    }
    buckets[reason].add(basename(file, '.json'));
  }
}

const print = (label, set) => {
  const list = [...set].sort();
  console.log(`${label} (${list.length}):`);
  for (const s of list) console.log(`  ${s}`);
};
print('Substring/slice correction', buckets['substring-slice']);
console.log();
print('JSON.parse fallback correction', buckets['json-parse']);
console.log(`\nTotal corrected generators: ${total}`);
