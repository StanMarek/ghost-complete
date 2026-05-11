import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

import { processGenerator } from './index.js';

const __dirname = dirname(fileURLToPath(import.meta.url));

// Generators referencing Fig's minified single-letter helpers (`l`, `p`,
// `c`, `d`, `h`, `f`) are flagged as having free identifiers by the AST
// analyzer. Because the runtime installs pure-JS definitions for those
// names in every non-token-only gc-jsrt job (see crates/gc-jsrt/src/helpers.js),
// `buildSelfContainedJsRuntime` preserves bodies whose only free identifiers
// are in `known-helpers.json`. TokenOnly is reserved for bodies that fail
// that older proof but still do not reference host capabilities.

describe('processGenerator — helper-bearing _custom bodies', () => {
  it('preserves the JS source when the only free identifiers are known helpers', () => {
    const gen = {
      _custom: true,
      _customSource: 'function(t){return l(t,"Roles","RoleName")}',
    };
    const result = processGenerator(gen, 'aws');
    assert.equal(result.requires_js, true);
    assert.ok(
      result.js_runtime,
      'expected js_runtime to be attached when all free idents are known helpers',
    );
    assert.equal(result.js_runtime.kind, 'custom');
    assert.equal(result.js_runtime.source, gen._customSource);
    assert.equal(result.js_runtime.self_contained, true);
  });

  it('preserves bodies that reference multiple known helpers', () => {
    const gen = {
      _scriptFunction: true,
      _scriptSource: 'e=>h(e,l)',
    };
    const result = processGenerator(gen, 'aws');
    assert.ok(
      result.js_runtime,
      'expected js_runtime to be attached for helper-only references',
    );
    assert.equal(result.js_runtime.kind, 'script_function');
    assert.equal(result.js_runtime.source, gen._scriptSource);
    assert.equal(result.js_runtime.self_contained, true);
  });

  it('preserves bodies with genuinely unknown free identifiers as token_only', () => {
    const gen = {
      _custom: true,
      _customSource: 'function(t){return xyzUnknownHelper(t)}',
    };
    const result = processGenerator(gen, 'aws');
    assert.equal(result.requires_js, true);
    assert.deepStrictEqual(result.js_runtime, {
      kind: 'token_only',
      source: gen._customSource,
      self_contained: false,
    });
  });

  it('preserves bodies mixing known helpers with unknown identifiers as token_only', () => {
    const gen = {
      _custom: true,
      _customSource: 'function(t){let x = unknownThing(); return l(t,x,"name")}',
    };
    const result = processGenerator(gen, 'aws');
    assert.equal(result.requires_js, true);
    assert.deepStrictEqual(result.js_runtime, {
      kind: 'token_only',
      source: gen._customSource,
      self_contained: false,
    });
  });
});

describe('known-helpers.json', () => {
  it('is a JSON file listing string identifier names', async () => {
    const path = join(__dirname, 'known-helpers.json');
    const raw = await readFile(path, 'utf8');
    const data = JSON.parse(raw);
    assert.ok(Array.isArray(data.helpers), 'expected { "helpers": [ ... ] }');
    assert.ok(data.helpers.length >= 6, 'expected at least 6 helper names');
    for (const name of data.helpers) {
      assert.equal(typeof name, 'string');
      assert.match(name, /^[a-z]$/, `helper "${name}" should be a single lower-case letter`);
    }
    // Required helpers must stay in sync with crates/gc-jsrt/src/helpers.js
    for (const required of ['l', 'p', 'c', 'd', 'h', 'f']) {
      assert.ok(data.helpers.includes(required), `missing required helper "${required}"`);
    }
  });

  it('every name is bound on globalScope in crates/gc-jsrt/src/helpers.js', async () => {
    // Cross-validation: every helper allow-listed here MUST have a
    // corresponding `globalScope.<name> = ...` binding in the runtime
    // preamble. Without it, the converter would preserve a post_process
    // body referencing the name and the QuickJS sandbox would throw
    // ReferenceError at job dispatch — a silent regression that produces
    // zero suggestions.
    //
    // We READ helpers.js (no edits) and grep for top-level assignments
    // to globalScope. The pattern is the actual binding contract used
    // by the IIFE wrapper: see crates/gc-jsrt/src/helpers.js.
    const allowlistRaw = await readFile(
      join(__dirname, 'known-helpers.json'),
      'utf8',
    );
    const allowlist = JSON.parse(allowlistRaw).helpers;

    const helpersJsPath = join(
      __dirname,
      '..',
      '..',
      '..',
      'crates',
      'gc-jsrt',
      'src',
      'helpers.js',
    );
    const helpersSrc = await readFile(helpersJsPath, 'utf8');

    for (const name of allowlist) {
      // Match `globalScope.<name> = ...` with any whitespace around `=`.
      // Name is a single lower-case letter per the schema test above, so
      // a literal substring check is sufficient and unambiguous.
      //
      // NOTE: this regex is coupled to the current `globalScope.<name> = ...`
      // binding pattern in helpers.js. If that wrapper is refactored (e.g. to
      // `Object.assign(globalScope, {...})` or a loop), update this regex; the
      // 6 user-workflow goldens in crates/gc-suggest/tests/js_post_process_dispatch.rs
      // validate the runtime behavior independently and will catch a true
      // binding regression.
      const re = new RegExp(`\\bglobalScope\\.${name}\\s*=`);
      assert.match(
        helpersSrc,
        re,
        `helper "${name}" is allow-listed in known-helpers.json but has no globalScope.${name} = ... binding in crates/gc-jsrt/src/helpers.js`,
      );
    }
  });
});
