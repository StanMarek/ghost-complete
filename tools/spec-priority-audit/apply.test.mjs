/**
 * Black-box tests for `apply.mjs`. Exercises the script through its public
 * CLI rather than importing helper functions directly, so the test suite is
 * decoupled from the script's internal layout.
 *
 * For each test we lay out a sandbox that mirrors the repo:
 *   <sandbox>/tools/spec-priority-audit/{apply.mjs,heuristics.json}
 *   <sandbox>/specs/*.json
 * and run `node apply.mjs [--dry-run]` from inside the sandbox. Because
 * `apply.mjs` resolves `specs/` and `heuristics.json` via `__dirname`,
 * dropping a verbatim copy of the script into the sandbox makes it operate
 * on the sandboxed files in complete isolation from the real `specs/` tree.
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, copyFile, readFile, writeFile, stat } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const APPLY_MJS = join(__dirname, 'apply.mjs');

/**
 * Build a fresh sandbox directory containing:
 *   - a copy of the real apply.mjs at `<root>/tools/spec-priority-audit/`
 *   - a custom heuristics.json next to it
 *   - the provided spec files written under `<root>/specs/`
 * Returns helpers for inspecting and rerunning.
 */
async function makeSandbox({ heuristics, specs }) {
  const root = await mkdtemp(join(tmpdir(), 'spec-priority-audit-test-'));
  const toolDir = join(root, 'tools', 'spec-priority-audit');
  const specsDir = join(root, 'specs');
  await mkdir(toolDir, { recursive: true });
  await mkdir(specsDir, { recursive: true });
  await copyFile(APPLY_MJS, join(toolDir, 'apply.mjs'));
  await writeFile(
    join(toolDir, 'heuristics.json'),
    JSON.stringify(heuristics, null, 2)
  );
  for (const [name, body] of Object.entries(specs)) {
    await writeFile(join(specsDir, name), JSON.stringify(body, null, 2) + '\n');
  }

  const run = (extraArgs = []) => {
    const res = spawnSync(
      'node',
      [join(toolDir, 'apply.mjs'), ...extraArgs],
      { encoding: 'utf8' }
    );
    return res;
  };

  const readSpec = async (name) => {
    const raw = await readFile(join(specsDir, name), 'utf8');
    return JSON.parse(raw);
  };

  return { root, run, readSpec, specsDir, toolDir };
}

// apply.mjs hard-fails when heuristics reference spec names that don't exist
// on disk, so each sandbox builds heuristics whose `specs` list lines up with
// the spec files it actually writes.
function gitOnlyHeuristics() {
  return {
    families: {
      vcs: {
        specs: ['git'],
        subcommands: { add: 85, status: 92 },
        flags: { '--force': 18, '-f': 18 },
      },
    },
  };
}

function cargoOnlyHeuristics() {
  return {
    families: {
      package_manager: {
        specs: ['cargo'],
        subcommands: { install: 92, build: 88 },
        flags: { '--release': 78, '-r': 78 },
      },
    },
  };
}

test('apply.mjs preserves an existing priority on a subcommand', async () => {
  const { run, readSpec } = await makeSandbox({
    heuristics: gitOnlyHeuristics(),
    specs: {
      'git.json': {
        name: 'git',
        // Heuristic would set status -> 92; existing override (50) must win.
        subcommands: [{ name: 'status', priority: 50 }],
      },
    },
  });

  const res = run();
  assert.equal(res.status, 0, `stderr: ${res.stderr}`);

  const spec = await readSpec('git.json');
  assert.equal(spec.subcommands[0].priority, 50);
});

test('apply.mjs skips writing when the heuristic value equals the kind base', async () => {
  // Subcommand kind base = 70, flag kind base = 30. Writing those would be a
  // no-op for ranking, so apply.mjs should not touch the spec file at all.
  const heuristics = {
    families: {
      vcs: {
        specs: ['git'],
        subcommands: { foo: 70 }, // equals SUBCOMMAND_KIND_BASE
        flags: { '--bar': 30 }, // equals FLAG_KIND_BASE
      },
    },
  };
  const { run, readSpec, specsDir } = await makeSandbox({
    heuristics,
    specs: {
      'git.json': {
        name: 'git',
        subcommands: [{ name: 'foo' }],
        options: [{ name: ['--bar'] }],
      },
    },
  });

  const before = (await stat(join(specsDir, 'git.json'))).mtimeMs;
  const res = run();
  assert.equal(res.status, 0, `stderr: ${res.stderr}`);

  const spec = await readSpec('git.json');
  // Neither kind-base value should have been written.
  assert.equal(spec.subcommands[0].priority, undefined);
  assert.equal(spec.options[0].priority, undefined);

  // mtime should not have advanced — we wrote no file.
  const after = (await stat(join(specsDir, 'git.json'))).mtimeMs;
  assert.equal(after, before);
});

test('apply.mjs recurses into nested subcommands', async () => {
  const { run, readSpec } = await makeSandbox({
    heuristics: gitOnlyHeuristics(),
    specs: {
      'git.json': {
        name: 'git',
        subcommands: [
          {
            name: 'remote',
            subcommands: [
              { name: 'add' }, // heuristic: vcs.subcommands.add = 85
            ],
          },
        ],
      },
    },
  });

  const res = run();
  assert.equal(res.status, 0, `stderr: ${res.stderr}`);

  const spec = await readSpec('git.json');
  // Nested `git remote add` must pick up the same bump as a top-level `add`.
  assert.equal(spec.subcommands[0].subcommands[0].priority, 85);
});

test('multi-name option picks the lowest priority among matching names', async () => {
  // `--force` (18) and `-f` (18) collide, but if names disagreed we must
  // pick the smallest — keep the test honest by making them differ.
  const heuristics = {
    families: {
      vcs: {
        specs: ['git'],
        subcommands: {},
        flags: { '--force': 25, '-f': 18 },
      },
    },
  };
  const { run, readSpec } = await makeSandbox({
    heuristics,
    specs: {
      'git.json': {
        name: 'git',
        options: [{ name: ['--force', '-f'] }],
      },
    },
  });

  const res = run();
  assert.equal(res.status, 0, `stderr: ${res.stderr}`);

  const spec = await readSpec('git.json');
  // Lowest priority across both names wins.
  assert.equal(spec.options[0].priority, 18);
});

test('apply.mjs is idempotent: a second run produces zero further changes', async () => {
  const { run, readSpec } = await makeSandbox({
    heuristics: cargoOnlyHeuristics(),
    specs: {
      'cargo.json': {
        name: 'cargo',
        subcommands: [{ name: 'install' }, { name: 'build' }],
        options: [{ name: ['--release'] }],
      },
    },
  });

  const first = run();
  assert.equal(first.status, 0, `first stderr: ${first.stderr}`);
  const after1 = await readSpec('cargo.json');
  assert.equal(after1.subcommands[0].priority, 92);
  assert.equal(after1.subcommands[1].priority, 88);
  assert.equal(after1.options[0].priority, 78);

  const second = run();
  assert.equal(second.status, 0, `second stderr: ${second.stderr}`);
  // The summary line includes a "Specs modified:" counter; second run should
  // touch zero files because every priority is already set.
  assert.match(second.stdout, /Specs modified:\s+0\b/);

  const after2 = await readSpec('cargo.json');
  assert.deepEqual(after2, after1);
});

test('apply.mjs fails cleanly when heuristics.json is malformed', async () => {
  const { toolDir, run } = await makeSandbox({
    heuristics: gitOnlyHeuristics(),
    specs: {
      'git.json': { name: 'git' },
    },
  });
  // Corrupt the heuristics file we just placed.
  await writeFile(join(toolDir, 'heuristics.json'), '{ not valid json');

  const res = run();
  assert.notEqual(res.status, 0);
  // Some non-empty diagnostic should land on stderr (Node's default
  // unhandled-rejection trace, in this case).
  assert.ok(
    res.stderr.length > 0,
    'expected a stderr message describing the failure'
  );
});

test('--dry-run reports counts but writes nothing', async () => {
  const { run, readSpec, specsDir } = await makeSandbox({
    heuristics: gitOnlyHeuristics(),
    specs: {
      'git.json': {
        name: 'git',
        subcommands: [{ name: 'status' }, { name: 'add' }],
        options: [{ name: ['--force'] }],
      },
    },
  });

  const before = (await stat(join(specsDir, 'git.json'))).mtimeMs;
  const res = run(['--dry-run']);
  assert.equal(res.status, 0, `stderr: ${res.stderr}`);
  // The dry-run banner is part of the contract; lock it in.
  assert.match(res.stdout, /dry-run: no files written/);

  const after = (await stat(join(specsDir, 'git.json'))).mtimeMs;
  assert.equal(after, before, 'spec file should be untouched on dry-run');

  const spec = await readSpec('git.json');
  // Priorities must NOT have been written.
  assert.equal(spec.subcommands[0].priority, undefined);
  assert.equal(spec.subcommands[1].priority, undefined);
  assert.equal(spec.options[0].priority, undefined);
});
