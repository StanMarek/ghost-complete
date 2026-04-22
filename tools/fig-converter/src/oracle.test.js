// Unit tests for the Phase 0 oracle — pure helper functions only.
//
// The end-to-end oracle run (npm run oracle:changed) is the real acceptance
// test; these cover `compareResults`, `walkGenerators`, and `runJsGenerator`
// so regressions in the diff logic or the sandbox surface get caught fast
// without spinning up cargo + specs.

import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { compareResults, walkGenerators, runJsGenerator, summarizeInputs } from './oracle.js';

describe('compareResults', () => {
  it('equal on matching name↔text pairs', () => {
    const js = [{ name: 'a' }, { name: 'b' }];
    const rust = [{ text: 'a' }, { text: 'b' }];
    assert.deepEqual(compareResults(js, rust), { equal: true });
  });

  it('length mismatch surfaces in reason', () => {
    const r = compareResults([{ name: 'a' }], [{ text: 'a' }, { text: 'b' }]);
    assert.equal(r.equal, false);
    assert.match(r.reason, /length mismatch/);
  });

  it('mismatched name↔text reports the offending index', () => {
    const r = compareResults([{ name: 'a' }, { name: 'b' }], [{ text: 'a' }, { text: 'c' }]);
    assert.equal(r.equal, false);
    assert.match(r.reason, /item\[1\]/);
  });

  it('js non-array value rejected', () => {
    const r = compareResults('not an array', []);
    assert.equal(r.equal, false);
    assert.match(r.reason, /js returned non-array/);
  });

  it('description compared only when BOTH sides have one', () => {
    // JS has description, rust doesn't — skip description comparison.
    const r1 = compareResults(
      [{ name: 'a', description: 'desc' }],
      [{ text: 'a' }],
    );
    assert.deepEqual(r1, { equal: true });

    // Both sides have description, but they disagree — fail.
    const r2 = compareResults(
      [{ name: 'a', description: 'desc1' }],
      [{ text: 'a', description: 'desc2' }],
    );
    assert.equal(r2.equal, false);
    assert.match(r2.reason, /description mismatch/);

    // Both agree on description — pass.
    const r3 = compareResults(
      [{ name: 'a', description: 'same' }],
      [{ text: 'a', description: 'same' }],
    );
    assert.deepEqual(r3, { equal: true });
  });

  it('empty arrays are equal', () => {
    assert.deepEqual(compareResults([], []), { equal: true });
  });
});

describe('walkGenerators', () => {
  it('yields all generators with the correct path', () => {
    const spec = {
      name: 'test',
      subcommands: [
        {
          name: 'sub',
          args: { generators: [{ requires_js: true, js_source: 'x' }] },
        },
      ],
    };
    const found = [...walkGenerators(spec, '')];
    assert.equal(found.length, 1);
    assert.equal(found[0].path, '/subcommands[0]/args/generators[0]');
    assert.equal(found[0].gen.js_source, 'x');
  });

  it('yields nested generators (legacy two-level nesting)', () => {
    // Unlikely in today's corpus but the walker should still handle it.
    const spec = {
      generators: [
        {
          generators: [{ js_source: 'inner' }],
        },
      ],
    };
    const found = [...walkGenerators(spec, '')];
    // The outer generators entry yields one result; walking into it yields
    // the inner generators entry as another.
    assert.equal(found.length, 2);
  });
});

describe('runJsGenerator', () => {
  // Note: JSON-roundtripping the vm's return value normalizes the prototype
  // (vm-created objects have a different `Object.prototype` reference than
  // the main realm, which makes assert.deepStrictEqual refuse to match even
  // though the structure is identical). The oracle itself only reads `.name`
  // / `.description`, so a string-property comparison is sufficient here.
  it('arrow function happy path', async () => {
    const r = await runJsGenerator('i => [{name: i + "x"}]', 'foo');
    assert.equal(r.outcome, 'ok');
    assert.equal(JSON.stringify(r.value), JSON.stringify([{ name: 'foox' }]));
  });

  it('bare function() expression', async () => {
    const r = await runJsGenerator('function(i){return [{name: i}]}', 'bar');
    assert.equal(r.outcome, 'ok');
    assert.equal(JSON.stringify(r.value), JSON.stringify([{ name: 'bar' }]));
  });

  it('async arrow return awaited', async () => {
    const r = await runJsGenerator('async i => [{name: "async_" + i}]', 'y');
    assert.equal(r.outcome, 'ok');
    assert.equal(JSON.stringify(r.value), JSON.stringify([{ name: 'async_y' }]));
  });

  it('exception classified js_exception', async () => {
    const r = await runJsGenerator('i => JSON.parse("not json")', 'x');
    assert.equal(r.outcome, 'error');
    assert.equal(r.kind, 'js_exception');
    assert.match(r.message, /SyntaxError/);
  });

  it('setTimeout is unavailable (sandbox is locked per plan §0.2)', async () => {
    const r = await runJsGenerator('i => setTimeout(() => {}, 0)', 'x');
    assert.equal(r.outcome, 'error');
    assert.equal(r.kind, 'js_exception');
    assert.match(r.message, /setTimeout/);
  });

  it('require is unavailable (sandbox is locked per plan §0.2)', async () => {
    const r = await runJsGenerator('i => require("fs")', 'x');
    assert.equal(r.outcome, 'error');
    assert.equal(r.kind, 'js_exception');
    assert.match(r.message, /require/);
  });
});

describe('summarizeInputs', () => {
  it('all-pass returns pass', () => {
    const s = summarizeInputs([{ kind: 'pass' }, { kind: 'pass' }]);
    assert.deepEqual(s, { outcome: 'pass' });
  });

  it('any fail wins over pass and oracle_error', () => {
    const s = summarizeInputs([
      { kind: 'pass' },
      { kind: 'fail', reason: 'length mismatch: js=0, rust=1' },
      { kind: 'oracle_error', exception_class: 'js_exception', exception: 'boom' },
    ]);
    assert.equal(s.outcome, 'fail');
    assert.match(s.diff_summary, /length mismatch/);
  });

  it('single oracle_error: exception + exception_class unchanged, no exception_classes', () => {
    const s = summarizeInputs([
      { kind: 'oracle_error', exception_class: 'js_exception', exception: 'js_exception: TypeError: bad' },
    ]);
    assert.equal(s.outcome, 'oracle_error');
    assert.equal(s.exception_class, 'js_exception');
    assert.equal(s.exception, 'js_exception: TypeError: bad');
    assert.equal(s.exception_classes, undefined);
  });

  it('multiple oracle_errors with same class: suffixes (+N more errors), no exception_classes', () => {
    const s = summarizeInputs([
      { kind: 'oracle_error', exception_class: 'js_exception', exception: 'js_exception: TypeError: a' },
      { kind: 'oracle_error', exception_class: 'js_exception', exception: 'js_exception: TypeError: b' },
      { kind: 'oracle_error', exception_class: 'js_exception', exception: 'js_exception: TypeError: c' },
    ]);
    assert.equal(s.outcome, 'oracle_error');
    assert.equal(s.exception_class, 'js_exception');
    assert.match(s.exception, /\(\+2 more errors\)$/);
    // backward-compat: still starts with the first error's message.
    assert.match(s.exception, /^js_exception: TypeError: a /);
    assert.equal(s.exception_classes, undefined);
  });

  it('two oracle_errors same class: singular "error" not "errors"', () => {
    const s = summarizeInputs([
      { kind: 'oracle_error', exception_class: 'js_exception', exception: 'js_exception: a' },
      { kind: 'oracle_error', exception_class: 'js_exception', exception: 'js_exception: b' },
    ]);
    assert.match(s.exception, /\(\+1 more error\)$/);
  });

  it('multiple oracle_errors with different classes: exception_classes lists distinct classes', () => {
    const s = summarizeInputs([
      { kind: 'oracle_error', exception_class: 'js_exception', exception: 'js_exception: a' },
      { kind: 'oracle_error', exception_class: 'rust_exception', exception: 'rust_exception: b' },
      { kind: 'oracle_error', exception_class: 'js_exception', exception: 'js_exception: c' },
    ]);
    assert.equal(s.outcome, 'oracle_error');
    assert.equal(s.exception_class, 'js_exception'); // first wins
    assert.match(s.exception, /\(\+2 more errors\)$/);
    assert.deepEqual(s.exception_classes, ['js_exception', 'rust_exception']);
  });

  it('mixed pass + oracle_errors still aggregated as oracle_error', () => {
    const s = summarizeInputs([
      { kind: 'pass' },
      { kind: 'oracle_error', exception_class: 'js_timeout', exception: 'js_timeout: took too long' },
      { kind: 'oracle_error', exception_class: 'js_exception', exception: 'js_exception: boom' },
    ]);
    assert.equal(s.outcome, 'oracle_error');
    assert.equal(s.exception_class, 'js_timeout');
    assert.deepEqual(s.exception_classes, ['js_timeout', 'js_exception']);
    assert.match(s.exception, /\(\+1 more error\)$/);
  });

  it('empty input list produces missing_fixture oracle_error', () => {
    const s = summarizeInputs([]);
    assert.equal(s.outcome, 'oracle_error');
    assert.equal(s.exception_class, 'missing_fixture');
  });
});
