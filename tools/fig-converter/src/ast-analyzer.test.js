import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';
import { analyzeGenerator } from './ast-analyzer.js';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const fixturesDir = resolve(__dirname, '..', 'fixtures');

/**
 * Helper — pull the `fig_api_refs` array down to just the names, for
 * readable assertions in tests that don't care about `kind`.
 */
function refNames(result) {
  return result.fig_api_refs.map((r) => r.name).sort();
}

function findRef(result, name) {
  return result.fig_api_refs.find((r) => r.name === name);
}

describe('analyzeGenerator — Fig API detection', () => {
  it('case 1: flags a direct Fig API call as `direct`', () => {
    const src = `(ctx) => ctx.executeShellCommand({ command: "git status" })`;
    const result = analyzeGenerator(src);
    assert.equal(result.parse_error, null);
    const ref = findRef(result, 'executeShellCommand');
    assert.ok(ref, 'expected executeShellCommand in refs');
    assert.equal(ref.kind, 'direct');
  });

  it('case 2: flags a destructured binding as `destructured`', () => {
    const src = `
      const { executeShellCommand } = fig;
      export default (ctx) => executeShellCommand({ command: "ls" });
    `;
    const result = analyzeGenerator(src);
    assert.equal(result.parse_error, null);
    const ref = findRef(result, 'executeShellCommand');
    assert.ok(ref, 'expected executeShellCommand in refs');
    assert.equal(ref.kind, 'destructured');
  });

  it('case 3: flags a plain rename as `aliased`', () => {
    const src = `
      const { executeShellCommand } = fig;
      const cmd = executeShellCommand;
      export default () => cmd({ command: "ls" });
    `;
    const result = analyzeGenerator(src);
    assert.equal(result.parse_error, null);
    // Both the original destructured ref AND the alias should appear.
    const aliasRef = findRef(result, 'cmd');
    assert.ok(aliasRef, 'expected alias `cmd` to be tracked');
    assert.equal(aliasRef.kind, 'aliased');
  });

  it('case 4: flags a member-expression rename as `member-aliased`', () => {
    const src = `
      const cmd = fig.executeShellCommand;
      export default () => cmd({ command: "ls" });
    `;
    const result = analyzeGenerator(src);
    assert.equal(result.parse_error, null);
    const aliasRef = findRef(result, 'cmd');
    assert.ok(aliasRef, 'expected alias `cmd` to be tracked as member-aliased');
    assert.equal(aliasRef.kind, 'member-aliased');
  });

  it('case 5: flags CJS `require("fig")` re-export as `reexported`', () => {
    const src = `
      const lib = require('fig');
      module.exports = () => lib.executeShellCommand({ command: "ls" });
    `;
    const result = analyzeGenerator(src);
    assert.equal(result.parse_error, null);
    const libRef = findRef(result, 'lib');
    assert.ok(libRef, 'expected `lib` to be flagged as a reexported Fig module');
    assert.equal(libRef.kind, 'reexported');
  });

  it('case 6: flags ES-module import from Fig module', () => {
    const src = `
      import { executeShellCommand } from 'fig';
      export default () => executeShellCommand({ command: "ls" });
    `;
    const result = analyzeGenerator(src);
    assert.equal(result.parse_error, null);
    const ref = findRef(result, 'executeShellCommand');
    assert.ok(ref, 'expected executeShellCommand from ESM import');
    assert.equal(ref.kind, 'reexported');
  });

  it('case 7: allowlist smoke fixture produces zero fig refs', () => {
    const src = readFileSync(resolve(fixturesDir, 'allowlist-smoke.js'), 'utf8');
    const result = analyzeGenerator(src);
    assert.equal(result.parse_error, null, `parse error: ${result.parse_error}`);
    assert.deepEqual(
      result.fig_api_refs,
      [],
      `expected no fig refs, got: ${JSON.stringify(result.fig_api_refs, null, 2)}`
    );
  });

  it('case 8: suspicious-refs fixture flags fakeFigThing as `free`', () => {
    const src = readFileSync(resolve(fixturesDir, 'suspicious-refs.js'), 'utf8');
    const result = analyzeGenerator(src);
    assert.equal(result.parse_error, null);
    const ref = findRef(result, 'fakeFigThing');
    assert.ok(ref, 'expected fakeFigThing to be flagged');
    assert.equal(ref.kind, 'free');
  });

  it('case 9: malformed JS returns zero-filled struct with parse_error', () => {
    const src = '(out) => out.split("'; // unterminated string, unclosed paren
    const result = analyzeGenerator(src);
    assert.ok(result.parse_error, 'expected parse_error to be set');
    assert.equal(typeof result.parse_error, 'string');
    assert.deepEqual(result.fig_api_refs, []);
    assert.deepEqual(result.shape, {
      fingerprint: '',
      has_json_parse: false,
      has_regex_match: false,
      has_substring_or_slice: false,
      has_conditional: false,
      has_await: false,
    });
  });

  it('case 10: pure transform has zero fig refs', () => {
    const src = `(out) => out.split("\\n").filter(Boolean).map(e => ({name: e}))`;
    const result = analyzeGenerator(src);
    assert.equal(result.parse_error, null);
    assert.deepEqual(result.fig_api_refs, []);
  });
});

describe('analyzeGenerator — shape fingerprint', () => {
  it('case 11: fingerprint is stable across differing string literals', () => {
    const a = analyzeGenerator(`(out) => out.split("\\n").filter(Boolean).map(e => ({name: e}))`);
    const b = analyzeGenerator(`(out) => out.split("|").filter(Boolean).map(e => ({name: e}))`);
    assert.equal(a.parse_error, null);
    assert.equal(b.parse_error, null);
    assert.equal(a.shape.fingerprint, b.shape.fingerprint);
    // And it should be the canonical form.
    assert.equal(a.shape.fingerprint, '.split(STR).filter(FN).map(FN)');
  });

  it('case 12: JSON.parse with property chain produces a documented fingerprint', () => {
    const result = analyzeGenerator(`(out) => JSON.parse(out).foo.bar`);
    assert.equal(result.parse_error, null);
    assert.equal(result.shape.fingerprint, 'JSON.parse(...).PROP.PROP');
    assert.equal(result.shape.has_json_parse, true);
  });

  it('case 13: has_await tracks presence of await expressions', () => {
    const asyncResult = analyzeGenerator(`
      async (ctx) => {
        const r = await ctx.executeShellCommand({ command: "git status" });
        return r.stdout.split("\\n");
      }
    `);
    assert.equal(asyncResult.parse_error, null);
    assert.equal(asyncResult.shape.has_await, true);

    const syncResult = analyzeGenerator(`(out) => out.split("\\n")`);
    assert.equal(syncResult.parse_error, null);
    assert.equal(syncResult.shape.has_await, false);
  });

  it('case 14: has_conditional tracks branching control flow', () => {
    const conditional = analyzeGenerator(`(x) => x.a ? x.b : x.c`);
    assert.equal(conditional.parse_error, null);
    assert.equal(conditional.shape.has_conditional, true);

    const straight = analyzeGenerator(`(x) => x.a.b.c`);
    assert.equal(straight.parse_error, null);
    assert.equal(straight.shape.has_conditional, false);
  });

  it('case 15: has_substring_or_slice distinguishes substring/slice from other calls', () => {
    const sub = analyzeGenerator(`(s) => s.substring(0, 5)`);
    assert.equal(sub.parse_error, null);
    assert.equal(sub.shape.has_substring_or_slice, true);

    const sliceArr = analyzeGenerator(`(arr) => arr.slice(0, 10)`);
    assert.equal(sliceArr.parse_error, null);
    assert.equal(sliceArr.shape.has_substring_or_slice, true);

    const none = analyzeGenerator(`(out) => out.split("\\n").map(x => x)`);
    assert.equal(none.parse_error, null);
    assert.equal(none.shape.has_substring_or_slice, false);
  });

  it('bonus: has_regex_match true when .match() is called with a regex literal', () => {
    const withRegex = analyzeGenerator(`(s) => s.match(/^foo/).slice(1)`);
    assert.equal(withRegex.parse_error, null);
    assert.equal(withRegex.shape.has_regex_match, true);

    const noRegex = analyzeGenerator(`(s) => s.split("\\n")`);
    assert.equal(noRegex.parse_error, null);
    assert.equal(noRegex.shape.has_regex_match, false);
  });

  it('bonus: has_json_parse false without JSON.parse', () => {
    const result = analyzeGenerator(`(out) => out.split("\\n")`);
    assert.equal(result.parse_error, null);
    assert.equal(result.shape.has_json_parse, false);
  });
});
