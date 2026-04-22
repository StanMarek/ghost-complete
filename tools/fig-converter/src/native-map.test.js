import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { matchNativeGenerator } from './native-map.js';

// Real postProcess JS source strings taken from specs/arduino-cli.json (the
// pre-regen output). Used as fixtures for the arduino-cli disambiguation tests.
const ARDUINO_FQBN_POSTPROCESS = "t=>{try{return JSON.parse(t).filter(i=>i.matching_boards).map(i=>({name:i.matching_boards[0].fqbn,description:`${i.matching_boards[0].name} on port ${i.port.address}`}))}catch{return[]}}";
const ARDUINO_PORT_POSTPROCESS = "t=>{try{return JSON.parse(t).filter(i=>i.matching_boards).map(i=>({name:i.port.address,description:`${i.matching_boards[0].name} port connection`}))}catch{return[]}}";

describe('matchNativeGenerator', () => {
  it('maps git branch to git_branches', () => {
    const result = matchNativeGenerator('git', ['git', 'branch', '--no-color']);
    assert.deepStrictEqual(result, { type: 'git_branches' });
  });

  it('maps git tag to git_tags', () => {
    const result = matchNativeGenerator('git', ['git', 'tag']);
    assert.deepStrictEqual(result, { type: 'git_tags' });
  });

  it('maps git remote to git_remotes', () => {
    const result = matchNativeGenerator('git', ['git', 'remote']);
    assert.deepStrictEqual(result, { type: 'git_remotes' });
  });

  it('returns null for unmapped commands', () => {
    const result = matchNativeGenerator('brew', ['brew', 'list', '-1']);
    assert.equal(result, null);
  });

  it('returns null for empty/invalid input', () => {
    assert.equal(matchNativeGenerator('test', []), null);
    assert.equal(matchNativeGenerator('test', null), null);
    assert.equal(matchNativeGenerator('test', ['git']), null);
  });

  it('matches only first two elements of script array', () => {
    // git branch with extra flags should still match
    const result = matchNativeGenerator('git', [
      'git', 'branch', '--no-color', '--sort=-committerdate',
    ]);
    assert.deepStrictEqual(result, { type: 'git_branches' });
  });

  it('does not match partial commands', () => {
    // 'git branches' is not 'git branch'
    assert.equal(matchNativeGenerator('git', ['git', 'branches']), null);
  });

  it('maps multipass list to multipass_list', () => {
    const result = matchNativeGenerator('multipass', ['multipass', 'list', '--format', 'json']);
    assert.deepStrictEqual(result, { type: 'multipass_list' });
  });

  it('maps defaults domains to defaults_domains', () => {
    const result = matchNativeGenerator('defaults', ['defaults', 'domains']);
    assert.deepStrictEqual(result, { type: 'defaults_domains' });
  });

  it('maps pandoc --list-input-formats to pandoc_input_formats', () => {
    const result = matchNativeGenerator('pandoc', ['pandoc', '--list-input-formats']);
    assert.deepStrictEqual(result, { type: 'pandoc_input_formats' });
  });

  it('maps pandoc --list-output-formats to pandoc_output_formats', () => {
    const result = matchNativeGenerator('pandoc', ['pandoc', '--list-output-formats']);
    assert.deepStrictEqual(result, { type: 'pandoc_output_formats' });
  });

  it('maps ansible-doc --list to ansible_doc_modules', () => {
    // Short form: just `ansible-doc --list`.
    assert.deepStrictEqual(
      matchNativeGenerator('ansible-doc', ['ansible-doc', '--list']),
      { type: 'ansible_doc_modules' },
    );
    // Long form with --json: key is still built from the first two elements.
    assert.deepStrictEqual(
      matchNativeGenerator('ansible-doc', ['ansible-doc', '--list', '--json']),
      { type: 'ansible_doc_modules' },
    );
  });

  it('maps conda env list to mamba_envs only for mamba spec', () => {
    // In the mamba spec, `conda env list` routes to our mamba_envs provider.
    assert.deepStrictEqual(
      matchNativeGenerator('mamba', ['conda', 'env', 'list']),
      { type: 'mamba_envs' },
    );
    // In the conda spec, the same script stays unmapped (no conda_envs provider yet).
    assert.equal(
      matchNativeGenerator('conda', ['conda', 'env', 'list']),
      null,
    );
  });

  it('maps arduino-cli board list to arduino_cli_boards when postProcess is fqbn-extracting', () => {
    const result = matchNativeGenerator(
      'arduino-cli',
      ['arduino-cli', 'board', 'list', '--format', 'json'],
      ARDUINO_FQBN_POSTPROCESS,
    );
    assert.deepStrictEqual(result, { type: 'arduino_cli_boards' });
  });

  it('maps arduino-cli board list to arduino_cli_ports when postProcess is port-extracting', () => {
    const result = matchNativeGenerator(
      'arduino-cli',
      ['arduino-cli', 'board', 'list', '--format', 'json'],
      ARDUINO_PORT_POSTPROCESS,
    );
    assert.deepStrictEqual(result, { type: 'arduino_cli_ports' });
  });

  it('returns null for arduino-cli board with missing or unrecognized postProcess', () => {
    // No postProcess at all — can't disambiguate, return null rather than guess.
    assert.equal(
      matchNativeGenerator('arduino-cli', ['arduino-cli', 'board', 'list', '--format', 'json']),
      null,
    );
    // Unrelated postProcess (doesn't match either the fqbn or port pattern).
    assert.equal(
      matchNativeGenerator(
        'arduino-cli',
        ['arduino-cli', 'board', 'list', '--format', 'json'],
        't => t.split("\\n")',
      ),
      null,
    );
  });

  it('new third argument does not affect existing git mappings', () => {
    // Regression check: passing an extraneous postProcess source for a non-arduino
    // key should not interfere with the normal lookup.
    const result = matchNativeGenerator('git', ['git', 'branch'], 'some js source');
    assert.deepStrictEqual(result, { type: 'git_branches' });
  });

  it('strips git --no-optional-locks prefix before matching', () => {
    // Upstream fig git spec uses `["git", "--no-optional-locks", "branch", ...]`
    // as the canonical script prefix (avoids touching index.lock). Without
    // stripping this driver-level no-op, every git generator would miss the
    // native map. See docs/phase-minus-1-followups.md §1 for the deferred
    // bug this fix closes.
    assert.deepStrictEqual(
      matchNativeGenerator(
        'git',
        ['git', '--no-optional-locks', 'branch', '-a', '--no-color', '--sort=-committerdate'],
      ),
      { type: 'git_branches' },
    );
    assert.deepStrictEqual(
      matchNativeGenerator('git', ['git', '--no-optional-locks', 'tag', '--list']),
      { type: 'git_tags' },
    );
    assert.deepStrictEqual(
      matchNativeGenerator('git', ['git', '--no-optional-locks', 'remote']),
      { type: 'git_remotes' },
    );
  });

  it('only strips no-op flags for their driver command', () => {
    // `--no-optional-locks` is a git-specific no-op. If another command happens
    // to use a literal argv element of that name, we must NOT skip over it.
    assert.equal(
      matchNativeGenerator('other', ['other', '--no-optional-locks', 'branch']),
      null,
    );
  });
});
