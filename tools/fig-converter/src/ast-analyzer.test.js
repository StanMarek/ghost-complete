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

  it('case 16: recovers bare function expression via paren-wrap fallback', () => {
    // Bare `function(a){...}` is invalid at statement position under
    // sourceType: 'module' (the parser expects a FunctionDeclaration with
    // a name). The two-stage parser wraps in parens to force expression
    // context. Real corpus example: amplify.json's JSON.parse(a).envs.map(...).
    const src = `function(a){ return a.split("\\n") }`;
    const result = analyzeGenerator(src);
    assert.equal(result.parse_error, null, `expected clean parse, got: ${result.parse_error}`);
    assert.equal(result.shape.fingerprint, '.split(STR)');
    assert.deepEqual(result.fig_api_refs, []);
  });

  it('case 17: recovers object-literal return via paren-wrap fallback', () => {
    // `{ name: "x" }` at statement position is a BlockStatement, not an
    // ObjectExpression. Wrapping in parens disambiguates it as an expression.
    const src = `{ name: "x", description: "y" }`;
    const result = analyzeGenerator(src);
    assert.equal(result.parse_error, null, `expected clean parse, got: ${result.parse_error}`);
    // Fingerprint for a bare object literal is the OBJ placeholder token.
    assert.equal(result.shape.fingerprint, 'OBJ');
    assert.deepEqual(result.fig_api_refs, []);
  });

  it('case 18: genuinely malformed input still returns parse_error after fallback', () => {
    // An unclosed string/paren fails BOTH the plain parse and the wrapped
    // parse, so the error surfaces with the original module-mode message.
    //
    // This also PINS the error-position guarantee: the module-mode parse
    // reports the unterminated-string at column 19, while the paren-wrapped
    // parse would report it at column 20 (shifted by the leading `(`).
    // The analyzer returns the ORIGINAL module-mode error so diagnostics
    // point at the true offset in the source the caller passed in.
    const src = '(out) => out.split("'; // unterminated string, unclosed paren
    const result = analyzeGenerator(src);
    assert.ok(result.parse_error, 'expected parse_error to be set after fallback');
    assert.equal(typeof result.parse_error, 'string');
    // Exact module-mode message; the wrapped-parse message has (1:20).
    assert.equal(
      result.parse_error,
      'Unterminated string constant. (1:19)',
      'expected original module-mode error (col 19), not the wrapped-parse error (col 20)',
    );
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
});

describe('analyzeGenerator — shape fingerprint', () => {
  it('case 11: fingerprint is stable across differing string literals', () => {
    const a = analyzeGenerator(`(out) => out.split("\\n").filter(Boolean).map(e => ({name: e}))`);
    const b = analyzeGenerator(`(out) => out.split("|").filter(Boolean).map(e => ({name: e}))`);
    assert.equal(a.parse_error, null);
    assert.equal(b.parse_error, null);
    assert.equal(a.shape.fingerprint, b.shape.fingerprint);
    // And it should be the canonical form. Phase-2 recount: arrow callbacks
    // now descend into their body — `e => ({name: e})` has an ObjectExpression
    // body, so `.map(<OBJ>)` replaces the old `.map(FN)` shape.
    assert.equal(a.shape.fingerprint, '.split(STR).filter(FN).map(<OBJ>)');
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

  it('case 14b: has_conditional true for nullish coalescing (??)', () => {
    // `??` is a branch: right-hand side only evaluates when left is null/undefined.
    // Pinning this so the bucket classifier stays predictable (plan §1).
    const nullish = analyzeGenerator(`(x) => x ?? 1`);
    assert.equal(nullish.parse_error, null);
    assert.equal(nullish.shape.has_conditional, true);
  });

  it('case 14c: has_conditional true for short-circuit &&', () => {
    // Short-circuit `&&` as defensive property access is a branch.
    // Pinning this so the bucket classifier stays predictable (plan §1).
    const shortCircuit = analyzeGenerator(`(x) => x.foo && x.foo.bar`);
    assert.equal(shortCircuit.parse_error, null);
    assert.equal(shortCircuit.shape.has_conditional, true);
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

describe('analyzeGenerator — gap fixes (property access + destructure-from-arg)', () => {
  // -- Gap A: property-access against callback parameters --------------
  // A property name on a callback-param receiver (e.g. `.map(x => x.context)`)
  // must NOT be classified as a Fig API ref, even when the property name
  // happens to match a FIG_API_NAME. Only bare references or members of a
  // genuinely-free Fig identifier should flag.

  it('gap A.1: property on callback param is NOT a Fig ref (context)', () => {
    const src = `(out) => out.split('\\n').map(x => x.context.value)`;
    const result = analyzeGenerator(src);
    assert.equal(result.parse_error, null);
    assert.deepEqual(
      result.fig_api_refs,
      [],
      `expected no refs, got: ${JSON.stringify(result.fig_api_refs)}`,
    );
  });

  it('gap A.2: property on callback param is NOT a Fig ref (currentWorkingDirectory)', () => {
    const src = `(out) => out.filter(t => t.currentWorkingDirectory).map(t => t.name)`;
    const result = analyzeGenerator(src);
    assert.equal(result.parse_error, null);
    assert.deepEqual(
      result.fig_api_refs,
      [],
      `expected no refs, got: ${JSON.stringify(result.fig_api_refs)}`,
    );
  });

  it('gap A.3: member access on genuinely-free `fig` still flags executeShellCommand + fig', () => {
    const src = `(out) => fig.executeShellCommand({ command: "ls" })`;
    const result = analyzeGenerator(src);
    assert.equal(result.parse_error, null);
    // Don't over-correct: `fig` here is a free identifier, not a param,
    // and `.executeShellCommand` is a direct Fig API call.
    const names = refNames(result);
    assert.ok(names.includes('executeShellCommand'), `expected executeShellCommand, got: ${names}`);
    assert.ok(names.includes('fig'), `expected fig, got: ${names}`);
  });

  it('gap A.4: nested property on callback param is NOT a Fig ref', () => {
    const src = `(out) => out.map(x => ({...x, context: x.context}))`;
    const result = analyzeGenerator(src);
    assert.equal(result.parse_error, null);
    assert.deepEqual(
      result.fig_api_refs,
      [],
      `expected no refs, got: ${JSON.stringify(result.fig_api_refs)}`,
    );
  });

  it('gap A.5: method CALL on callback param is NOT a Fig ref', () => {
    // The strongest form of Gap A: `.map(x => x.executeShellCommand(...))`.
    // Here `x` is a callback-param binding (from `.map`), so `x.executeShellCommand`
    // is an unrelated method call on the input data — NOT a Fig API ref.
    // Pre-fix: the `CallExpression` visitor blindly pushes `executeShellCommand`
    // as `direct` for any `<obj>.FIG_API_NAME(args)` without checking whether
    // `<obj>` resolves to a callback-param binding.
    const src = `(out) => out.map(x => x.executeShellCommand(y))`;
    const result = analyzeGenerator(src);
    assert.equal(result.parse_error, null);
    const escRef = findRef(result, 'executeShellCommand');
    assert.equal(
      escRef,
      undefined,
      `expected no executeShellCommand ref, got: ${JSON.stringify(result.fig_api_refs)}`,
    );
  });

  it('gap A.6: method CALL on callback param is NOT a Fig ref (runCommand)', () => {
    const src = `(out) => out.map(x => x.runCommand())`;
    const result = analyzeGenerator(src);
    assert.equal(result.parse_error, null);
    assert.deepEqual(
      result.fig_api_refs,
      [],
      `expected no refs, got: ${JSON.stringify(result.fig_api_refs)}`,
    );
  });

  it('gap A.7: top-level arrow param (Fig ctx) method call STILL flags', () => {
    // Regression guard — don't over-correct. `(ctx) => ctx.executeShellCommand(...)`
    // is the canonical Fig-generator shape (see case 1). The top-level arrow
    // param is conventionally Fig context; we continue to flag this.
    const src = `(ctx) => ctx.executeShellCommand({ command: "git status" })`;
    const result = analyzeGenerator(src);
    assert.equal(result.parse_error, null);
    const ref = findRef(result, 'executeShellCommand');
    assert.ok(ref, 'expected executeShellCommand ref on top-level arrow param');
    assert.equal(ref.kind, 'direct');
  });

  // -- Gap B: destructure-from-arg (callback param) --------------------
  // Destructured names from an arbitrary callback param (e.g. array
  // element) are lexical to the arrow/callback, not pulled from a Fig
  // source. They must NOT be classified as Fig-API refs.

  it('gap B.1: destructure inside `.map(({searchTerm, context}) => …)` is NOT a Fig ref', () => {
    const src = `(out) => JSON.parse(out).map(({searchTerm, context}) => ({name: searchTerm}))`;
    const result = analyzeGenerator(src);
    assert.equal(result.parse_error, null);
    assert.deepEqual(
      result.fig_api_refs,
      [],
      `expected no refs, got: ${JSON.stringify(result.fig_api_refs)}`,
    );
  });

  it('gap B.2: module-scope destructuring from `fig` still flags executeShellCommand', () => {
    // Regression guard — don't over-correct the destructure exemption.
    const src = `
      const { executeShellCommand } = fig;
      export default (out) => executeShellCommand(out);
    `;
    const result = analyzeGenerator(src);
    assert.equal(result.parse_error, null);
    const ref = findRef(result, 'executeShellCommand');
    assert.ok(ref, 'expected executeShellCommand ref');
    assert.equal(ref.kind, 'destructured');
  });

  it('gap B.3: destructure in `.filter(({name}) => …)` is NOT a Fig ref', () => {
    const src = `(out) => out.filter(({name}) => name != null)`;
    const result = analyzeGenerator(src);
    assert.equal(result.parse_error, null);
    assert.deepEqual(
      result.fig_api_refs,
      [],
      `expected no refs, got: ${JSON.stringify(result.fig_api_refs)}`,
    );
  });

  it('gap B.4: destructure inside two-arg arrow param is NOT a Fig ref', () => {
    const src = `(_, args) => args.map(({context}) => context)`;
    const result = analyzeGenerator(src);
    assert.equal(result.parse_error, null);
    assert.deepEqual(
      result.fig_api_refs,
      [],
      `expected no refs, got: ${JSON.stringify(result.fig_api_refs)}`,
    );
  });
});

describe('analyzeGenerator — callback-body fingerprint descent (Phase 2 recount)', () => {
  // Phase 1 spike finding 3: `.map(FN)` hid dotted-path patterns inside
  // higher-order-function callbacks, so `needs_dotted_path_json_extract`
  // was undercounted. Arrow / function callbacks now descend into their
  // body and emit `.map(<inner>)`.

  it('descends into .map callback body and surfaces dotted JSON.parse path', () => {
    const src = `(out) => out.split("\\n").map(line => JSON.parse(line).metadata.id)`;
    const result = analyzeGenerator(src);
    assert.equal(result.parse_error, null);
    // Sanity: Pass-2 visitor still fires for symbols inside callbacks.
    assert.equal(result.shape.has_json_parse, true);
    // Pin exact fingerprint: dotted path nested inside .map(<...>).
    assert.equal(
      result.shape.fingerprint,
      '.split(STR).map(<JSON.parse(...).PROP.PROP>)'
    );
  });

  it('identity callback after .filter(Boolean) produces .map(<...>)', () => {
    // identity callback descent yields <...>; doesn't add signal.
    const src = `(out) => out.split("\\n").filter(Boolean).map(x => x)`;
    const result = analyzeGenerator(src);
    assert.equal(result.parse_error, null);
    assert.equal(
      result.shape.fingerprint,
      '.split(STR).filter(FN).map(<...>)'
    );
  });

  it('async callback preserves has_await AND descends into the body', () => {
    const src = `(out) => out.split("\\n").map(async x => await ctx.run(x))`;
    const result = analyzeGenerator(src);
    assert.equal(result.parse_error, null);
    // Pass-2 has_await flag unchanged.
    assert.equal(result.shape.has_await, true);
    // Pin exact fingerprint: `await ...` visible inside .map(<...>).
    assert.equal(
      result.shape.fingerprint,
      '.split(STR).map(<await .run(...)>)'
    );
  });

  it('plain identifier callback .map(Boolean) still fingerprints as .map(FN)', () => {
    // Regression guard: descent is gated on ArrowFunctionExpression /
    // FunctionExpression. Plain Identifiers stay in `leafToken`.
    const src = `(out) => out.split("\\n").map(Boolean)`;
    const result = analyzeGenerator(src);
    assert.equal(result.parse_error, null);
    assert.equal(result.shape.fingerprint, '.split(STR).map(FN)');
  });

  it('fingerprint is deterministic across repeated analyses', () => {
    const src = `(out) => out.split("\\n").map(line => JSON.parse(line).metadata.id)`;
    const a = analyzeGenerator(src).shape.fingerprint;
    const b = analyzeGenerator(src).shape.fingerprint;
    assert.equal(a, b);
  });
});
