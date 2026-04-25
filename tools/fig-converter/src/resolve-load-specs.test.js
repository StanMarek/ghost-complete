/**
 * resolve-load-specs.test.js
 *
 * Focused cycle-guard tests for `resolveLoadSpecs`. The fixtures here are Fig
 * raw specs fed through `convertSpec` to produce the intermediate shape that
 * `resolveLoadSpecs` expects (`subcommands[].* _loadSpec` markers).
 *
 * Upstream reality that motivates these tests:
 *   - `simctl.js` self-references via its `help` subcommand (`loadSpec: "simctl"`)
 *   - `xcrun.js` transitively loads `simctl`, so the cycle cascades
 * Without a guard, both blow up Node's heap. See docs/phase-minus-1-followups.md §2.
 */

import { describe, it, beforeEach, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { convertSpec } from './static-converter.js';
import { resolveLoadSpecs } from './index.js';

/** Captures console.warn output for the duration of a test. */
function captureWarn() {
  const warnings = [];
  const original = console.warn;
  console.warn = (...args) => {
    warnings.push(args.map((a) => String(a)).join(' '));
  };
  return {
    warnings,
    restore() {
      console.warn = original;
    },
  };
}

/**
 * Build a loader from a map of {name: rawFigSpec}. Unknown names return null,
 * matching the real `loadFigSpec` contract.
 */
function makeLoader(specs) {
  return async (name) => (specs[name] ? specs[name] : null);
}

describe('resolveLoadSpecs cycle guard', () => {
  let warnCapture;

  beforeEach(() => {
    warnCapture = captureWarn();
  });

  afterEach(() => {
    warnCapture.restore();
  });

  it('detects direct self-reference A → A (one warning, no infinite recursion)', async () => {
    // A has a subcommand "help" whose loadSpec points back to A.
    const rawA = {
      name: 'A',
      subcommands: [
        { name: 'help', loadSpec: 'A' },
        { name: 'other', description: 'leaf' },
      ],
    };
    const intermediate = convertSpec(rawA);
    const loader = makeLoader({ A: rawA });

    const result = await resolveLoadSpecs(intermediate, 'A', new Set(['A']), loader);

    assert.equal(warnCapture.warnings.length, 1, 'expected exactly one warning');
    assert.match(warnCapture.warnings[0], /loadSpec cycle detected/);
    assert.match(warnCapture.warnings[0], /A → A/);
    // The help subcommand survives with no _loadSpec marker remaining.
    const help = result.subcommands.find((s) => s.name === 'help');
    assert.ok(help, 'help subcommand should still exist');
    assert.equal(help._loadSpec, undefined, '_loadSpec marker should be stripped');
    // Sibling subcommand untouched.
    const other = result.subcommands.find((s) => s.name === 'other');
    assert.ok(other, 'sibling subcommand should still exist');
  });

  it('detects two-hop cycle A → B → A (one warning)', async () => {
    const rawA = {
      name: 'A',
      subcommands: [{ name: 'to-b', loadSpec: 'B' }],
    };
    const rawB = {
      name: 'B',
      subcommands: [{ name: 'back-to-a', loadSpec: 'A' }],
    };
    const intermediate = convertSpec(rawA);
    const loader = makeLoader({ A: rawA, B: rawB });

    const result = await resolveLoadSpecs(intermediate, 'A', new Set(['A']), loader);

    assert.equal(warnCapture.warnings.length, 1, 'expected exactly one warning');
    assert.match(warnCapture.warnings[0], /A → B → A/);
    // A's subcommand "to-b" should have been inlined with B's subcommands.
    const toB = result.subcommands.find((s) => s.name === 'to-b');
    assert.ok(toB, 'to-b subcommand should exist');
    assert.ok(Array.isArray(toB.subcommands), 'to-b should have inlined B.subcommands');
    const backToA = toB.subcommands.find((s) => s.name === 'back-to-a');
    assert.ok(backToA, 'back-to-a should still exist (inlined from B)');
    assert.equal(backToA._loadSpec, undefined, '_loadSpec marker stripped on cycle skip');
  });

  it('detects three-hop cycle A → B → C → A (one warning)', async () => {
    const rawA = {
      name: 'A',
      subcommands: [{ name: 'to-b', loadSpec: 'B' }],
    };
    const rawB = {
      name: 'B',
      subcommands: [{ name: 'to-c', loadSpec: 'C' }],
    };
    const rawC = {
      name: 'C',
      subcommands: [{ name: 'back-to-a', loadSpec: 'A' }],
    };
    const intermediate = convertSpec(rawA);
    const loader = makeLoader({ A: rawA, B: rawB, C: rawC });

    const result = await resolveLoadSpecs(intermediate, 'A', new Set(['A']), loader);

    assert.equal(warnCapture.warnings.length, 1);
    assert.match(warnCapture.warnings[0], /A → B → C → A/);
    // Walk the inlined chain: A.to-b → B's content → to-c → C's content → back-to-a
    const toB = result.subcommands.find((s) => s.name === 'to-b');
    assert.ok(toB && Array.isArray(toB.subcommands), 'A.to-b should inline B');
    const toC = toB.subcommands.find((s) => s.name === 'to-c');
    assert.ok(toC && Array.isArray(toC.subcommands), 'B.to-c should inline C');
    const backToA = toC.subcommands.find((s) => s.name === 'back-to-a');
    assert.ok(backToA, 'C.back-to-a should exist (cycle-skipped, marker stripped)');
    assert.equal(backToA._loadSpec, undefined);
  });

  it('resolves deep non-cyclic chain A → B → C → D without warnings', async () => {
    const rawA = { name: 'A', subcommands: [{ name: 'to-b', loadSpec: 'B' }] };
    const rawB = { name: 'B', subcommands: [{ name: 'to-c', loadSpec: 'C' }] };
    const rawC = { name: 'C', subcommands: [{ name: 'to-d', loadSpec: 'D' }] };
    const rawD = {
      name: 'D',
      subcommands: [{ name: 'leaf', description: 'terminal' }],
    };
    const intermediate = convertSpec(rawA);
    const loader = makeLoader({ A: rawA, B: rawB, C: rawC, D: rawD });

    const result = await resolveLoadSpecs(intermediate, 'A', new Set(['A']), loader);

    assert.equal(warnCapture.warnings.length, 0, 'no cycle warnings expected');

    // All four layers should be inlined end-to-end.
    const toB = result.subcommands.find((s) => s.name === 'to-b');
    assert.ok(toB && Array.isArray(toB.subcommands));
    const toC = toB.subcommands.find((s) => s.name === 'to-c');
    assert.ok(toC && Array.isArray(toC.subcommands));
    const toD = toC.subcommands.find((s) => s.name === 'to-d');
    assert.ok(toD && Array.isArray(toD.subcommands));
    const leaf = toD.subcommands.find((s) => s.name === 'leaf');
    assert.ok(leaf, 'deepest leaf should be inlined');
    // No _loadSpec markers anywhere.
    assert.ok(!JSON.stringify(result).includes('_loadSpec'));
  });

  it('allows sibling subcommands to load the same target without false positive', async () => {
    // A has two subcommands, both loadSpec: "B". Neither B nor A form a cycle.
    // If visited were shared globally across siblings, the second sibling would
    // trip the guard. This test pins that visited is path-scoped.
    const rawA = {
      name: 'A',
      subcommands: [
        { name: 'first', loadSpec: 'B' },
        { name: 'second', loadSpec: 'B' },
      ],
    };
    const rawB = {
      name: 'B',
      subcommands: [{ name: 'leaf', description: 'terminal' }],
    };
    const intermediate = convertSpec(rawA);
    const loader = makeLoader({ A: rawA, B: rawB });

    const result = await resolveLoadSpecs(intermediate, 'A', new Set(['A']), loader);

    assert.equal(warnCapture.warnings.length, 0, 'sibling same-target loads must not warn');
    const first = result.subcommands.find((s) => s.name === 'first');
    const second = result.subcommands.find((s) => s.name === 'second');
    assert.ok(first && Array.isArray(first.subcommands) && first.subcommands.length === 1);
    assert.ok(second && Array.isArray(second.subcommands) && second.subcommands.length === 1);
    assert.equal(first.subcommands[0].name, 'leaf');
    assert.equal(second.subcommands[0].name, 'leaf');
  });

  it('applies cycle guard to object-form loadSpec ({ specName: ... })', async () => {
    const rawA = {
      name: 'A',
      subcommands: [{ name: 'help', loadSpec: { specName: 'A' } }],
    };
    const intermediate = convertSpec(rawA);
    const loader = makeLoader({ A: rawA });

    const result = await resolveLoadSpecs(intermediate, 'A', new Set(['A']), loader);

    assert.equal(warnCapture.warnings.length, 1);
    assert.match(warnCapture.warnings[0], /A → A/);
    const help = result.subcommands.find((s) => s.name === 'help');
    assert.ok(help);
    assert.equal(help._loadSpec, undefined);
  });

  it('leaves function-form loadSpec untouched (sets requires_js, no cycle interaction)', async () => {
    // The intermediate spec carries the function reference directly on _loadSpec.
    // convertSpec preserves it as-is because typeof figSub.loadSpec !== 'undefined'.
    const fn = () => ({ name: 'dynamic' });
    const rawA = {
      name: 'A',
      subcommands: [{ name: 'dyn', loadSpec: fn }],
    };
    const intermediate = convertSpec(rawA);
    // Sanity: the marker made it through the static pass.
    assert.equal(typeof intermediate.subcommands[0]._loadSpec, 'function');

    const loader = makeLoader({ A: rawA });
    const result = await resolveLoadSpecs(intermediate, 'A', new Set(['A']), loader);

    assert.equal(warnCapture.warnings.length, 0, 'function-form must not trigger cycle guard');
    const dyn = result.subcommands.find((s) => s.name === 'dyn');
    assert.ok(dyn);
    assert.equal(dyn.requires_js, true);
    assert.equal(dyn._loadSpec, undefined);
  });
});
