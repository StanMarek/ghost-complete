import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { RECOGNIZERS, rewriteGenerators } from './patch-local-project-providers.mjs';

// Helpers — build minimal generator shapes that match each recognizer.
// Mirrors the actual @withfig/autocomplete output we're patching in
// specs/{make,npm,cargo}.json.

function makeMatchingGenerator() {
  return {
    requires_js: true,
    js_source: 'async (f, a) => { await a({ command: "bash", args: ["-c", "make -qp | awk -F: \'/^[a-zA-Z0-9]/{print $1}\'"] }) }',
    priority: 80,
    cache: { ttl_seconds: 5 },
  };
}

function npmMatchingGenerator() {
  return {
    script: ['bash', '-c', "until [[ -f package.json ]] || [[ $PWD = '/' ]]; do cd ..; done; cat package.json"],
    js_source: 'function (out) { return Object.entries(JSON.parse(out).scripts).map(([k]) => ({ name: k })) }',
    priority: 80,
    cache: { ttl_seconds: 5 },
  };
}

function cargoMatchingGenerator() {
  return {
    script: ['cargo', 'metadata', '--format-version', '1', '--no-deps'],
    js_source: 'e=>JSON.parse(e).packages.map(a=>({icon:"pkg",name:a.name,description:a.version}))',
    priority: 80,
    cache: { ttl_seconds: 30 },
  };
}

function cargoFullMetadataPackageGenerator() {
  return {
    script: ['cargo', 'metadata', '--format-version', '1'],
    js_source: 'e=>JSON.parse(e).packages.map(a=>({name:a.name,description:a.description}))',
    priority: 80,
    cache: { ttl_seconds: 30 },
  };
}

function cargoDependencyGenerator() {
  return {
    script: ['cargo', 'metadata', '--format-version', '1'],
    js_source: 'e=>{let t=JSON.parse(e),i=le(t).flatMap(n=>n.dependencies).map(n=>({name:n.name,description:n.req}));return[...new Map(i.map(n=>[n.name,n])).values()]}',
    priority: 80,
    cache: { ttl_seconds: 30 },
  };
}

function nonMatchingGenerator() {
  return {
    script: ['echo', 'hello'],
    transforms: ['split_lines'],
    priority: 50,
  };
}

describe('rewriteGenerators — matching + sibling preservation', () => {
  it('rewrites make generator and preserves sibling priority/cache on the parent arg', () => {
    const spec = {
      name: 'make',
      priority: 80, // sibling field on the parent — must survive untouched.
      args: {
        name: 'target',
        priority: 80,
        generators: [makeMatchingGenerator()],
      },
    };
    const stats = { rewrites: 0 };
    rewriteGenerators(spec, RECOGNIZERS.make, stats);

    assert.equal(stats.rewrites, 1);
    assert.equal(spec.priority, 80);
    assert.equal(spec.args.priority, 80);
    assert.deepStrictEqual(spec.args.generators[0], {
      type: 'makefile_targets',
      cache: { ttl_seconds: 5 },
    });
  });

  it('rewrites make generator and preserves cache field on the rewritten generator', () => {
    // The recognizer carries `cache` from the input generator into the
    // replacement. This is the contract that lets the script preserve
    // hand-curated TTLs on a per-generator basis without having to know
    // anything else about the input shape.
    const original = makeMatchingGenerator();
    const spec = { args: { generators: [original] } };
    const stats = { rewrites: 0 };
    rewriteGenerators(spec, RECOGNIZERS.make, stats);
    assert.deepStrictEqual(spec.args.generators[0], {
      type: 'makefile_targets',
      cache: { ttl_seconds: 5 },
    });
  });

  it('rewrites npm bash-c generator to npm_scripts and preserves cache', () => {
    const spec = {
      subcommands: [
        {
          name: 'run',
          priority: 80,
          args: {
            generators: [npmMatchingGenerator()],
          },
        },
      ],
    };
    const stats = { rewrites: 0 };
    rewriteGenerators(spec, RECOGNIZERS.npm, stats);

    assert.equal(stats.rewrites, 1);
    assert.equal(spec.subcommands[0].priority, 80);
    assert.deepStrictEqual(spec.subcommands[0].args.generators[0], {
      type: 'npm_scripts',
      cache: { ttl_seconds: 5 },
    });
  });

  it('rewrites cargo metadata generator to cargo_workspace_members and preserves cache', () => {
    const spec = {
      options: [
        {
          name: '-p',
          args: {
            generators: [cargoMatchingGenerator()],
          },
        },
      ],
    };
    const stats = { rewrites: 0 };
    rewriteGenerators(spec, RECOGNIZERS.cargo, stats);

    assert.equal(stats.rewrites, 1);
    assert.deepStrictEqual(spec.options[0].args.generators[0], {
      type: 'cargo_workspace_members',
      cache: { ttl_seconds: 30 },
    });
  });
});

describe('rewriteGenerators — recursive descent', () => {
  it('descends into nested subcommands[].args[].generators (the typical fig spec shape)', () => {
    // Three layers of nesting plus an `args` array (some fig specs use
    // arrays of args, others use a single object) — both shapes must work.
    const spec = {
      subcommands: [
        {
          name: 'outer',
          subcommands: [
            {
              name: 'inner',
              args: [
                {
                  name: 'target',
                  generators: [makeMatchingGenerator()],
                },
              ],
            },
          ],
        },
      ],
    };
    const stats = { rewrites: 0 };
    rewriteGenerators(spec, RECOGNIZERS.make, stats);
    assert.equal(stats.rewrites, 1);
    assert.deepStrictEqual(
      spec.subcommands[0].subcommands[0].args[0].generators[0],
      { type: 'makefile_targets', cache: { ttl_seconds: 5 } },
    );
  });

  it('rewrites multiple matching generators across the tree', () => {
    const spec = {
      subcommands: [
        { name: 'a', args: { generators: [makeMatchingGenerator()] } },
        { name: 'b', args: { generators: [makeMatchingGenerator()] } },
      ],
      args: { generators: [makeMatchingGenerator()] },
    };
    const stats = { rewrites: 0 };
    rewriteGenerators(spec, RECOGNIZERS.make, stats);
    assert.equal(stats.rewrites, 3);
  });
});

describe('rewriteGenerators — pass-through for non-matching generators', () => {
  it('returns the original generator object identity when no recognizer matches', () => {
    const original = nonMatchingGenerator();
    const spec = { args: { generators: [original] } };
    const stats = { rewrites: 0 };
    rewriteGenerators(spec, RECOGNIZERS.make, stats);

    assert.equal(stats.rewrites, 0);
    // Object identity — not just deep-equal — survives.
    assert.equal(spec.args.generators[0], original);
  });

  it('mixes matching and non-matching generators in the same array, leaving non-matching identical', () => {
    const original = nonMatchingGenerator();
    const spec = {
      args: {
        generators: [makeMatchingGenerator(), original],
      },
    };
    const stats = { rewrites: 0 };
    rewriteGenerators(spec, RECOGNIZERS.make, stats);
    assert.equal(stats.rewrites, 1);
    assert.deepStrictEqual(spec.args.generators[0], {
      type: 'makefile_targets',
      cache: { ttl_seconds: 5 },
    });
    assert.equal(spec.args.generators[1], original);
  });

  it('npm recognizer does not match a make-shaped generator', () => {
    const spec = { args: { generators: [makeMatchingGenerator()] } };
    const stats = { rewrites: 0 };
    rewriteGenerators(spec, RECOGNIZERS.npm, stats);
    assert.equal(stats.rewrites, 0);
  });

  it('cargo recognizer does not match an npm-shaped generator', () => {
    const spec = { args: { generators: [npmMatchingGenerator()] } };
    const stats = { rewrites: 0 };
    rewriteGenerators(spec, RECOGNIZERS.cargo, stats);
    assert.equal(stats.rewrites, 0);
  });

  it('cargo recognizer does not rewrite dependency-graph package generators', () => {
    const original = cargoFullMetadataPackageGenerator();
    const spec = { args: { generators: [original] } };
    const stats = { rewrites: 0 };
    rewriteGenerators(spec, RECOGNIZERS.cargo, stats);

    assert.equal(stats.rewrites, 0);
    assert.equal(spec.args.generators[0], original);
  });

  it('cargo recognizer does not rewrite dependency-list generators', () => {
    const original = cargoDependencyGenerator();
    const spec = { args: { generators: [original] } };
    const stats = { rewrites: 0 };
    rewriteGenerators(spec, RECOGNIZERS.cargo, stats);

    assert.equal(stats.rewrites, 0);
    assert.equal(spec.args.generators[0], original);
  });
});

describe('rewriteGenerators — idempotency', () => {
  it('a second pass over an already-patched spec produces zero rewrites', () => {
    const spec = {
      subcommands: [
        {
          args: {
            priority: 80,
            generators: [makeMatchingGenerator()],
          },
        },
      ],
    };

    const first = { rewrites: 0 };
    rewriteGenerators(spec, RECOGNIZERS.make, first);
    assert.equal(first.rewrites, 1);

    const second = { rewrites: 0 };
    rewriteGenerators(spec, RECOGNIZERS.make, second);
    assert.equal(second.rewrites, 0);

    // And the priority sibling field is still intact after both passes.
    assert.equal(spec.subcommands[0].args.priority, 80);
    assert.deepStrictEqual(spec.subcommands[0].args.generators[0], {
      type: 'makefile_targets',
      cache: { ttl_seconds: 5 },
    });
  });

  it('idempotent for npm and cargo as well', () => {
    const npmSpec = { args: { generators: [npmMatchingGenerator()] } };
    const cargoSpec = { args: { generators: [cargoMatchingGenerator()] } };

    const a = { rewrites: 0 };
    rewriteGenerators(npmSpec, RECOGNIZERS.npm, a);
    assert.equal(a.rewrites, 1);
    const b = { rewrites: 0 };
    rewriteGenerators(npmSpec, RECOGNIZERS.npm, b);
    assert.equal(b.rewrites, 0);

    const c = { rewrites: 0 };
    rewriteGenerators(cargoSpec, RECOGNIZERS.cargo, c);
    assert.equal(c.rewrites, 1);
    const d = { rewrites: 0 };
    rewriteGenerators(cargoSpec, RECOGNIZERS.cargo, d);
    assert.equal(d.rewrites, 0);
  });
});

describe('rewriteGenerators — null/primitive safety', () => {
  it('handles null and primitive nodes without throwing', () => {
    const stats = { rewrites: 0 };
    rewriteGenerators(null, RECOGNIZERS.make, stats);
    rewriteGenerators(undefined, RECOGNIZERS.make, stats);
    rewriteGenerators(42, RECOGNIZERS.make, stats);
    rewriteGenerators('string', RECOGNIZERS.make, stats);
    assert.equal(stats.rewrites, 0);
  });

  it('handles a top-level array of nodes', () => {
    const arr = [
      { generators: [makeMatchingGenerator()] },
      { generators: [makeMatchingGenerator()] },
    ];
    const stats = { rewrites: 0 };
    rewriteGenerators(arr, RECOGNIZERS.make, stats);
    assert.equal(stats.rewrites, 2);
  });
});

// Current specs store post_process bodies at `js_runtime.source`; only
// specs from older converter releases carry the legacy `js_source`.
const PACKAGE_JSON_WALKER = [
  'bash',
  '-c',
  "until [[ -f package.json ]] || [[ $PWD = '/' ]]; do cd ..; done; cat package.json",
];
const SCRIPTS_EXTRACTOR = 'function(e,[n]){if(e.trim()=="")return[];try{let t=JSON.parse(e),i=t.scripts,a=t.fig||{};if(i)return Object.entries(i).map(([r,s])=>({name:r,description:s}))}catch(t){console.error(t)}return[]}';
const DEPENDENCIES_EXTRACTOR = 'function(e,n=[]){if(e.trim()==="")return[];try{let t=JSON.parse(e),i=t.dependencies??{};return Object.keys(i).filter(s=>!n.some(l=>l===s)).map(s=>({name:s}))}catch(t){return console.error(t),[]}}';

function jsRuntimeGenerator(source) {
  return {
    cache: { cache_by_directory: true },
    js_runtime: { kind: 'post_process', source },
    requires_js: true,
    script: [...PACKAGE_JSON_WALKER],
  };
}

describe('rewriteGenerators — js_runtime.source generators', () => {
  it('npm recognizer reads js_runtime.source, not only the legacy js_source', () => {
    const spec = { args: { generators: [jsRuntimeGenerator(SCRIPTS_EXTRACTOR)] } };
    const stats = { rewrites: 0 };
    rewriteGenerators(spec, RECOGNIZERS.npm, stats);
    assert.equal(stats.rewrites, 1);
    assert.deepStrictEqual(spec.args.generators[0], {
      cache: { cache_by_directory: true },
      type: 'npm_scripts',
    });
  });

  for (const specName of ['pnpm', 'yarn', 'bun', 'nr', 'rushx', 'meteor']) {
    it(`${specName}: rewrites the package.json scripts extractor to npm_scripts`, () => {
      const spec = {
        args: {
          generators: [jsRuntimeGenerator(SCRIPTS_EXTRACTOR)],
          isVariadic: true,
          name: 'Scripts',
        },
      };
      const stats = { rewrites: 0 };
      rewriteGenerators(spec, RECOGNIZERS[specName], stats);

      assert.equal(stats.rewrites, 1);
      // Byte-level shape, key order included: it must serialize exactly
      // like the converter's `npm run` output in specs/npm.json.
      assert.equal(
        JSON.stringify(spec),
        JSON.stringify({
          args: {
            generators: [{ cache: { cache_by_directory: true }, type: 'npm_scripts' }],
            isVariadic: true,
            name: 'Scripts',
          },
        }),
      );
    });

    it(`${specName}: leaves the package.json dependency extractor alone`, () => {
      const original = jsRuntimeGenerator(DEPENDENCIES_EXTRACTOR);
      const spec = { args: { generators: [original] } };
      const stats = { rewrites: 0 };
      rewriteGenerators(spec, RECOGNIZERS[specName], stats);

      assert.equal(stats.rewrites, 0);
      assert.equal(spec.args.generators[0], original);
    });
  }
});
