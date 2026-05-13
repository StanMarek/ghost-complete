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

import { readdir, readFile, mkdir, rm, writeFile } from 'node:fs/promises';
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { join, basename, resolve, relative, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';
import { spawn } from 'node:child_process';
import { convertSpec } from './static-converter.js';
import { matchPostProcess, unrecognizedSingleLetterHelper } from './post-process-matcher.js';
import { matchNativeFromJsSource, matchNativeGenerator } from './native-map.js';
import { mapAwsSdkGenerator } from './aws-mapper.js';
import { analyzeGenerator } from './ast-analyzer.js';
import { analyzeTokenOnly } from './token-only-analyzer.js';
import { extractSubprocessGenerator } from './subprocess-extractor.js';
import { stringifySorted } from './serialize.js';

/**
 * Names that gc-jsrt installs as global helpers in every job's sandbox
 * (see crates/gc-jsrt/src/helpers.js). When the AST analyzer flags a
 * body's free identifiers and ALL of them are in this set, we preserve
 * the source — the runtime guarantees the names will resolve.
 *
 * Shape-validated at module load so a corrupted or schema-changed
 * known-helpers.json fails loudly here instead of silently degrading
 * every helper-bearing body to `js_runtime: undefined` downstream.
 */
const KNOWN_HELPERS = (() => {
  const parsed = JSON.parse(
    readFileSync(new URL('./known-helpers.json', import.meta.url), 'utf8'),
  );
  if (!Array.isArray(parsed.helpers) || parsed.helpers.length === 0) {
    throw new Error(
      'known-helpers.json missing or empty `helpers` array (expected a non-empty string list)',
    );
  }
  // Per-element validation: a partially corrupted list (e.g.
  // `["l", 42, "f"]`) must NOT silently land non-strings in the Set —
  // `KNOWN_HELPERS.has('l')` would still work but `.has(42)` would
  // miss any minified body that references that helper, degrading only
  // some bodies. The regex is the schema the helper-preservation test
  // pins in helper-preservation.test.js (`/^[a-z]$/`).
  for (const name of parsed.helpers) {
    if (typeof name !== 'string' || !/^[a-z]$/.test(name)) {
      throw new Error(
        `known-helpers.json: invalid helper name ${JSON.stringify(name)} ` +
          `— expected single-lowercase-letter string`,
      );
    }
  }
  return new Set(parsed.helpers);
})();

const BUILD_DIR = join(
  import.meta.dirname,
  '..',
  'node_modules',
  '@withfig',
  'autocomplete',
  'build'
);
const UX11_DECLINE_REPORT = join(
  import.meta.dirname,
  '..',
  'reports',
  'ux-11-decline-reasons.json',
);

const REPORTABLE_SUBPROCESS_DECLINES = new Set([
  'tokens-derived-command',
  'tokens-derived-args',
  'tokens-derived-postprocess',
  'multiple-awaits',
  'filesystem-read',
  'fetch-reference',
  'network-bound-command',
  'git-subprocess-not-native',
  'nonliteral-command',
  'nonliteral-args',
  // A parse failure in the Fig source is rare but signals upstream
  // shape drift worth investigating. Pass it through the audit report
  // so a future bump of @withfig/autocomplete can't silently move a
  // batch of generators off the static-extract path.
  'parse-error',
]);

let activeDeclineCollector = null;

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

function unresolvedLoadSpecMessage(targetName, specName, visited) {
  const loadPath = [...visited, targetName].join(' -> ');
  return `unresolved loadSpec target "${targetName}" while resolving "${specName}" (${loadPath})`;
}

const KNOWN_MISSING_LOAD_SPECS = new Set([
  'gcloud\0gcloud/alpha',
  'gcloud\0gcloud/beta',
]);

function isKnownMissingLoadSpec(specName, targetName) {
  return KNOWN_MISSING_LOAD_SPECS.has(`${specName}\0${targetName}`);
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
            let loaded;
            try {
              loaded = await loader(targetName);
            } catch (err) {
              const message = unresolvedLoadSpecMessage(targetName, specName, visited);
              const detail = err && err.message ? err.message : String(err);
              throw new Error(`${message}: ${detail}`, { cause: err });
            }

            if (!loaded || typeof loaded !== 'object') {
              if (isKnownMissingLoadSpec(specName, targetName)) {
                console.warn(
                  `[converter] known missing loadSpec target "${targetName}" while resolving "${specName}"; keeping static subcommand`
                );
                await resolveLoadSpecs(sub, specName, visited, loader);
                continue;
              }
              throw new Error(unresolvedLoadSpecMessage(targetName, specName, visited));
            }

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
      const processed = processGenerator(generators[i], specName);
      const awsSdkGen = processed?.type === 'aws_sdk'
        ? null
        : mapAwsSdkGenerator(specName, processed?.script);
      if (awsSdkGen) {
        const result = { ...awsSdkGen };
        if (processed.script) result.script = processed.script;
        if (processed.transforms) result.transforms = processed.transforms;
        if (processed._lowered_from_requires_js) {
          result._lowered_from_requires_js = processed._lowered_from_requires_js;
        }
        if (processed.cache) result.cache = processed.cache;
        generators[i] = result;
      } else {
        generators[i] = processed;
      }
    }
  });
}

function applySpecFixups(spec, specName) {
  if (!spec || typeof spec !== 'object') return spec;

  if (specName === 'cd' && spec.name === 'cd') {
    spec.args = {
      name: 'directory',
      template: 'folders',
    };
  }

  if (specName === 'cargo' && spec.name === 'cargo') {
    applyCargoPriorityFixups(spec);
    applyCargoNativeProviderFixups(spec);
  }

  if (specName === 'aws' && spec.name === 'aws') {
    applyAwsCliFixups(spec);
  }

  if (specName === 'brew' && spec.name === 'brew') {
    applyBrewNativeProviderFixups(spec);
  }

  if (specName === 'npm' && spec.name === 'npm') {
    applyNpmNativeProviderFixups(spec);
  }

  if (specName === 'nrm' && spec.name === 'nrm') {
    applyNpmNativeProviderFixups(spec);
  }

  if (specName === 'tmux' && spec.name === 'tmux') {
    applyTmuxNativeProviderFixups(spec);
  }

  if (specName === 'systemctl' && spec.name === 'systemctl') {
    applySystemdNativeProviderFixups(spec);
  }

  if (specName === 'chown' && spec.name === 'chown') {
    applyChownNativeProviderFixup(spec);
  }

  return spec;
}

function namesInclude(name, needle) {
  const names = Array.isArray(name) ? name : [name];
  return names.includes(needle);
}

function argList(value) {
  if (!value) return [];
  return Array.isArray(value) ? value : [value];
}

function walkSpecNodes(node, callback) {
  if (!node || typeof node !== 'object') return;
  callback(node);

  for (const arg of argList(node.args)) walkSpecNodes(arg, callback);

  for (const opt of node.options ?? []) {
    callback(opt);
    for (const arg of argList(opt.args)) walkSpecNodes(arg, callback);
  }

  for (const sub of node.subcommands ?? []) walkSpecNodes(sub, callback);
}

function firstGenerator(owner) {
  return owner?.generators?.[0] ?? null;
}

function firstArgGenerator(owner) {
  const arg = owner?.args;
  if (Array.isArray(arg)) return null;
  return firstGenerator(arg);
}

function generatorSource(gen) {
  if (!gen || typeof gen !== 'object') return '';
  return gen.js_runtime?.source ?? gen.js_source ?? '';
}

function setArgProvider(owner, provider) {
  const arg = owner?.args;
  if (!arg || Array.isArray(arg) || typeof arg !== 'object') return false;
  arg.generators = [provider];
  return true;
}

function setNodeProvider(owner, provider) {
  if (!owner || typeof owner !== 'object') return false;
  owner.generators = [provider];
  return true;
}

function provider(type, params = null) {
  return params ? { type, params } : { type };
}

function applyCargoNativeProviderFixups(spec) {
  const targetKinds = new Map([
    ['--bin', 'bin'],
    ['--example', 'example'],
    ['--test', 'test'],
    ['--bench', 'bench'],
  ]);

  walkSpecNodes(spec, (node) => {
    if (!node.options) return;
    for (const opt of node.options) {
      for (const [flag, kind] of targetKinds) {
        if (!namesInclude(opt.name, flag)) continue;
        const gen = firstArgGenerator(opt);
        if (!isCargoMetadataGenerator(gen)) continue;
        setArgProvider(opt, provider('cargo_targets', { kind }));
      }

      if (!namesInclude(opt.name, '--features')) continue;
      const gen = firstArgGenerator(opt);
      if (!gen?.script || gen.script[0] !== 'cargo' || gen.script[1] !== 'read-manifest') {
        continue;
      }
      setArgProvider(opt, provider('cargo_features'));
    }
  });
}

/// `cargo metadata` shows up in two shapes after the converter pipeline:
/// the legacy "preserved JS body" with the original async wrapper still
/// in `js_runtime.source` (matched by the textual cargo/metadata/targets
/// regex), and the post-ux-11 static-extracted shape where the script is
/// already lifted into `gen.script` and the post-stmt body is minified
/// (the `targets` token is renamed away). Detecting either is enough to
/// route the option to the native cargo_targets provider.
function isCargoMetadataGenerator(gen) {
  if (!gen) return false;
  if (
    Array.isArray(gen.script) &&
    gen.script[0] === 'cargo' &&
    gen.script[1] === 'metadata'
  ) {
    return true;
  }
  return /cargo[\s\S]*metadata[\s\S]*targets/.test(generatorSource(gen));
}

function npmDependencyProviderFromSource(source) {
  if (typeof source !== 'string' || !source.includes('package.json')) return null;
  if (!source.includes('dependencies') && !source.includes('devDependencies')) return null;
  if (source.includes('Object.assign') || source.includes('optionalDependencies')) {
    return provider('npm_all_dependencies');
  }
  if (source.includes('devDependencies')) return provider('npm_dev_dependencies');
  if (source.includes('dependencies')) return provider('npm_dependencies');
  return null;
}

function applyNpmNativeProviderFixups(spec) {
  walkSpecNodes(spec, (node) => {
    const gen = firstGenerator(node);
    const native = npmDependencyProviderFromSource(generatorSource(gen));
    if (native) setNodeProvider(node, native);
  });
}

function applyBrewNativeProviderFixups(spec) {
  walkSpecNodes(spec, (node) => {
    const gen = firstGenerator(node);
    if (!gen?.script || gen.script[0] !== 'brew' || gen.script[1] !== 'formulae') {
      return;
    }
    setNodeProvider(node, provider('brew_formulae_searchable'));
  });
}

function applyTmuxNativeProviderFixups(spec) {
  const map = new Map([
    ['ls', 'tmux_sessions'],
    ['list-sessions', 'tmux_sessions'],
    ['lsw', 'tmux_windows'],
    ['list-windows', 'tmux_windows'],
    ['lsp', 'tmux_panes'],
    ['list-panes', 'tmux_panes'],
    ['lsc', 'tmux_clients'],
    ['list-clients', 'tmux_clients'],
  ]);

  walkSpecNodes(spec, (node) => {
    const gen = firstGenerator(node);
    if (!gen?.script || gen.script[0] !== 'tmux') return;
    const type = map.get(gen.script[1]);
    if (type) setNodeProvider(node, provider(type));
  });
}

function applySystemdNativeProviderFixups(spec) {
  const activeUnitCommands = new Set([
    'stop',
    'reload',
    'restart',
    'try-restart',
    'reload-or-restart',
    'try-reload-or-restart',
    'kill',
    'clean',
    'freeze',
    'thaw',
  ]);

  for (const sub of spec.subcommands ?? []) {
    const gen = firstArgGenerator(sub);
    const source = generatorSource(gen);
    if (!source.includes('systemctl') || !source.includes('list-units')) continue;
    if (source.includes('list-unit-files')) continue;
    const type = activeUnitCommands.has(sub.name) ? 'systemd_active_units' : 'systemd_units';
    setArgProvider(sub, provider(type));
  }
}

function applyChownNativeProviderFixup(spec) {
  const ownerArg = argList(spec.args)[0];
  const gen = firstGenerator(ownerArg);
  const source = generatorSource(gen);
  if (!source.includes('dscl') || !source.includes('/Users')) return;
  ownerArg.generators = [provider('dscl_users'), provider('dscl_groups')];
}

function applyCargoPriorityFixups(spec) {
  const subcommandPriorities = new Map([
    ['bench', 65],
    ['build', 92],
    ['clean', 55],
    ['doc', 55],
    ['init', 55],
    ['install', 75],
    ['publish', 75],
    ['run', 92],
    ['test', 90],
    ['uninstall', 78],
    ['update', 78],
    ['add', 90],
    ['remove', 80],
  ]);
  for (const sub of spec.subcommands ?? []) {
    if (typeof sub?.name === 'string' && subcommandPriorities.has(sub.name)) {
      sub.priority = subcommandPriorities.get(sub.name);
    }
  }

  const optionPriority = (name) => {
    const names = Array.isArray(name) ? name : [name];
    if (names.includes('-q') || names.includes('--quiet')) return 25;
    if (names.includes('-v') || names.includes('--verbose')) return 25;
    if (names.includes('--dry-run')) return 50;
    if (names.includes('-f') && names.includes('--force')) return 20;
    if (names.includes('-f') && names.includes('--format')) return 20;
    return null;
  };

  const walk = (node) => {
    if (!node || typeof node !== 'object') return;
    for (const opt of node.options ?? []) {
      const priority = optionPriority(opt.name);
      if (priority !== null) opt.priority = priority;
      walk(opt);
    }
    for (const sub of node.subcommands ?? []) walk(sub);
    if (Array.isArray(node.args)) {
      for (const arg of node.args) walk(arg);
    } else {
      walk(node.args);
    }
  };
  walk(spec);
}

function ensureOption(options, option) {
  if (!Array.isArray(options)) return;
  const names = Array.isArray(option.name) ? option.name : [option.name];
  const exists = options.some((opt) => names.some((name) => namesInclude(opt?.name, name)));
  if (!exists) options.push(option);
}

function ensureSubcommand(subcommands, subcommand, beforeName = null) {
  if (!Array.isArray(subcommands)) return;
  if (subcommands.some((sub) => sub?.name === subcommand.name)) return;
  if (beforeName) {
    const index = subcommands.findIndex((sub) => sub?.name === beforeName);
    if (index !== -1) {
      subcommands.splice(index, 0, subcommand);
      return;
    }
  }
  subcommands.push(subcommand);
}

function applyAwsCliFixups(spec) {
  if (!Array.isArray(spec.options)) spec.options = [];
  ensureOption(spec.options, {
    args: {
      name: 'region',
    },
    description: 'The region to use. Overrides config/env settings.',
    name: ['--region'],
  });

  const sso = (spec.subcommands ?? []).find((sub) => sub?.name === 'sso');
  if (sso) {
    if (!Array.isArray(sso.subcommands)) sso.subcommands = [];
    ensureSubcommand(
      sso.subcommands,
      {
        description:
          'Retrieves and caches an AWS IAM Identity Center access token to exchange for AWS credentials.',
        name: 'login',
        options: [
          {
            description:
              'Disables automatically opening the verification URL in the default browser.',
            name: ['--no-browser'],
          },
          {
            args: {
              name: 'string',
            },
            description: 'An explicit SSO session to use to login.',
            name: ['--sso-session'],
          },
        ],
      },
      'logout',
    );
  }

  const profile = (spec.options ?? []).find((opt) => {
    const names = Array.isArray(opt?.name) ? opt.name : [opt?.name];
    return names.includes('--profile');
  });
  const gen = profile?.args?.generators?.[0];
  if (!gen?.cache?.cache_by_directory) return;
  if (!Number.isFinite(gen.cache.ttl_seconds) || gen.cache.ttl_seconds <= 0) {
    gen.cache.ttl_seconds = 300;
  }
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

function buildSelfContainedJsRuntime(kind, source) {
  if (!source || typeof source !== 'string') return null;
  const analysis = analyzeGenerator(source);
  if (analysis.parse_error) return null;
  // Free identifiers normally disqualify a body — they would throw
  // ReferenceError in the sandbox. The runtime installs pure-JS
  // definitions for Fig's minified helpers in every job (see
  // crates/gc-jsrt/src/helpers.js), so free refs whose names are
  // entirely covered by that preamble are safe to preserve.
  const freeRefs = analysis.fig_api_refs.filter((ref) => ref.kind === 'free');
  if (freeRefs.some((ref) => !KNOWN_HELPERS.has(ref.name))) return null;
  return { kind, source, self_contained: true };
}

function buildTokenOnlyJsRuntime(source) {
  const analysis = analyzeTokenOnly(source);
  if (!analysis.token_only) return null;
  return { kind: 'token_only', source, self_contained: false };
}

function subprocessExtractionResult(source, gen, specName) {
  const extracted = extractSubprocessGenerator(source);
  if (extracted.kind === 'native') {
    const result = {
      script: extracted.script,
      transforms: extracted.transforms,
      _static_extracted_subprocess: true,
    };
    if (gen.cache) result.cache = gen.cache;
    return result;
  }
  if (extracted.kind === 'fallback_post_process') {
    const result = {
      script: extracted.script,
      requires_js: true,
      js_runtime: {
        kind: 'post_process',
        source: extracted.fallbackJsSource,
        self_contained: true,
      },
    };
    if (gen.cache) result.cache = gen.cache;
    return result;
  }
  if (
    activeDeclineCollector &&
    extracted.kind === 'declined' &&
    REPORTABLE_SUBPROCESS_DECLINES.has(extracted.reason)
  ) {
    activeDeclineCollector.push({ spec: specName, reason: extracted.reason });
  }
  return null;
}

/**
 * Process a single generator through the conversion pipeline.
 *
 * Priority order:
 * 1. Native generator map (git branch → git_branches)
 * 2. script + postProcess → pattern match → transforms
 * 3. script + splitOn → transforms
 * 4. script (function) → js_runtime.kind = "script_function"
 * 5. custom → js_runtime.kind = "custom"
 * 6. Template-only → pass through
 *
 * Exported so the converter test suite can pin the native-first /
 * requires_js / js_runtime emission seams without spinning up the upstream
 * loader for a full spec.
 *
 * @param {object} gen - Intermediate generator from static-converter
 * @param {string} specName - The spec name
 * @returns {object} Final Ghost Complete generator
 */
export function processGenerator(gen, specName) {
  if (!gen || typeof gen !== 'object') return gen;

  // Case: custom async generator — try a native rewrite first, then
  // fall back to requires_js. This is the seam that catches the
  // upstream `make` generators (they're `custom: async (f, a) => ...`
  // bodies that shell out to `make -qp` plus a Makefile parse), so
  // there is no `script` array to key on at all. The strip-on-rewrite
  // contract holds: a successful native match returns the bare native
  // gen (plus optional cache), dropping `_custom`, `_customSource`,
  // `requires_js`, `js_source`, `js_runtime`, `script`, and
  // `script_template` along with every other internal marker.
  if (gen._custom) {
    const native = matchNativeFromJsSource(specName, gen._customSource);
    if (native) {
      const result = { ...native };
      if (gen.cache) result.cache = gen.cache;
      return result;
    }
    const extracted = subprocessExtractionResult(gen._customSource, gen, specName);
    if (extracted) return extracted;
    // Preserve the older self-contained path first so proven Custom
    // generators keep their host API and helper preamble semantics. TokenOnly
    // is the fallback for bodies that failed that proof but do not reference
    // host capabilities.
    const result = { requires_js: true };
    const runtime =
      buildSelfContainedJsRuntime('custom', gen._customSource) ??
      buildTokenOnlyJsRuntime(gen._customSource);
    if (runtime) {
      result.js_runtime = runtime;
    }
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
    const extracted = subprocessExtractionResult(gen._scriptSource, gen, specName);
    if (extracted) return extracted;
    // Preserve proven ScriptFunction behaviour first. TokenOnly is only the
    // host-API-free fallback for bodies that would otherwise be stripped.
    const result = { requires_js: true };
    const runtime =
      buildSelfContainedJsRuntime('script_function', gen._scriptSource) ??
      buildTokenOnlyJsRuntime(gen._scriptSource);
    if (runtime) {
      result.js_runtime = runtime;
    }
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
      const unknownHelper = unrecognizedSingleLetterHelper(gen._postProcessSource);
      if (unknownHelper) {
        console.error(
          `[helper-recovery] ${specName}: unrecognized single-letter postProcess helper ${unknownHelper} in ${gen._postProcessSource}`,
        );
      }
      const awsSdkGen = mapAwsSdkGenerator(specName, gen.script);
      if (awsSdkGen && !match.requires_js) {
        const result = {
          ...awsSdkGen,
          script: gen.script,
          transforms: match.transforms,
          _lowered_from_requires_js: true,
        };
        if (gen.cache) result.cache = gen.cache;
        return result;
      }

      const result = { script: gen.script };

      if (match.requires_js) {
        result.requires_js = true;
        // The matcher couldn't lower postProcess to declarative transforms
        // but may have extracted a usable JS body. When present, attach it
        // so the runtime can feed the script's stdout through the function.
        // When absent, leaving `js_runtime` off signals the runtime that
        // there is no JS to evaluate.
        if (match.js_source) {
          result.js_runtime = { kind: 'post_process', source: match.js_source };
        }
        // Propagate the _corrected_in marker set by the matcher for the
        // specific bug-class paths (substring/slice, JSON.parse
        // unresolvable-field). Passed through cleanGenerator via its
        // allowlist so it survives into the final JSON on disk.
        if (match._corrected_in) result._corrected_in = match._corrected_in;
      } else {
        result.transforms = match.transforms;
        result._lowered_from_requires_js = true;
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
const GENERATOR_FIELD_ALLOWLIST = new Set([
  '_corrected_in',
  '_lowered_from_requires_js',
  '_static_extracted_subprocess',
]);

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

  // Step 4: Apply command-specific static recoveries that are safer than
  // preserving helper-backed Fig custom generators.
  spec = applySpecFixups(spec, specName);

  // Step 5: Clean internal markers
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
    loweredTransformGenerators: 0,
    staticExtractedSubprocessGenerators: 0,
    requiresJsGenerators: 0,
    tokenOnlyGenerators: 0,
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
        else if (gen.transforms) {
          stats.transformGenerators++;
          if (gen._lowered_from_requires_js) stats.loweredTransformGenerators++;
          if (gen._static_extracted_subprocess) stats.staticExtractedSubprocessGenerators++;
        } else if (gen.requires_js) {
          stats.requiresJsGenerators++;
          if (gen.js_runtime?.kind === 'token_only') stats.tokenOnlyGenerators++;
        }
      }
    }
  }

  walk(spec);
  return stats;
}

/**
 * Get the list of all available spec names.
 *
 * The result is sorted lexicographically (on JS string code points) so
 * batch dispatch order is deterministic across runs and OSes — `readdir`'s
 * traversal order is filesystem-dependent on macOS and Linux. Determinism
 * here is load-bearing for the corpus-hash gate.
 */
export async function listSpecNames() {
  const entries = await readdir(BUILD_DIR);
  return entries
    .filter(f =>
      f.endsWith('.js') &&
      !f.startsWith('@') &&
      !f.startsWith('.') &&
      f !== 'index.js'
    )
    .map(f => f.replace(/\.js$/, ''))
    .sort();
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
    loweredTransformGenerators: 0,
    staticExtractedSubprocessGenerators: 0,
    requiresJsGenerators: 0,
    tokenOnlyGenerators: 0,
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
 * @param {boolean} [opts.deterministic=true] - Emit each spec via
 *   `stringifySorted` (sorted object keys) so two runs over the same input
 *   produce byte-identical files. Pass `false` only for back-compat /
 *   debugging; CI's corpus-hash gate assumes deterministic mode.
 * @param {boolean} [opts.reportDeclines=false] - Collect ux-11 subprocess
 *   extraction decline diagnostics.
 * @returns {Promise<{totals: object, errors: Array<{spec: string, error: string}>, declines: Array<{spec: string, reason: string}>}>}
 */
export async function runConversionBatch({
  specNames,
  outputDir,
  dryRun = false,
  deterministic = true,
  reportDeclines = false,
}) {
  const totals = makeEmptyTotals();
  const errors = [];
  const declines = [];
  const previousDeclineCollector = activeDeclineCollector;
  activeDeclineCollector = reportDeclines ? declines : null;

  try {
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
        // Canonical (sorted-key) emission so two runs over the same input
        // produce byte-identical output — gated by the determinism mode.
        // See `src/serialize.js` and `scripts/check-corpus-hash.mjs`.
        const serialized = deterministic
          ? stringifySorted(spec, 2)
          : JSON.stringify(spec, null, 2);
        await writeFile(outputPath, serialized + '\n');
      }

      totals.converted++;
      totals.subcommands += stats.subcommands;
      totals.options += stats.options;
      totals.generators += stats.generators;
      totals.nativeGenerators += stats.nativeGenerators;
      totals.transformGenerators += stats.transformGenerators;
      totals.loweredTransformGenerators += stats.loweredTransformGenerators;
      totals.staticExtractedSubprocessGenerators += stats.staticExtractedSubprocessGenerators;
      totals.requiresJsGenerators += stats.requiresJsGenerators;
      totals.tokenOnlyGenerators += stats.tokenOnlyGenerators;
      } catch (err) {
        errors.push({ spec: specName, error: err.message });
        totals.failed++;
      }
    }
  } finally {
    activeDeclineCollector = previousDeclineCollector;
  }

  return { totals, errors, declines };
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
  if (totals.loweredTransformGenerators > 0) {
    console.log(`    Lowered JS:     ${totals.loweredTransformGenerators}`);
  }
  if (totals.staticExtractedSubprocessGenerators > 0) {
    console.log(`    Static extract: ${totals.staticExtractedSubprocessGenerators}`);
  }
  console.log(`  Requires JS:      ${totals.requiresJsGenerators}`);
  if (totals.tokenOnlyGenerators > 0) {
    console.log(`  TokenOnly:        ${totals.tokenOnlyGenerators}`);
  }

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

function printTokenOnlyReport(totals) {
  console.log(`TokenOnly promoted: ${totals.tokenOnlyGenerators}`);
}

async function writeDeclineReport(declines) {
  const byReason = {};
  for (const entry of declines) {
    byReason[entry.reason] = (byReason[entry.reason] || 0) + 1;
  }
  const report = {
    total: declines.length,
    by_reason: Object.fromEntries(Object.entries(byReason).sort(([a], [b]) => a.localeCompare(b))),
    declines,
  };
  await mkdir(join(import.meta.dirname, '..', 'reports'), { recursive: true });
  await writeFile(UX11_DECLINE_REPORT, stringifySorted(report, 2) + '\n');
  console.log(`Decline report:     ${UX11_DECLINE_REPORT}`);
}

async function removeCorpusHash(outputDir) {
  await rm(join(outputDir, 'corpus-hash.txt'), { force: true });
}

async function exitOnConversionFailure(totals, errors, outputDir, dryRun) {
  if ((totals.failed ?? 0) === 0 && errors.length === 0) {
    if ((totals.converted ?? 0) > 0) {
      return false;
    }
    console.error('Conversion produced 0 specs; refusing to write corpus hash.');
    if (outputDir && !dryRun) {
      await removeCorpusHash(outputDir);
    }
    process.exitCode = 1;
    return true;
  }

  const failed = Math.max(totals.failed ?? 0, errors.length);
  console.error(
    `Conversion failed for ${failed} spec${failed === 1 ? '' : 's'}; refusing to write corpus hash.`
  );
  if (outputDir && !dryRun) {
    await removeCorpusHash(outputDir);
  }
  process.exitCode = 1;
  return true;
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
function runWorkerBatch({ batch, outputDir, dryRun, heapMb, deterministic, reportDeclines }) {
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
    // Forward the determinism mode to the worker — `--deterministic` is
    // default-on, so we only need to emit `--no-deterministic` when off.
    if (!deterministic) {
      args.push('--no-deterministic');
    }
    if (reportDeclines) {
      args.push('--report-declines');
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
        declines: [],
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
          declines: [],
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
          declines: [],
        });
        return;
      }

      try {
        const parsed = JSON.parse(last);
        resolvePromise({
          totals: { ...makeEmptyTotals(), ...(parsed.totals || {}) },
          errors: Array.isArray(parsed.errors) ? parsed.errors : [],
          declines: Array.isArray(parsed.declines) ? parsed.declines : [],
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
          declines: [],
        });
      }
    });
  });
}

/**
 * Compute the SHA-256 of every emitted spec under `outputDir`, hashing each
 * normalized relative path and file length before its contents in
 * sorted-relative-path order. Writes the result as a single line of hex
 * digest + newline to `<outputDir>/corpus-hash.txt` and returns the digest.
 *
 * Sort is on the spec's POSIX-style relative path (e.g. `aws/s3.json`)
 * so the order is OS-independent — Windows backslashes are normalised
 * before sorting. The hash file itself is excluded from the digest.
 *
 * @param {string} outputDir - Absolute path to the corpus directory.
 * @returns {Promise<string>} The hex SHA-256 digest.
 */
export async function writeCorpusHash(outputDir) {
  // Walk the corpus tree to collect every *.json spec file. We avoid
  // blowing the stack on large trees by iterating with an explicit queue.
  /** @type {string[]} */
  const files = [];
  /** @type {string[]} */
  const queue = [outputDir];
  while (queue.length > 0) {
    const dir = queue.shift();
    const entries = await readdir(dir, { withFileTypes: true });
    for (const e of entries) {
      const full = join(dir, e.name);
      if (e.isDirectory()) {
        queue.push(full);
      } else if (e.isFile() && e.name.endsWith('.json')) {
        files.push(full);
      }
    }
  }

  // Sort by POSIX-style relative path so order is OS-independent.
  files.sort((a, b) => {
    const ra = relative(outputDir, a).split(sep).join('/');
    const rb = relative(outputDir, b).split(sep).join('/');
    return ra < rb ? -1 : ra > rb ? 1 : 0;
  });

  if (files.length === 0) {
    throw new Error(`Refusing to write corpus hash for empty corpus: ${outputDir}`);
  }

  const hash = createHash('sha256');
  for (const file of files) {
    const rel = relative(outputDir, file).split(sep).join('/');
    const buf = await readFile(file);
    hash.update(
      `${Buffer.byteLength(rel, 'utf8')}\0${rel}\0${buf.length}\0`,
      'utf8',
    );
    hash.update(buf);
  }
  const digest = hash.digest('hex');
  await writeFile(join(outputDir, 'corpus-hash.txt'), digest + '\n');
  return digest;
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
      // Determinism mode (default ON). When set, every spec is emitted via
      // `stringifySorted` (sorted object keys), the input file list is
      // sorted before dispatch, and the orchestrator writes
      // `<output>/corpus-hash.txt` after the corpus is complete.
      // `--no-deterministic` opts back to legacy `JSON.stringify` for
      // back-compat / debugging only — CI's corpus-hash gate assumes ON.
      deterministic: { type: 'boolean', default: true },
      'no-deterministic': { type: 'boolean' },
      'report-token-only': { type: 'boolean' },
      'report-declines': { type: 'boolean' },
    },
  });

  const outputDir = values.output ? resolve(values.output) : null;
  const isDryRun = values['dry-run'] || false;
  const isWorker = values['batch-worker'] || false;
  // `--no-deterministic` overrides the default-on `--deterministic`.
  const deterministic = !values['no-deterministic'] && values.deterministic !== false;
  const reportTokenOnly = values['report-token-only'] || false;
  const reportDeclines = values['report-declines'] || false;

  if (!outputDir && !isDryRun) {
    console.error(
      'Usage: node src/index.js --output <dir> [--specs name1,name2] [--dry-run]\n' +
      '                          [--batch-size N] [--report-token-only] [--report-declines]\n' +
      '                          [--deterministic|--no-deterministic]\n' +
      '                          (default: --deterministic)',
    );
    process.exit(1);
  }

  if (outputDir) {
    await mkdir(outputDir, { recursive: true });
    if (!isDryRun) {
      await removeCorpusHash(outputDir);
    }
  }

  // Determine which specs to process. User-supplied --specs lists are
  // sorted too so any consumer (snapshot tests, hash recompute) sees the
  // same iteration order regardless of how the user typed the list.
  let specNames;
  if (values.specs !== undefined) {
    specNames = values.specs.split(',').map((s) => s.trim()).filter(Boolean).sort();
  } else {
    specNames = await listSpecNames();
  }

  // --- Worker mode: do one batch in-process and emit JSON on stdout ---
  if (isWorker) {
    console.error(`[worker] converting ${specNames.length} specs`);
    const { totals, errors, declines } = await runConversionBatch({
      specNames,
      outputDir,
      dryRun: isDryRun,
      deterministic,
      reportDeclines,
    });
    // EXACTLY one line of JSON on stdout; nothing else.
    process.stdout.write(JSON.stringify({ totals, errors, declines }) + '\n');
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
    const { totals, errors, declines } = await runConversionBatch({
      specNames,
      outputDir,
      dryRun: isDryRun,
      deterministic,
      reportDeclines,
    });
    printSummary(totals, errors);
    if (reportTokenOnly) printTokenOnlyReport(totals);
    if (reportDeclines) await writeDeclineReport(declines);
    if (await exitOnConversionFailure(totals, errors, outputDir, isDryRun)) {
      return;
    }
    if (deterministic && outputDir && !isDryRun) {
      const digest = await writeCorpusHash(outputDir);
      console.log(`Corpus hash:        ${digest}`);
    }
    return;
  }

  // Batched path: spawn one Node subprocess per batch. Each child exits
  // after its batch, freeing its ESM module cache. Sequential on purpose
  // (parallelism is a separate plan — this fix is "don't OOM").
  const batches = chunk(specNames, batchSize);
  const aggregateTotals = makeEmptyTotals();
  const aggregateErrors = [];
  const aggregateDeclines = [];

  for (let i = 0; i < batches.length; i++) {
    const batch = batches[i];
    const first = batch[0];
    const last = batch[batch.length - 1];
    process.stderr.write(
      `[batch-${i + 1}/${batches.length}] converting ${batch.length} specs: ${first}..${last}\n`
    );
    const { totals, errors, declines } = await runWorkerBatch({
      batch,
      outputDir,
      dryRun: isDryRun,
      heapMb,
      deterministic,
      reportDeclines,
    });
    mergeTotals(aggregateTotals, totals);
    for (const e of errors) aggregateErrors.push(e);
    for (const d of declines) aggregateDeclines.push(d);
  }

  printSummary(aggregateTotals, aggregateErrors);
  if (reportTokenOnly) printTokenOnlyReport(aggregateTotals);
  if (reportDeclines) await writeDeclineReport(aggregateDeclines);
  if (await exitOnConversionFailure(aggregateTotals, aggregateErrors, outputDir, isDryRun)) {
    return;
  }
  if (deterministic && outputDir && !isDryRun) {
    const digest = await writeCorpusHash(outputDir);
    console.log(`Corpus hash:        ${digest}`);
  }
}

// Run main only when executed directly (not imported)
if (import.meta.url === `file://${process.argv[1]}`) {
  main().catch(err => {
    console.error('Fatal error:', err);
    process.exit(1);
  });
}
