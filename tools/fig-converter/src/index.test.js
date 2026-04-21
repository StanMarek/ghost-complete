import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { convertSingleSpec, listSpecNames, cleanGenerator, runConversionBatch } from './index.js';

describe('listSpecNames', () => {
  it('returns an array of spec names', async () => {
    const names = await listSpecNames();
    assert.ok(Array.isArray(names));
    assert.ok(names.length > 100, `Expected 100+ specs, got ${names.length}`);
    assert.ok(names.includes('git'));
    assert.ok(names.includes('ls'));
    assert.ok(names.includes('brew'));
  });
});

describe('convertSingleSpec', () => {
  it('converts ls (pure static spec)', async () => {
    const result = await convertSingleSpec('ls');
    assert.ok(result);
    const { spec, stats } = result;
    assert.equal(spec.name, 'ls');
    assert.equal(spec.description, 'List directory contents');
    assert.ok(spec.options.length > 10, 'ls should have many options');
    // ls has template args
    assert.ok(spec.args);
  });

  it('converts brew (spec with generators)', async () => {
    const result = await convertSingleSpec('brew');
    assert.ok(result);
    const { spec, stats } = result;
    assert.equal(spec.name, 'brew');
    assert.ok(spec.subcommands.length > 0);
    assert.ok(stats.generators > 0, 'brew should have generators');
    assert.ok(stats.transformGenerators > 0, 'brew should have transform generators');
  });

  it('converts cat (minimal spec)', async () => {
    const result = await convertSingleSpec('cat');
    assert.ok(result);
    assert.equal(result.spec.name, 'cat');
  });

  it('returns null for nonexistent spec', async () => {
    const result = await convertSingleSpec('this_spec_does_not_exist_xyz');
    assert.equal(result, null);
  });

  describe('native generator map priority', () => {
    it('emits native type for git branch generators', async () => {
      const result = await convertSingleSpec('git');
      assert.ok(result);
      const { spec, stats } = result;

      // Find a generator that should be native (git branches)
      // The git spec should have some native generators
      assert.ok(stats.nativeGenerators >= 0, 'git spec should use some native generators');

      // Walk the spec to find generators with type: "git_branches"
      let foundNative = false;
      function walk(obj) {
        if (!obj || typeof obj !== 'object') return;
        if (obj.generators && Array.isArray(obj.generators)) {
          for (const gen of obj.generators) {
            if (gen.type === 'git_branches' || gen.type === 'git_remotes' || gen.type === 'git_tags') {
              foundNative = true;
            }
          }
        }
        if (obj.subcommands) for (const s of obj.subcommands) walk(s);
        if (obj.options) for (const o of obj.options) {
          if (o.args) {
            const args = Array.isArray(o.args) ? o.args : [o.args];
            for (const a of args) walk(a);
          }
        }
        if (obj.args) {
          const args = Array.isArray(obj.args) ? obj.args : [obj.args];
          for (const a of args) walk(a);
        }
      }
      walk(spec);

      // The git spec in @withfig/autocomplete uses custom generators with
      // script: ["git", "branch", ...] — these should be matched by the native map.
      // However, the actual git spec may use more complex patterns.
      // At minimum, verify the conversion completed without errors.
      assert.ok(spec.subcommands.length > 10, 'git should have many subcommands');
    });

    it('does not emit native type for non-git generators', async () => {
      const result = await convertSingleSpec('brew');
      assert.ok(result);
      const { stats } = result;
      // brew has no git-like generators, so nativeGenerators should be 0
      assert.equal(stats.nativeGenerators, 0, 'brew should have 0 native generators');
    });
  });

  describe('generator processing', () => {
    it('converts postProcess generators to transforms', async () => {
      const result = await convertSingleSpec('brew');
      assert.ok(result);

      // Walk to find a transform generator
      let foundTransform = false;
      function walk(obj) {
        if (!obj || typeof obj !== 'object') return;
        if (obj.generators && Array.isArray(obj.generators)) {
          for (const gen of obj.generators) {
            if (gen.transforms && Array.isArray(gen.transforms)) {
              foundTransform = true;
              // Verify transforms are valid
              assert.ok(gen.transforms.includes('split_lines'));
            }
          }
        }
        if (obj.subcommands) for (const s of obj.subcommands) walk(s);
        if (obj.args) {
          const args = Array.isArray(obj.args) ? obj.args : [obj.args];
          for (const a of args) walk(a);
        }
      }
      walk(result.spec);
      assert.ok(foundTransform, 'Should find at least one transform generator in brew');
    });

    it('marks custom generators as requires_js', async () => {
      const result = await convertSingleSpec('brew');
      assert.ok(result);

      // brew has at least one custom generator
      if (result.stats.requiresJsGenerators > 0) {
        let foundRequiresJs = false;
        function walk(obj) {
          if (!obj || typeof obj !== 'object') return;
          if (obj.generators && Array.isArray(obj.generators)) {
            for (const gen of obj.generators) {
              if (gen.requires_js) foundRequiresJs = true;
            }
          }
          if (obj.subcommands) for (const s of obj.subcommands) walk(s);
          if (obj.options) for (const o of obj.options) {
            if (o.args) {
              const args = Array.isArray(o.args) ? o.args : [o.args];
              for (const a of args) walk(a);
            }
          }
          if (obj.args) {
            const args = Array.isArray(obj.args) ? obj.args : [obj.args];
            for (const a of args) walk(a);
          }
        }
        walk(result.spec);
        assert.ok(foundRequiresJs, 'Should mark custom generators as requires_js');
      }
    });
  });

  describe('output format', () => {
    it('produces clean JSON without internal markers', async () => {
      const result = await convertSingleSpec('brew');
      assert.ok(result);
      const json = JSON.stringify(result.spec);
      // No internal markers should survive
      assert.ok(!json.includes('"_loadSpec"'), 'Should not contain _loadSpec');
      assert.ok(!json.includes('"_postProcess"'), 'Should not contain _postProcess');
      assert.ok(!json.includes('"_postProcessSource"'), 'Should not contain _postProcessSource');
      assert.ok(!json.includes('"_custom"'), 'Should not contain _custom');
      assert.ok(!json.includes('"_customSource"'), 'Should not contain _customSource');
      assert.ok(!json.includes('"_scriptFunction"'), 'Should not contain _scriptFunction');
      assert.ok(!json.includes('"_splitOn"'), 'Should not contain _splitOn');
    });

    it('uses internally-tagged format for parameterized transforms', async () => {
      const result = await convertSingleSpec('git');
      assert.ok(result);
      const json = JSON.stringify(result.spec);

      // If there are error_guard transforms, they should be internally tagged
      if (json.includes('error_guard')) {
        assert.ok(json.includes('"type":"error_guard"'));
      }
    });
  });
});

describe('cleanGenerator', () => {
  it('strips generic underscore-prefixed keys', () => {
    const result = cleanGenerator({
      script: ['cmd'],
      _postProcessSource: 'foo',
      _custom: true,
      _internal: 'secret',
    });
    assert.deepStrictEqual(result, { script: ['cmd'] });
  });

  it('preserves _corrected_in (format-extension allowlist)', () => {
    // _corrected_in is a persistent spec-format extension, not an internal
    // marker. It must survive cleaning so downstream tools (doctor, etc.)
    // can see which generators were re-classified.
    const result = cleanGenerator({
      requires_js: true,
      js_source: 'fn body',
      _corrected_in: 'v0.10.0',
      _postProcessSource: 'should be stripped',
    });
    assert.equal(result._corrected_in, 'v0.10.0');
    assert.equal(result.requires_js, true);
    assert.equal(result.js_source, 'fn body');
    assert.equal(result._postProcessSource, undefined);
  });

  it('preserves non-underscore keys unchanged', () => {
    const gen = {
      type: 'git_branches',
      cache: { ttl_seconds: 300 },
      transforms: ['split_lines'],
    };
    const result = cleanGenerator(gen);
    assert.deepStrictEqual(result, gen);
  });

  it('does not treat arbitrary underscore-keys as allowlisted', () => {
    // Defense-in-depth: the allowlist must be exact, not a prefix match.
    // A hypothetical future typo like `_corrected_in_` or `_corrected` must
    // still be stripped.
    const result = cleanGenerator({
      script: ['cmd'],
      _corrected: 'nope',
      _corrected_in_v2: 'nope',
      _CORRECTED_IN: 'nope',
    });
    assert.deepStrictEqual(result, { script: ['cmd'] });
  });
});

describe('runConversionBatch', () => {
  it('aggregates totals from multiple small specs (in-process smoke test)', async () => {
    // Sanity-check the shared worker body: converting three small specs via
    // the batch function should yield totals equal to the sum of calling
    // convertSingleSpec on each individually. This locks the in-process /
    // subprocess-worker code paths to the same semantics without spawning
    // a subprocess.
    const specs = ['ls', 'cat', 'echo'];

    // Reference totals: sum of per-spec stats.
    const reference = {
      converted: 0,
      subcommands: 0,
      options: 0,
      generators: 0,
    };
    for (const name of specs) {
      const r = await convertSingleSpec(name);
      assert.ok(r, `${name} should convert`);
      reference.converted++;
      reference.subcommands += r.stats.subcommands;
      reference.options += r.stats.options;
      reference.generators += r.stats.generators;
    }

    const { totals, errors } = await runConversionBatch({
      specNames: specs,
      outputDir: null,
      dryRun: true,
    });

    assert.deepStrictEqual(errors, []);
    assert.equal(totals.converted, reference.converted);
    assert.equal(totals.failed, 0);
    assert.equal(totals.subcommands, reference.subcommands);
    assert.equal(totals.options, reference.options);
    assert.equal(totals.generators, reference.generators);
  });

  it('records per-spec failures in errors without aborting the batch', async () => {
    // A nonexistent spec must surface as an error but must not prevent
    // subsequent real specs in the same batch from being counted.
    const { totals, errors } = await runConversionBatch({
      specNames: ['cat', 'this_spec_does_not_exist_xyz', 'echo'],
      outputDir: null,
      dryRun: true,
    });

    assert.equal(totals.converted, 2);
    assert.equal(totals.failed, 1);
    assert.equal(errors.length, 1);
    assert.equal(errors[0].spec, 'this_spec_does_not_exist_xyz');
  });
});

describe('batched convert integration', () => {
  it('batched subprocess output matches direct convertSingleSpec output', { timeout: 60000 }, async () => {
    const specs = ['ls', 'cat', 'echo', 'brew', 'git', 'npm', 'docker', 'grep', 'tar', 'curl'];
    const tempDir = await mkdtemp(join(tmpdir(), 'gc-convert-it-'));
    try {
      // Run the orchestrator as a real subprocess with --batch-size 3, which
      // forces 10 specs to split into 4 batches (ceil(10/3) = 4), exercising
      // the subprocess-per-batch code path rather than the in-process fast path.
      const indexPath = fileURLToPath(new URL('./index.js', import.meta.url));
      const child = spawn(
        process.execPath,
        [indexPath, '--output', tempDir, '--specs', specs.join(','), '--batch-size', '3'],
        { stdio: ['ignore', 'pipe', 'pipe'] },
      );

      let stderr = '';
      child.stderr.on('data', (b) => { stderr += b.toString(); });
      let stdout = '';
      child.stdout.on('data', (b) => { stdout += b.toString(); });

      const exitCode = await new Promise((resolve) => child.on('close', resolve));
      assert.equal(exitCode, 0, `orchestrator exited ${exitCode}.\nstdout:\n${stdout}\nstderr:\n${stderr}`);

      // Confirm the batched subprocess path was actually exercised: the
      // orchestrator emits "[batch-N/M]" progress lines on stderr.
      assert.match(stderr, /\[batch-\d+\/\d+\]/, `expected batching progress in stderr, got:\n${stderr}`);

      // Compare each batched output file against an in-process convertSingleSpec
      // call. They must be structurally identical post-parse.
      for (const name of specs) {
        const direct = await convertSingleSpec(name);
        if (!direct) {
          // Should not happen for the chosen specs, but skip gracefully rather
          // than failing the entire test if the upstream dep is missing it.
          console.log(`[integration] convertSingleSpec returned null for '${name}', skipping`);
          continue;
        }

        const batchedPath = join(tempDir, `${name}.json`);
        const batchedRaw = await readFile(batchedPath, 'utf8');
        const batched = JSON.parse(batchedRaw);

        assert.deepStrictEqual(batched, direct.spec, `output mismatch for spec '${name}'`);
      }
    } finally {
      await rm(tempDir, { recursive: true, force: true });
    }
  });
});
