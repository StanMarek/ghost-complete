#!/usr/bin/env node
// Phase 1 classification spike driver.
//
// Walks specs/*.json, extracts every generator with requires_js: true AND
// js_source: string, runs analyzeGenerator() on each, buckets by
// shape.fingerprint, assigns a verdict from the closed enum, and emits:
//
//   tools/fig-converter/docs/shape-inventory.json
//   tools/fig-converter/docs/shape-inventory.md
//   tools/fig-converter/docs/candidate-providers.json
//
// Idempotent: re-runs overwrite outputs cleanly.
//
// ---------------------------------------------------------------------------
// SLUG ALGORITHM
// ---------------------------------------------------------------------------
// 1. Normalise the fingerprint to lowercase.
// 2. Extract all method-call names with a regex: /\.([a-zA-Z_$][a-zA-Z0-9_$]*)\(/g
//    — this captures e.g. ["split","filter","map"] from ".split(STR).filter(FN).map(FN)".
//    If no method-calls found, fall back to the first 40 chars of the
//    fingerprint, stripped of non-alphanumeric chars and joined with hyphens.
// 3. Deduplicate the extracted names while preserving order.
// 4. Join with "-" to form a candidate slug.
// 5. Collision-avoidance: if the slug is already registered by a different
//    fingerprint, append "-2", "-3", etc. until the slug is unique.
// Special-case: fingerprint "" → slug "empty"; "<parse_error>" → "parse-error".
// ---------------------------------------------------------------------------

import { readdir, readFile, writeFile, mkdir } from 'node:fs/promises';
import { join, resolve, dirname, basename } from 'node:path';
import { fileURLToPath } from 'node:url';
import { analyzeGenerator } from '../src/ast-analyzer.js';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);

const SPECS_DIR = resolve(__dirname, '..', '..', '..', 'specs');
const DOCS_DIR = resolve(__dirname, '..', 'docs');

// ---------------------------------------------------------------------------
// Corpus walk
// ---------------------------------------------------------------------------

function* walkGenerators(obj, path) {
  if (Array.isArray(obj)) {
    for (let i = 0; i < obj.length; i++) {
      yield* walkGenerators(obj[i], `${path}[${i}]`);
    }
  } else if (obj !== null && typeof obj === 'object') {
    for (const [k, v] of Object.entries(obj)) {
      if (k === 'generators' && Array.isArray(v)) {
        // Single-pass: yield qualifying generators AND recurse into each
        // entry, without re-visiting. No real spec has a `generators` array
        // nested inside a `generators` entry today, but the old two-pass
        // structure would have double-yielded such a case. This pattern is
        // semantically identical on today's corpus (1889 still) and safe
        // against future nesting.
        for (let i = 0; i < v.length; i++) {
          const g = v[i];
          if (g && typeof g === 'object') {
            if (g.requires_js === true && typeof g.js_source === 'string') {
              yield { path: `${path}/generators[${i}]`, gen: g };
            }
            yield* walkGenerators(g, `${path}/generators[${i}]`);
          }
        }
      } else {
        yield* walkGenerators(v, `${path}/${k}`);
      }
    }
  }
}

// ---------------------------------------------------------------------------
// Slug derivation
// ---------------------------------------------------------------------------

function deriveSlugCandidate(fingerprint) {
  if (fingerprint === '') return 'empty';
  if (fingerprint === '<parse_error>') return 'parse-error';

  const methodRe = /\.([a-zA-Z_$][a-zA-Z0-9_$]*)\(/g;
  const names = [];
  const seen = new Set();
  let m;
  while ((m = methodRe.exec(fingerprint)) !== null) {
    if (!seen.has(m[1])) {
      names.push(m[1]);
      seen.add(m[1]);
    }
  }

  if (names.length > 0) {
    return names.join('-').toLowerCase();
  }

  const fallback = fingerprint
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 50);
  return fallback || 'unknown';
}

// Assigns unique shape_id slugs to composite-keyed buckets. Returns a
// Map<bucketKey, slug> where bucketKey is "<fingerprint>|<hasFigApiRefs>".
// Slug rules:
//   - Base slug derived from fingerprint.
//   - If bucket has fig api refs, append "-with-fig-refs" suffix (BEFORE
//     any numeric collision suffix).
//   - Collision suffix (-2, -3, ...) applies per variant independently.
function assignSlugs(buckets) {
  const entries = [...buckets.entries()].sort((a, b) => b[1].count - a[1].count);
  const claimedSlugs = new Set();
  const result = new Map();

  for (const [key, bucket] of entries) {
    const base = deriveSlugCandidate(bucket.fingerprint);
    let candidate = bucket.hasFigApiRefs ? `${base}-with-fig-refs` : base;
    if (!claimedSlugs.has(candidate)) {
      claimedSlugs.add(candidate);
      result.set(key, candidate);
    } else {
      let n = 2;
      let slug;
      do {
        slug = `${candidate}-${n}`;
        n++;
      } while (claimedSlugs.has(slug));
      claimedSlugs.add(slug);
      result.set(key, slug);
    }
  }

  return result;
}

// ---------------------------------------------------------------------------
// Verdict assignment
// ---------------------------------------------------------------------------

/**
 * Compose the composite bucket key used by both the bucketing pass and
 * the per-item → bucket lookup for candidate-providers. Extracted so both
 * call sites share one key construction and can never drift.
 *
 * Parse-error items collapse into a single bucket regardless of shape
 * flags (their shape is zero-filled because analysis short-circuits).
 */
function composeBucketKey(fingerprintKey, shape, hasFigApiRefs) {
  if (fingerprintKey === '<parse_error>') return '<parse_error>|false';
  return [
    fingerprintKey,
    hasFigApiRefs,
    shape.has_json_parse,
    shape.has_regex_match,
    shape.has_substring_or_slice,
    shape.has_conditional,
    shape.has_await,
  ].join('|');
}

// Revised verdict priority (post-review):
//   1. parse_error non-null → hand_audit_required
//   2. has_fig_api_refs: true (ANY ref, any kind) → hand_audit_required
//   3. has_await: true → requires_runtime
//   4. has_json_parse: true:
//        - fingerprint contains 2+ .PROP hops after JSON.parse → needs_dotted_path_json_extract
//        - else → existing_transforms (single-field json_extract is already supported)
//   5. All shape booleans false → existing_transforms (simple line-oriented archetype)
//   6. has_regex_match: true → needs_new_transform_regex_match
//   7. has_substring_or_slice: true → needs_new_transform_substring_slice
//   8. has_conditional: true → needs_new_transform_conditional_split
//   9. Fallback → needs_new_transform_misc
function assignVerdict(shape, hasFigApiRefs, hasParseError) {
  if (hasParseError) return 'hand_audit_required';
  if (hasFigApiRefs) return 'hand_audit_required';
  if (shape.has_await) return 'requires_runtime';

  if (shape.has_json_parse) {
    const fp = shape.fingerprint;
    const hasDottedPath = fp.includes('JSON.parse(...)') &&
      (() => {
        const afterParse = fp.indexOf('JSON.parse(...)');
        const tail = fp.slice(afterParse);
        const propMatches = (tail.match(/\.PROP/g) || []).length;
        return propMatches >= 2;
      })();
    if (hasDottedPath) return 'needs_dotted_path_json_extract';
    // Single-field json_extract is already in the existing transform pipeline.
    return 'existing_transforms';
  }

  if (
    !shape.has_regex_match &&
    !shape.has_substring_or_slice &&
    !shape.has_conditional
  ) {
    return 'existing_transforms';
  }

  if (shape.has_regex_match) return 'needs_new_transform_regex_match';
  if (shape.has_substring_or_slice) return 'needs_new_transform_substring_slice';
  if (shape.has_conditional) return 'needs_new_transform_conditional_split';

  return 'needs_new_transform_misc';
}

// ---------------------------------------------------------------------------
// Qualification heuristics for candidate-providers
// ---------------------------------------------------------------------------

/**
 * @param {string} command
 * @param {Array<{gen: object, verdict: string}>} members  per-generator verdict
 *        looked up from the bucket-level verdict map — NOT recomputed here.
 *        This keeps `assignVerdict` the sole oracle for verdicts.
 */
function qualifyCommand(command, members) {
  const scriptCommands = [];
  const seenScripts = new Set();
  for (const { gen } of members) {
    if (gen.script) {
      const s = typeof gen.script === 'string' ? gen.script :
        Array.isArray(gen.script) ? gen.script.join(' ') : String(gen.script);
      if (!seenScripts.has(s)) {
        seenScripts.add(s);
        scriptCommands.push(s);
      }
    }
  }

  const allScripts = scriptCommands.join(' ').toLowerCase();
  const commandLower = command.toLowerCase();

  const authKeywords = ['oauth', 'login', 'token', 'saml', 'auth', 'credential', 'secret', 'api-key', 'apikey'];
  const noAuth = !authKeywords.some(kw => commandLower.includes(kw) || allScripts.includes(kw));

  const paginationKeywords = ['--page', '--limit', '--offset', '--cursor', 'pagination', 'paginate'];
  const noPagination = !paginationKeywords.some(kw => allScripts.includes(kw));

  const hasPipeline = scriptCommands.some(s => /[|;&]/.test(s));
  const singleSubprocessNoPipeline = !hasPipeline;

  const fsParsingCommands = new Set([
    'cat', 'jq', 'awk', 'sed', 'grep', 'find', 'ls', 'head', 'tail',
    'cargo', 'npm', 'yarn', 'pnpm', 'pip', 'pip3', 'go',
  ]);
  const noFileSystemParsing = !fsParsingCommands.has(commandLower);

  const totalGens = members.length;
  const existingTransformCount = members.filter(m => m.verdict === 'existing_transforms').length;
  const stableLineOutput = existingTransformCount > 0 &&
    (existingTransformCount / totalGens) >= 0.5;

  const unboundedCommands = new Set(['aws', 'gcloud', 'find']);
  const outputBounded = !unboundedCommands.has(commandLower);

  const rateLimitedCommands = new Set([
    'gh', 'github', 'gitlab', 'heroku', 'netlify', 'vercel', 'fly',
    'stripe', 'twilio', 'okta', 'salesforce',
  ]);
  const noRateLimits = !rateLimitedCommands.has(commandLower) &&
    !authKeywords.some(kw => commandLower.includes(kw));

  const noNewTransitiveDeps = true;

  return {
    scriptCommands,
    qualification: {
      single_subprocess_no_pipeline: singleSubprocessNoPipeline,
      no_auth: noAuth,
      no_pagination: noPagination,
      stable_line_output: stableLineOutput,
      no_file_system_parsing: noFileSystemParsing,
      output_bounded: outputBounded,
      no_rate_limits: noRateLimits,
      no_new_transitive_deps: noNewTransitiveDeps,
    },
  };
}

function buildRationale(command, qual, qualifies) {
  if (qualifies) {
    return `Command '${command}' meets all 8 criteria — subprocess-based, no auth, no pagination, stable line output.`;
  }
  const failed = Object.entries(qual)
    .filter(([, v]) => !v)
    .map(([k]) => k.replace(/_/g, ' '));
  return `Disqualified: ${failed.join(', ')}.`;
}

// ---------------------------------------------------------------------------
// Stable JSON stringify
// ---------------------------------------------------------------------------

function stableStringify(obj, indent = 2) {
  return JSON.stringify(obj, null, indent);
}

// ---------------------------------------------------------------------------
// Markdown generation
// ---------------------------------------------------------------------------

function generateMarkdown(inventory) {
  const { total_generators, shapes } = inventory;
  const verdictCounts = new Map();
  for (const shape of shapes) {
    verdictCounts.set(shape.verdict, (verdictCounts.get(shape.verdict) || 0) + shape.count);
  }

  const lines = [];
  lines.push('# Shape Inventory — Phase 1 Classification Spike');
  lines.push('');
  lines.push('> Canonical machine-readable data: [shape-inventory.json](./shape-inventory.json)');
  lines.push('');
  lines.push(`**Total generators analysed:** ${total_generators}`);
  lines.push(`**Total distinct shapes:** ${shapes.length}`);
  lines.push('');

  lines.push('## Verdict Distribution');
  lines.push('');
  lines.push('| Verdict | Count | % |');
  lines.push('|---------|-------|---|');
  const sortedVerdicts = [...verdictCounts.entries()].sort((a, b) => b[1] - a[1]);
  for (const [verdict, count] of sortedVerdicts) {
    const pct = ((count / total_generators) * 100).toFixed(1);
    lines.push(`| \`${verdict}\` | ${count} | ${pct}% |`);
  }
  lines.push('');

  lines.push('## Shapes Table');
  lines.push('');
  lines.push('| shape_id | count | fingerprint (≤80 chars) | verdict | has_fig_api_refs | example_spec |');
  lines.push('|----------|-------|-------------------------|---------|------------------|--------------|');
  const sortedShapes = [...shapes].sort((a, b) => b.count - a.count);
  for (const shape of sortedShapes) {
    const fp = shape.fingerprint.length > 80
      ? shape.fingerprint.slice(0, 77) + '...'
      : shape.fingerprint;
    const fpEscaped = fp.replace(/\|/g, '\\|');
    lines.push(
      `| \`${shape.shape_id}\` | ${shape.count} | \`${fpEscaped}\` | \`${shape.verdict}\` | ${shape.has_fig_api_refs} | ${shape.example_spec} |`
    );
  }
  lines.push('');

  lines.push('## Per-Verdict Breakdown (Top 5 Shapes Each)');
  lines.push('');
  const verdictGroups = new Map();
  for (const shape of shapes) {
    if (!verdictGroups.has(shape.verdict)) verdictGroups.set(shape.verdict, []);
    verdictGroups.get(shape.verdict).push(shape);
  }
  for (const [verdict, vShapes] of [...verdictGroups.entries()].sort((a, b) => {
    const ca = verdictCounts.get(a[0]) || 0;
    const cb = verdictCounts.get(b[0]) || 0;
    return cb - ca;
  })) {
    const total = verdictCounts.get(verdict) || 0;
    lines.push(`### \`${verdict}\` (${total} generators, ${vShapes.length} shapes)`);
    lines.push('');
    const top5 = [...vShapes].sort((a, b) => b.count - a.count).slice(0, 5);
    for (const shape of top5) {
      const fp = shape.fingerprint.length > 80
        ? shape.fingerprint.slice(0, 77) + '...'
        : shape.fingerprint;
      lines.push(`- **\`${shape.shape_id}\`** (${shape.count}): \`${fp.replace(/\|/g, '\\|')}\``);
    }
    lines.push('');
  }

  return lines.join('\n');
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

async function main() {
  await mkdir(DOCS_DIR, { recursive: true });

  console.log(`Walking specs in ${SPECS_DIR}...`);
  const files = (await readdir(SPECS_DIR)).filter(f => f.endsWith('.json')).sort();

  const allGenerators = [];

  for (const file of files) {
    const specBasename = basename(file, '.json');
    const spec = JSON.parse(await readFile(join(SPECS_DIR, file), 'utf8'));

    for (const { path, gen } of walkGenerators(spec, '')) {
      const id = `${specBasename}:${path}`;
      allGenerators.push({ id, specBasename, basename: file, js_source: gen.js_source, gen });
    }
  }

  console.log(`Found ${allGenerators.length} requires_js generators with js_source.`);

  console.log('Analyzing generators...');
  // Per-item analysis stops at the shape + refs flags. Verdicts are
  // computed ONCE per bucket below — never per item — so `assignVerdict`
  // has exactly one call site.
  const analyzed = allGenerators.map(({ id, specBasename, js_source, gen }) => {
    const result = analyzeGenerator(js_source);
    const hasFigApiRefs = result.fig_api_refs.length > 0;
    const hasParseError = result.parse_error !== null;
    const fingerprintKey = hasParseError ? '<parse_error>' : result.shape.fingerprint;
    const bucketKey = composeBucketKey(fingerprintKey, result.shape, hasFigApiRefs);

    return {
      id,
      specBasename,
      js_source,
      gen,
      shape: result.shape,
      fig_api_refs: result.fig_api_refs,
      parse_error: result.parse_error,
      hasFigApiRefs,
      hasParseError,
      fingerprintKey,
      bucketKey,
    };
  });

  // Bucket by COMPOSITE key (fingerprint, hasFigApiRefs, full shape flags).
  //
  // Why all the shape flags are in the key: the fingerprint only captures
  // the top-level return expression's call chain. Two generators with the
  // same fingerprint may have different shape booleans when (e.g.) one
  // wraps the return in an async function with `await` in the setup code.
  // That means their individual verdicts differ — one is `requires_runtime`,
  // the other is `existing_transforms`. Bucketing by fingerprint alone
  // would force them into the same bucket and pick one verdict for both,
  // mis-classifying the minority members.
  //
  // Including the flags in the key guarantees every bucket is homogeneous:
  // every member has the same shape booleans → every member gets the same
  // verdict → the bucket-level verdict is valid for ALL members.
  //
  // `has_fig_api_refs` is also in the key (per review issue 2) so that
  // minified-bundle free-ref false-positives don't poison clean siblings.
  //
  // Parse-error items collapse into a single bucket regardless of flags
  // (their shape struct is zero-filled because analysis short-circuits).
  const buckets = new Map();
  for (const item of analyzed) {
    const key = item.bucketKey;
    if (!buckets.has(key)) {
      buckets.set(key, {
        key,
        fingerprint: item.fingerprintKey,
        hasFigApiRefs: item.hasFigApiRefs,
        shape: item.shape,
        count: 0,
        generators: [],
        exampleSpec: null,
      });
    }
    const bucket = buckets.get(key);
    bucket.count++;
    bucket.generators.push(item);
    if (!bucket.exampleSpec) bucket.exampleSpec = item.specBasename + '.json';
  }

  const slugMap = assignSlugs(buckets);

  // Compute verdicts ONCE per bucket. This Map is the sole source of truth
  // for verdict lookups downstream (both the inventory output and the
  // candidate-providers qualification flow read from it).
  const verdictByBucketKey = new Map();
  for (const [key, bucket] of buckets) {
    const bucketHasParseError = bucket.fingerprint === '<parse_error>';
    verdictByBucketKey.set(
      key,
      assignVerdict(bucket.shape, bucket.hasFigApiRefs, bucketHasParseError),
    );
  }

  const shapes = [...buckets.entries()]
    .sort((a, b) => b[1].count - a[1].count)
    .map(([key, bucket]) => ({
      shape_id: slugMap.get(key),
      fingerprint: bucket.fingerprint,
      count: bucket.count,
      verdict: verdictByBucketKey.get(key),
      has_fig_api_refs: bucket.hasFigApiRefs,
      example_spec: bucket.exampleSpec,
      generator_ids: bucket.generators.map(g => g.id).sort(),
    }));

  const totalInShapes = shapes.reduce((s, sh) => s + sh.count, 0);
  if (totalInShapes !== allGenerators.length) {
    throw new Error(`Shape count mismatch: ${totalInShapes} vs ${allGenerators.length}`);
  }

  const inventory = {
    schema_version: '1.0',
    total_generators: allGenerators.length,
    shapes,
  };

  // Candidate providers — group generators by command (spec basename).
  // Per-item verdict comes from the bucket verdict map; qualifyCommand
  // never calls assignVerdict itself.
  const commandMap = new Map();
  for (const item of analyzed) {
    if (!commandMap.has(item.specBasename)) {
      commandMap.set(item.specBasename, []);
    }
    commandMap.get(item.specBasename).push({
      gen: item.gen,
      hasFigApiRefs: item.hasFigApiRefs,
      verdict: verdictByBucketKey.get(item.bucketKey),
    });
  }

  const candidates = [];
  for (const [command, members] of commandMap.entries()) {
    const { scriptCommands, qualification } = qualifyCommand(command, members);
    const qualifies = Object.values(qualification).every(Boolean);
    const figApiRefCount = members.filter(m => m.hasFigApiRefs).length;

    candidates.push({
      command,
      total_requires_js_generators: members.length,
      fig_api_ref_generators: figApiRefCount,
      script_commands_observed: scriptCommands,
      qualification,
      qualifies,
      rationale: buildRationale(command, qualification, qualifies),
    });
  }

  candidates.sort((a, b) => {
    if (a.qualifies !== b.qualifies) return a.qualifies ? -1 : 1;
    return b.total_requires_js_generators - a.total_requires_js_generators;
  });

  // No `generated_at` timestamp: keeps the file byte-stable across reruns.
  const candidateProviders = {
    schema_version: '1.0',
    candidates,
  };

  // Write outputs
  const inventoryPath = join(DOCS_DIR, 'shape-inventory.json');
  const mdPath = join(DOCS_DIR, 'shape-inventory.md');
  const candidatePath = join(DOCS_DIR, 'candidate-providers.json');

  await writeFile(inventoryPath, stableStringify(inventory) + '\n', 'utf8');
  console.log(`Wrote ${inventoryPath}`);

  await writeFile(mdPath, generateMarkdown(inventory) + '\n', 'utf8');
  console.log(`Wrote ${mdPath}`);

  await writeFile(candidatePath, stableStringify(candidateProviders) + '\n', 'utf8');
  console.log(`Wrote ${candidatePath}`);

  // Summary
  console.log('\n=== SUMMARY ===');
  console.log(`Total generators: ${allGenerators.length}`);
  console.log(`Total shapes: ${shapes.length}`);
  const verdictCounts = new Map();
  for (const sh of shapes) {
    verdictCounts.set(sh.verdict, (verdictCounts.get(sh.verdict) || 0) + sh.count);
  }
  console.log('\nVerdict distribution:');
  for (const [v, c] of [...verdictCounts.entries()].sort((a, b) => b[1] - a[1])) {
    const pct = ((c / allGenerators.length) * 100).toFixed(1);
    console.log(`  ${v}: ${c} (${pct}%)`);
  }
  const figApiTotal = analyzed.filter(i => i.hasFigApiRefs).length;
  console.log(`\nFig API ref generators: ${figApiTotal} (${((figApiTotal / allGenerators.length) * 100).toFixed(1)}%)`);
  console.log(`\nCandidate commands total: ${candidates.length}`);
  console.log(`Qualifying commands: ${candidates.filter(c => c.qualifies).length}`);
  console.log('\nTop 5 shapes by count:');
  for (const sh of shapes.slice(0, 5)) {
    const fp = sh.fingerprint.slice(0, 60);
    console.log(`  [${sh.count}] ${sh.shape_id}: ${fp}`);
  }
}

main().catch(err => {
  console.error('Fatal error:', err);
  process.exit(1);
});
