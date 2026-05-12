import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { extractSubprocessGenerator } from './subprocess-extractor.js';

describe('extractSubprocessGenerator', () => {
  it('lifts a single literal subprocess plus line transform to native script transforms', () => {
    const result = extractSubprocessGenerator(`async (tokens, runShell) => {
      const { stdout } = await runShell({ command: "apt", args: ["list"] });
      return stdout.split("\\n").filter(Boolean).map((line) => ({ name: line.trim() }));
    }`);

    assert.equal(result.kind, 'native');
    assert.deepStrictEqual(result.script, ['apt', 'list']);
    assert.deepStrictEqual(result.transforms, ['split_lines', 'filter_empty', 'trim']);
    assert.equal(result._static_extracted_subprocess, true);
  });

  it('declines token-derived args', () => {
    const result = extractSubprocessGenerator(`async (tokens, runShell) => {
      const { stdout } = await runShell({
        command: "systemctl",
        args: tokens.includes("--user") ? ["list-units", "--user"] : ["list-units"]
      });
      return JSON.parse(stdout).map((unit) => ({ name: unit.unit }));
    }`);

    assert.equal(result.kind, 'declined');
    assert.equal(result.reason, 'tokens-derived-args');
  });

  it('declines multiple subprocess calls', () => {
    const result = extractSubprocessGenerator(`async (tokens, runShell) => {
      const { stdout: prefix } = await runShell({ command: "npm", args: ["prefix"] });
      const { stdout } = await runShell({ command: "cat", args: [prefix + "/package.json"] });
      return Object.keys(JSON.parse(stdout).dependencies ?? {}).map((name) => ({ name }));
    }`);

    assert.equal(result.kind, 'declined');
    assert.equal(result.reason, 'multiple-awaits');
  });

  it('declines filesystem-read subprocesses', () => {
    const result = extractSubprocessGenerator(`async (tokens, runShell) => {
      const { stdout } = await runShell({ command: "cat", args: ["package.json"] });
      return Object.keys(JSON.parse(stdout).dependencies ?? {}).map((name) => ({ name }));
    }`);

    assert.equal(result.kind, 'declined');
    assert.equal(result.reason, 'filesystem-read');
  });

  it('declines git subprocesses for native-map handling', () => {
    const result = extractSubprocessGenerator(`async (tokens, runShell) => {
      const { stdout } = await runShell({ command: "git", args: ["branch"] });
      return stdout.split("\\n").map((line) => ({ name: line.trim() }));
    }`);

    assert.equal(result.kind, 'declined');
    assert.equal(result.reason, 'git-subprocess-not-native');
  });

  it('falls back to post_process JS when the post-step exceeds the transform pipeline', () => {
    // SPEC § A point 4: "If the post-processing exceeds what the
    // transform pipeline can express, leave the JS body but set
    // `kind: post_process` and emit a working `js_runtime`." This is
    // the path that lets us lift a cargo/npm/systemctl subprocess
    // natively even when the post-step is too rich for transforms.
    const result = extractSubprocessGenerator(`async (tokens, runShell) => {
      const { stdout } = await runShell({ command: "cargo", args: ["metadata", "--format-version", "1", "--no-deps"] });
      const parsed = JSON.parse(stdout);
      const targets = parsed.packages.flatMap((p) => p.targets);
      const bins = targets.filter((t) => t.kind.includes("bin"));
      return bins.map((t) => ({ name: t.name, description: t.src_path }));
    }`);

    assert.equal(result.kind, 'fallback_post_process');
    assert.deepStrictEqual(result.script, [
      'cargo',
      'metadata',
      '--format-version',
      '1',
      '--no-deps',
    ]);
    assert.match(result.fallbackJsSource, /^function\(stdout\) \{/);
    assert.match(result.fallbackJsSource, /JSON\.parse\(stdout\)/);
    assert.match(result.fallbackJsSource, /p\.targets/);
  });
});
