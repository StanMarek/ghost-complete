import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { cleanGenerator, processGenerator } from './index.js';
import { matchPostProcess, unrecognizedSingleLetterHelper } from './post-process-matcher.js';

const __dirname = dirname(fileURLToPath(import.meta.url));

async function loadHelperRegistry() {
  try {
    const raw = await readFile(join(__dirname, 'helper-registry.json'), 'utf8');
    return JSON.parse(raw);
  } catch {
    return {};
  }
}

const helperMatcherModule = await import('./helper-matcher.js').catch(() => ({}));
const matchHelperLookup = helperMatcherModule.matchHelperLookup ?? (() => null);

describe('helper registry', () => {
  it('pins provenance and covers Fig single-letter helpers', async () => {
    const registry = await loadHelperRegistry();

    assert.match(registry._provenance, /@withfig\/autocomplete/);
    assert.match(registry._pinned_to, /2\.692\.3/);
    assert.match(registry._pinned_to, /aef52acff84c45edde61ae610cc2c964802b9a38/);
    assert.equal(typeof registry.helpers, 'object');

    for (const name of ['l', 'p', 'c', 'd', 'h', 'f']) {
      assert.ok(registry.helpers[name], `missing helper registry entry for ${name}`);
      assert.ok(Array.isArray(registry.helpers[name].arity), `missing arity for ${name}`);
      assert.equal(typeof registry.helpers[name].mapping, 'object', `missing mapping for ${name}`);
    }
  });
});

describe('matchHelperLookup', () => {
  it('lowers list-extract helper calls to json_path_extract transforms', async () => {
    const registry = await loadHelperRegistry();
    const cases = [
      [
        'function(t){return l(t,"Roles","RoleName")}',
        { type: 'json_path_extract', array: 'Roles', name_field: 'RoleName' },
      ],
      [
        'e=>p(e,"CertificateSummaryList","CertificateArn")',
        {
          type: 'json_path_extract',
          array: 'CertificateSummaryList',
          name_field: 'CertificateArn',
        },
      ],
      [
        't=>d(t,"apps","appId")',
        { type: 'json_path_extract', array: 'apps', name_field: 'appId' },
      ],
      [
        't=>h(t,"clusters")',
        { type: 'json_path_extract', array: 'clusters' },
      ],
      [
        'function(t){return c(t,"MetricAlarms","AlarmName","AlarmDescription")}',
        {
          type: 'json_path_extract',
          array: 'MetricAlarms',
          name_field: 'AlarmName',
          description_field: 'AlarmDescription',
        },
      ],
    ];

    for (const [fnSource, transform] of cases) {
      assert.deepStrictEqual(matchHelperLookup(fnSource, registry), {
        transforms: [transform],
        requires_js: false,
        lowered_from_requires_js: true,
      });
    }
  });

  it('lowers named function expression bodies the same as anonymous ones', async () => {
    // SPEC § D enumerates `function name(t){...}` as one of the
    // accepted shapes; Babel parses these as FunctionExpression with
    // `id` set, which the matcher should accept without depending on
    // anonymity. Pinning this prevents a future predicate tightening
    // from silently dropping helper bodies that happen to be named.
    const registry = await loadHelperRegistry();
    assert.deepStrictEqual(
      matchHelperLookup(
        'function listRoles(t){return l(t,"Roles","RoleName")}',
        registry,
      ),
      {
        transforms: [
          { type: 'json_path_extract', array: 'Roles', name_field: 'RoleName' },
        ],
        requires_js: false,
        lowered_from_requires_js: true,
      },
    );
  });

  it('recognizes f helper calls but keeps them on the JS fallback', async () => {
    const registry = await loadHelperRegistry();
    const fnSource = 'stdout=>f(stdout,"lambda.amazonaws.com")';

    assert.deepStrictEqual(matchHelperLookup(fnSource, registry), {
      transforms: null,
      requires_js: true,
      js_source: fnSource,
    });
  });

  it('does not lower unknown single-letter helper calls', async () => {
    const registry = await loadHelperRegistry();

    assert.equal(matchHelperLookup('t=>q(t,"Roles","RoleName")', registry), null);
  });

  it('quotes helper keys that are flat JS properties but structural JSONPath syntax', async () => {
    const registry = await loadHelperRegistry();

    assert.deepStrictEqual(matchHelperLookup('t=>l(t,"foo.bar","0")', registry), {
      transforms: [
        {
          type: 'json_path_extract',
          array: "['foo.bar']",
          name_field: "['0']",
        },
      ],
      requires_js: false,
      lowered_from_requires_js: true,
    });
  });

  it('keeps helpers as JS when a flat key cannot be represented safely', async () => {
    const registry = await loadHelperRegistry();

    assert.equal(matchHelperLookup('t=>l(t,`foo${bar}`,"name")', registry), null);
    assert.equal(matchHelperLookup('t=>l(t,`foo.bar"\\\'baz`,"name")', registry), null);
  });

  it('surfaces unrecognized single-letter helper calls for dry-run diagnostics', () => {
    assert.equal(unrecognizedSingleLetterHelper('t=>q(t,"Roles","RoleName")'), 'q');
    assert.equal(unrecognizedSingleLetterHelper('t=>l(t,"Roles","RoleName")'), null);
    assert.equal(unrecognizedSingleLetterHelper('t=>JSON.parse(t).Roles'), null);
  });
});

describe('helper lowering through post-process conversion', () => {
  it('matchPostProcess runs helper lowering before JSON.parse and split matchers', () => {
    const fnSource = 'function(t){return l(t,"Roles","RoleName")}';

    assert.deepStrictEqual(matchPostProcess(fnSource), {
      transforms: [
        { type: 'json_path_extract', array: 'Roles', name_field: 'RoleName' },
      ],
      requires_js: false,
      lowered_from_requires_js: true,
    });
  });

  it('processGenerator keeps fallback script data when helper lowering maps to aws_sdk', () => {
    const out = processGenerator(
      {
        script: ['aws', 'iam', 'list-roles'],
        _postProcessSource: 'function(t){return l(t,"Roles","RoleName")}',
      },
      'aws',
    );

    assert.deepStrictEqual(out, {
      type: 'aws_sdk',
      params: {
        service: 'iam',
        operation: 'ListRoles',
        field: 'Roles[*].RoleName',
        description_field: 'Roles[*].Arn',
      },
      script: ['aws', 'iam', 'list-roles'],
      transforms: [
        { type: 'json_path_extract', array: 'Roles', name_field: 'RoleName' },
      ],
      _lowered_from_requires_js: true,
    });
  });

  it('cleanGenerator preserves the lowered helper marker', () => {
    assert.deepStrictEqual(cleanGenerator({
      script: ['cmd'],
      _lowered_from_requires_js: true,
      _internal: 'drop me',
    }), {
      script: ['cmd'],
      _lowered_from_requires_js: true,
    });
  });

  it('unknown single-letter helper calls remain requires_js', () => {
    const originalError = console.error;
    const warnings = [];
    console.error = (message) => warnings.push(String(message));
    let out;
    try {
      out = processGenerator(
        {
          script: ['aws', 'unknown'],
          _postProcessSource: 't=>q(t,"Roles","RoleName")',
        },
        'aws',
      );
    } finally {
      console.error = originalError;
    }

    assert.equal(out.requires_js, true);
    assert.equal(out.transforms, undefined);
    assert.equal(out._lowered_from_requires_js, undefined);
    assert.equal(out.js_runtime?.kind, 'post_process');
    assert.match(out.js_runtime.source, /^t=>q/);
    assert.match(warnings[0], /unrecognized single-letter postProcess helper q/);
  });
});
