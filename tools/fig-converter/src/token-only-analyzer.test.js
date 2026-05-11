import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { analyzeTokenOnly } from './token-only-analyzer.js';
import { processGenerator } from './index.js';

function assertAccepted(source) {
  const result = analyzeTokenOnly(source);
  assert.equal(result.token_only, true, `expected accept, got: ${JSON.stringify(result)}`);
  assert.equal(result.rejection, null);
}

function assertRejected(source, code, name) {
  const result = analyzeTokenOnly(source);
  assert.equal(result.token_only, false, `expected reject, got: ${JSON.stringify(result)}`);
  assert.equal(result.rejection?.code, code);
  if (name) assert.equal(result.rejection?.name, name);
}

describe('analyzeTokenOnly', () => {
  it('accepts kubectl-style previous-token helpers with free identifiers', () => {
    assertAccepted(`
      async (tokens) => {
        const previous = previousToken(tokens);
        if (!previous) return KUBE_RESOURCES;
        return RESOURCE_FIELDS[previous] || [];
      }
    `);
  });

  it('accepts a token state machine', () => {
    assertAccepted(`
      (tokens) => {
        let mode = "resource";
        const out = [];
        for (const token of tokens) {
          if (token === "-n" || token === "--namespace") mode = "namespace";
          else if (mode === "namespace") mode = "resource";
          else out.push(token);
        }
        return mode === "namespace" ? ["default", "kube-system"] : out;
      }
    `);
  });

  it('accepts a regex filter', () => {
    assertAccepted(`
      (tokens) => ["pods", "services", "deployments"]
        .filter((name) => /^depl/.test(name) || name.match(/pod/))
    `);
  });

  it('accepts a baked lookup table', () => {
    assertAccepted(`
      (tokens) => ({
        get: ["pods", "services", "deployments"],
        describe: ["nodes", "namespaces"],
      })[tokens[0] || "get"] || []
    `);
  });

  it('rejects await calls to free identifiers', () => {
    assertRejected(
      `async () => await runShell("kubectl get pods")`,
      'await_free_identifier_call',
      'runShell',
    );
  });

  it('rejects transpiled async generator yield calls to free identifiers', () => {
    assertRejected(
      `(tokens, runner, ctx) => ce(this, void 0, void 0, function* () {
        const result = yield runner({ command: "ls", args: ["-1"] });
        return result.stdout.split("\\n");
      })`,
      'yield_free_identifier_call',
      'runner',
    );
  });

  it('rejects fetch', () => {
    assertRejected(`async () => fetch("https://example.com")`, 'capability_identifier', 'fetch');
  });

  it('rejects process.env', () => {
    assertRejected(`() => process.env.PATH ? [] : []`, 'capability_identifier', 'process');
  });

  it('rejects fig.fs.readFile', () => {
    assertRejected(`async () => fig.fs.readFile("/tmp/file")`, 'capability_namespace', 'fig.fs');
  });

  it('rejects executeShellCommand', () => {
    assertRejected(
      `(ctx) => executeShellCommand({ command: "ls" })`,
      'capability_identifier',
      'executeShellCommand',
    );
  });
});

describe('processGenerator token_only wiring', () => {
  it('_custom promotes host-API-free bodies when self-contained proof fails', () => {
    const source = 'async (tokens) => ue(tokens).map(v.getCurrentInsertedDirectory)';
    const out = processGenerator(
      {
        _custom: true,
        _customSource: source,
      },
      '__token_only_test_spec__',
    );

    assert.deepStrictEqual(out, {
      requires_js: true,
      js_runtime: {
        kind: 'token_only',
        source,
        self_contained: false,
      },
    });
  });

  it('_scriptFunction promotes host-API-free bodies when self-contained proof fails', () => {
    const source = '(tokens) => ge(tokens, v.getCurrentInsertedDirectory)';
    const out = processGenerator(
      {
        _scriptFunction: true,
        _scriptSource: source,
      },
      '__token_only_test_spec__',
    );

    assert.deepStrictEqual(out, {
      requires_js: true,
      js_runtime: {
        kind: 'token_only',
        source,
        self_contained: false,
      },
    });
  });

  it('falls back to self-contained custom runtime when token-only rejects', () => {
    const source = '(executeShellCommand) => executeShellCommand ? [] : []';
    const out = processGenerator(
      {
        _custom: true,
        _customSource: source,
      },
      '__token_only_test_spec__',
    );

    assert.deepStrictEqual(out, {
      requires_js: true,
      js_runtime: {
        kind: 'custom',
        source,
        self_contained: true,
      },
    });
  });

  it('preserves self-contained script functions instead of changing argv semantics', () => {
    const source = '(tokens) => ["echo", tokens[tokens.length - 1] || "hello"]';
    const out = processGenerator(
      {
        _scriptFunction: true,
        _scriptSource: source,
      },
      '__token_only_test_spec__',
    );

    assert.deepStrictEqual(out, {
      requires_js: true,
      js_runtime: {
        kind: 'script_function',
        source,
        self_contained: true,
      },
    });
  });
});
