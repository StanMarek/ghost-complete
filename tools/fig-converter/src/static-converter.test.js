import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { convertSpec } from './static-converter.js';

describe('convertSpec', () => {
  it('converts a minimal spec with name and description', () => {
    const result = convertSpec({
      name: 'mycommand',
      description: 'A test command',
    });
    assert.deepStrictEqual(result, {
      name: 'mycommand',
      description: 'A test command',
    });
  });

  it('throws on invalid input', () => {
    assert.throws(() => convertSpec(null), /Invalid spec/);
    assert.throws(() => convertSpec('string'), /Invalid spec/);
  });

  it('normalizes array name to first element', () => {
    const result = convertSpec({
      name: ['mycommand', 'mc'],
      description: 'Test',
    });
    assert.equal(result.name, 'mycommand');
  });

  it('converts subcommands recursively', () => {
    const result = convertSpec({
      name: 'git',
      description: 'VCS',
      subcommands: [
        {
          name: 'remote',
          description: 'Manage remotes',
          subcommands: [
            {
              name: 'add',
              description: 'Add a remote',
              args: [
                { name: 'name', description: 'Remote name' },
                { name: 'url', description: 'Remote URL' },
              ],
            },
          ],
        },
      ],
    });
    assert.equal(result.subcommands.length, 1);
    assert.equal(result.subcommands[0].name, 'remote');
    assert.equal(result.subcommands[0].subcommands.length, 1);
    assert.equal(result.subcommands[0].subcommands[0].name, 'add');
    assert.equal(result.subcommands[0].subcommands[0].args.length, 2);
  });

  it('converts options with array names', () => {
    const result = convertSpec({
      name: 'test',
      options: [
        { name: ['-v', '--verbose'], description: 'Verbose output' },
        { name: '--quiet', description: 'Quiet mode' },
      ],
    });
    assert.deepStrictEqual(result.options[0].name, ['-v', '--verbose']);
    assert.deepStrictEqual(result.options[1].name, ['--quiet']);
  });

  it('converts option with args', () => {
    const result = convertSpec({
      name: 'test',
      options: [
        {
          name: '--output',
          description: 'Output file',
          args: { name: 'path', template: 'filepaths' },
        },
      ],
    });
    assert.equal(result.options[0].args.name, 'path');
    assert.equal(result.options[0].args.template, 'filepaths');
  });

  it('converts template field on args', () => {
    const result = convertSpec({
      name: 'ls',
      args: { name: 'path', template: ['filepaths', 'folders'] },
    });
    assert.deepStrictEqual(result.args.template, ['filepaths', 'folders']);
  });

  it('converts args as single object (not wrapped in array)', () => {
    const result = convertSpec({
      name: 'cat',
      args: { name: 'file', template: 'filepaths' },
    });
    // Single arg should stay as single object, not be wrapped in array
    assert.equal(result.args.name, 'file');
    assert.equal(result.args.template, 'filepaths');
  });

  it('converts args as array', () => {
    const result = convertSpec({
      name: 'cp',
      args: [
        { name: 'source', template: 'filepaths' },
        { name: 'dest', template: 'filepaths' },
      ],
    });
    assert.equal(result.args.length, 2);
    assert.equal(result.args[0].name, 'source');
    assert.equal(result.args[1].name, 'dest');
  });

  it('preserves isVariadic and isOptional on args', () => {
    const result = convertSpec({
      name: 'echo',
      args: { name: 'text', isVariadic: true, isOptional: true },
    });
    assert.equal(result.args.isVariadic, true);
    assert.equal(result.args.isOptional, true);
  });

  it('converts static suggestions', () => {
    const result = convertSpec({
      name: 'test',
      args: {
        name: 'level',
        suggestions: [
          'debug',
          { name: 'info', description: 'Information level' },
          { name: ['warn', 'warning'], description: 'Warning level' },
        ],
      },
    });
    assert.deepStrictEqual(result.args.suggestions, [
      { name: 'debug' },
      { name: 'info', description: 'Information level' },
      { name: 'warn', description: 'Warning level' },
    ]);
  });

  it('converts generators with template', () => {
    const result = convertSpec({
      name: 'test',
      args: {
        name: 'file',
        generators: { template: 'filepaths' },
      },
    });
    assert.deepStrictEqual(result.args.generators, [
      { template: 'filepaths' },
    ]);
  });

  it('converts generators with script array', () => {
    const result = convertSpec({
      name: 'test',
      args: {
        name: 'pkg',
        generators: {
          script: ['brew', 'list', '-1'],
          postProcess: (out) => out.split('\n'),
        },
      },
    });
    assert.deepStrictEqual(result.args.generators[0].script, [
      'brew',
      'list',
      '-1',
    ]);
    assert.equal(typeof result.args.generators[0]._postProcessSource, 'string');
  });

  it('converts generators with script string (splits on spaces)', () => {
    const result = convertSpec({
      name: 'test',
      args: {
        name: 'branch',
        generators: {
          script: 'git branch --no-color',
          postProcess: (out) => out.split('\n'),
        },
      },
    });
    assert.deepStrictEqual(result.args.generators[0].script, [
      'git',
      'branch',
      '--no-color',
    ]);
  });

  it('marks script-as-function generators', () => {
    const result = convertSpec({
      name: 'test',
      args: {
        name: 'thing',
        generators: {
          script: (tokens) => ['cmd', tokens[0]],
          postProcess: (out) => out.split('\n'),
        },
      },
    });
    assert.equal(result.args.generators[0]._scriptFunction, true);
    assert.equal(typeof result.args.generators[0]._scriptSource, 'string');
  });

  it('marks custom generators', () => {
    const result = convertSpec({
      name: 'test',
      args: {
        name: 'thing',
        generators: {
          custom: async (tokens, exec) => [],
        },
      },
    });
    assert.equal(result.args.generators[0]._custom, true);
  });

  it('converts splitOn generators', () => {
    const result = convertSpec({
      name: 'test',
      args: {
        name: 'item',
        generators: {
          script: ['some', 'cmd'],
          splitOn: '\n',
        },
      },
    });
    assert.deepStrictEqual(result.args.generators[0].script, ['some', 'cmd']);
    assert.equal(result.args.generators[0]._splitOn, '\n');
  });

  it('converts cache configuration', () => {
    const result = convertSpec({
      name: 'test',
      args: {
        name: 'item',
        generators: {
          script: ['brew', 'list'],
          cache: { ttl: 60000, cacheByDirectory: true },
          postProcess: (out) => out.split('\n'),
        },
      },
    });
    assert.deepStrictEqual(result.args.generators[0].cache, {
      ttl_seconds: 60,
      cache_by_directory: true,
    });
  });

  it('preserves loadSpec reference on subcommands', () => {
    const result = convertSpec({
      name: 'aws',
      subcommands: [
        {
          name: 's3',
          description: 'S3 commands',
          loadSpec: 'aws/s3',
        },
      ],
    });
    assert.equal(result.subcommands[0]._loadSpec, 'aws/s3');
  });

  it('strips icons from suggestions', () => {
    const result = convertSpec({
      name: 'test',
      args: {
        name: 'item',
        suggestions: [
          { name: 'foo', description: 'A foo', icon: 'fig://icon?type=box' },
        ],
      },
    });
    assert.equal(result.args.suggestions[0].icon, undefined);
  });

  it('filters out null subcommands/options', () => {
    const result = convertSpec({
      name: 'test',
      subcommands: [null, undefined, { name: 'valid' }],
      options: [null, { name: '--flag', description: 'ok' }],
    });
    assert.equal(result.subcommands.length, 1);
    assert.equal(result.options.length, 1);
  });

  it('handles multiple generators on a single arg', () => {
    const result = convertSpec({
      name: 'test',
      args: {
        name: 'item',
        generators: [
          { script: ['brew', 'formulae'], postProcess: (out) => out.split('\n') },
          { script: ['brew', 'casks'], postProcess: (out) => out.split('\n') },
        ],
      },
    });
    assert.equal(result.args.generators.length, 2);
    assert.deepStrictEqual(result.args.generators[0].script, ['brew', 'formulae']);
    assert.deepStrictEqual(result.args.generators[1].script, ['brew', 'casks']);
  });
});
