// determinism.test.js
//
// Pins the canonical-emit / corpus-hash determinism contract introduced
// for the ux-9b precursor work (PLAN.md Phase 3, SPEC § C).
//
// Invariants under test:
//   1. `stringifySorted` returns byte-identical output across N invocations
//      on the same value (idempotency).
//   2. The shape of the source object — specifically the order in which
//      keys were inserted — does NOT affect the canonical output, because
//      `stringifySorted` recursively re-orders by `Object.keys(...).sort()`.
//   3. Arrays preserve their original order — Fig spec semantics depend
//      on it (`subcommands` order = completion priority, `options` order
//      = documented order).
//   4. The corpus-hash function is stable: writing N spec files and
//      hashing them in sorted-relative-path order produces the same
//      SHA-256 across runs.
//
// PATH DEVIATION: PLAN.md says `tools/fig-converter/test/determinism.test.js`
// but the repo uses a flat `src/*.test.js` layout (matched by the
// package.json test glob). Following the existing convention.

import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { stringifySorted, canonicalize } from './serialize.js';
import { writeCorpusHash } from './index.js';

// 3 specs, distinct shapes — covers nested objects, options arrays, and
// a degenerate spec with no children. Matches the PLAN's note 10 fixture
// (Object.keys insertion order is intentionally NOT alphabetic so we can
// see the canonicalizer at work).
const FIXTURE = {
  'fixture-a': {
    name: 'fixture-a',
    subcommands: [{ name: 'b' }, { name: 'a' }],
  },
  'fixture-b': {
    name: 'fixture-b',
    options: [{ name: '--foo' }, { name: '--bar' }],
  },
  'fixture-c': {
    name: 'fixture-c',
  },
};

const RUNS = 5;
const INDEX_JS = fileURLToPath(new URL('./index.js', import.meta.url));

function runConverter(args) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [INDEX_JS, ...args], {
      stdio: ['ignore', 'pipe', 'pipe'],
    });

    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (buf) => {
      stdout += buf.toString('utf8');
    });
    child.stderr.on('data', (buf) => {
      stderr += buf.toString('utf8');
    });
    child.on('error', reject);
    child.on('close', (code) => {
      resolve({ code, stdout, stderr });
    });
  });
}

async function assertHashFileMissing(dir) {
  await assert.rejects(
    readFile(join(dir, 'corpus-hash.txt'), 'utf8'),
    { code: 'ENOENT' },
    'corpus-hash.txt must not be written for incomplete conversions',
  );
}

async function writeJsonFiles(dir, files, creationOrder) {
  for (const relPath of creationOrder) {
    const path = join(dir, relPath);
    await mkdir(dirname(path), { recursive: true });
    await writeFile(path, files[relPath]);
  }
}

describe('stringifySorted', () => {
  it('produces byte-identical output across 5 runs (per spec)', () => {
    for (const [_name, spec] of Object.entries(FIXTURE)) {
      const renders = new Set();
      for (let i = 0; i < RUNS; i++) {
        renders.add(stringifySorted(spec, 2));
      }
      assert.equal(
        renders.size,
        1,
        `expected stringifySorted to be idempotent across ${RUNS} runs, got ${renders.size} distinct outputs`,
      );
    }
  });

  it('output is independent of object key insertion order', () => {
    // Same logical spec built two different ways. Without sorting, these
    // would JSON-encode to different strings.
    const a = { name: 'cmd', description: 'd', options: [{ name: '--x' }] };
    const b = {};
    b.options = [{ name: '--x' }];
    b.description = 'd';
    b.name = 'cmd';

    assert.equal(stringifySorted(a, 2), stringifySorted(b, 2));
  });

  it('preserves array order (Fig array order is semantic)', () => {
    // subcommands order is documented as priority-bearing — sorting it
    // would silently reshuffle the completion menu. canonicalize must
    // sort OBJECT keys but leave arrays alone.
    const reversed = stringifySorted({
      subcommands: [{ name: 'b' }, { name: 'a' }],
    }, 2);
    const parsed = JSON.parse(reversed);
    assert.deepStrictEqual(
      parsed.subcommands.map((s) => s.name),
      ['b', 'a'],
      'array order must be preserved verbatim',
    );
  });

  it('matches JSON.stringify shape after key sort (newline / indent / escaping)', () => {
    // Sanity check: the canonical render is just JSON.stringify with
    // sorted keys. Indent and escaping rules are inherited.
    const value = { z: 1, a: 'a "b"\n' };
    assert.equal(
      stringifySorted(value, 2),
      JSON.stringify({ a: 'a "b"\n', z: 1 }, null, 2),
    );
  });

  it('canonicalize on a primitive returns the primitive', () => {
    assert.equal(canonicalize(null), null);
    assert.equal(canonicalize(42), 42);
    assert.equal(canonicalize('s'), 's');
    assert.equal(canonicalize(true), true);
  });
});

describe('writeCorpusHash', () => {
  it('produces a stable SHA-256 across 5 fresh tempdirs with the same fixture', async () => {
    const hashes = new Set();

    for (let i = 0; i < RUNS; i++) {
      const dir = await mkdtemp(join(tmpdir(), 'fig-determinism-'));
      try {
        for (const [name, spec] of Object.entries(FIXTURE)) {
          await writeFile(
            join(dir, `${name}.json`),
            stringifySorted(spec, 2) + '\n',
          );
        }
        const digest = await writeCorpusHash(dir);
        // The digest written to corpus-hash.txt and the return value must
        // agree. (corpus-hash.txt is excluded from the digest itself —
        // only *.json files are hashed.)
        hashes.add(digest);
      } finally {
        await rm(dir, { recursive: true, force: true });
      }
    }

    assert.equal(
      hashes.size,
      1,
      `expected one stable hash across ${RUNS} runs, got ${hashes.size} distinct: ${[...hashes].join(', ')}`,
    );
  });

  it('hash is hex SHA-256 (64 hex chars)', async () => {
    const dir = await mkdtemp(join(tmpdir(), 'fig-determinism-'));
    try {
      for (const [name, spec] of Object.entries(FIXTURE)) {
        await writeFile(join(dir, `${name}.json`), stringifySorted(spec, 2) + '\n');
      }
      const digest = await writeCorpusHash(dir);
      assert.match(digest, /^[0-9a-f]{64}$/, 'expected 64-char lowercase hex digest');
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  });

  it('hashes sorted relative paths independent of file creation order', async () => {
    const files = {
      'root.json': stringifySorted({ name: 'root', options: [{ name: '--root' }] }, 2) + '\n',
      'nested/alpha.json': stringifySorted({ name: 'alpha' }, 2) + '\n',
      'nested/deeper/zeta.json': stringifySorted({
        name: 'zeta',
        subcommands: [{ name: 'last' }],
      }, 2) + '\n',
    };
    const dirA = await mkdtemp(join(tmpdir(), 'fig-determinism-'));
    const dirB = await mkdtemp(join(tmpdir(), 'fig-determinism-'));
    try {
      await writeJsonFiles(dirA, files, [
        'nested/deeper/zeta.json',
        'root.json',
        'nested/alpha.json',
      ]);
      await writeJsonFiles(dirB, files, [
        'nested/alpha.json',
        'nested/deeper/zeta.json',
        'root.json',
      ]);

      const digestA = await writeCorpusHash(dirA);
      const digestB = await writeCorpusHash(dirB);
      const writtenA = (await readFile(join(dirA, 'corpus-hash.txt'), 'utf8')).trim();
      const writtenB = (await readFile(join(dirB, 'corpus-hash.txt'), 'utf8')).trim();

      assert.equal(digestA, digestB);
      assert.equal(writtenA, writtenB);
      assert.equal(writtenA, digestA);
    } finally {
      await rm(dirA, { recursive: true, force: true });
      await rm(dirB, { recursive: true, force: true });
    }
  });

  it('hash changes when a spec changes (smoke: not a no-op)', async () => {
    const dirA = await mkdtemp(join(tmpdir(), 'fig-determinism-'));
    const dirB = await mkdtemp(join(tmpdir(), 'fig-determinism-'));
    try {
      for (const [name, spec] of Object.entries(FIXTURE)) {
        await writeFile(join(dirA, `${name}.json`), stringifySorted(spec, 2) + '\n');
      }
      // Mutate one spec so dirB differs from dirA in spec content.
      const mutated = { ...FIXTURE };
      mutated['fixture-c'] = { name: 'fixture-c-CHANGED' };
      for (const [name, spec] of Object.entries(mutated)) {
        await writeFile(join(dirB, `${name}.json`), stringifySorted(spec, 2) + '\n');
      }
      const a = await writeCorpusHash(dirA);
      const b = await writeCorpusHash(dirB);
      assert.notEqual(a, b, 'hash should change when a spec changes');
    } finally {
      await rm(dirA, { recursive: true, force: true });
      await rm(dirB, { recursive: true, force: true });
    }
  });
});

describe('converter CLI failure handling', () => {
  it('exits non-zero and skips corpus hash when the fast path records failures', async () => {
    const dir = await mkdtemp(join(tmpdir(), 'fig-determinism-'));
    try {
      const result = await runConverter([
        '--output',
        dir,
        '--specs',
        'this_spec_does_not_exist_xyz',
      ]);

      assert.equal(
        result.code,
        1,
        `expected converter to fail on invalid --specs entry\nstdout:\n${result.stdout}\nstderr:\n${result.stderr}`,
      );
      assert.match(result.stdout, /Failed:\s+1/);
      assert.match(result.stdout, /this_spec_does_not_exist_xyz/);
      await assertHashFileMissing(dir);
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  });

  it('exits non-zero and skips corpus hash when the batched path records failures', { timeout: 60000 }, async () => {
    const dir = await mkdtemp(join(tmpdir(), 'fig-determinism-'));
    try {
      const result = await runConverter([
        '--output',
        dir,
        '--specs',
        'cat,this_spec_does_not_exist_xyz,echo',
        '--batch-size',
        '1',
      ]);

      assert.equal(
        result.code,
        1,
        `expected converter to fail when a worker reports a failed spec\nstdout:\n${result.stdout}\nstderr:\n${result.stderr}`,
      );
      assert.match(result.stdout, /Converted:\s+2/);
      assert.match(result.stdout, /Failed:\s+1/);
      assert.match(result.stdout, /this_spec_does_not_exist_xyz/);
      await assertHashFileMissing(dir);
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  });
});
