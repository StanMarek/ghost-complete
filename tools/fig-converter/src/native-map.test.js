import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { matchNativeGenerator } from './native-map.js';

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
});
