import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { qualifyCommand } from '../scripts/run-spike.mjs';

// Regression suite for the authKeywords heuristic in qualifyCommand.
// See spike-report.md §9 question 3 — the pre-extension list missed
// brand-only command names like `flyctl`, `firebase`, `pulumi`, etc.
describe('qualifyCommand — authKeywords heuristic', () => {
  it('flags flyctl as auth-requiring (brand match on command name)', () => {
    const result = qualifyCommand('flyctl', [
      { gen: { script: 'fly auth list' }, verdict: 'existing_transforms' },
    ]);
    assert.equal(result.qualification.no_auth, false);
  });

  it('flags firebase as auth-requiring (brand match on command name)', () => {
    const result = qualifyCommand('firebase', [
      { gen: { script: 'firebase deploy' }, verdict: 'existing_transforms' },
    ]);
    assert.equal(result.qualification.no_auth, false);
  });

  it('flags pulumi as auth-requiring (brand match on command name)', () => {
    const result = qualifyCommand('pulumi', [
      { gen: { script: 'pulumi stack ls' }, verdict: 'existing_transforms' },
    ]);
    assert.equal(result.qualification.no_auth, false);
  });

  it('does NOT flag elm as auth-requiring (regression: keyword set is not over-eager)', () => {
    const result = qualifyCommand('elm', [
      { gen: { script: 'elm make --list' }, verdict: 'existing_transforms' },
    ]);
    assert.equal(result.qualification.no_auth, true);
  });

  it('flags tsh as auth-requiring (Teleport login required)', () => {
    const result = qualifyCommand('tsh', [
      { gen: { script: 'tsh ls --format=json' }, verdict: 'existing_transforms' },
    ]);
    assert.equal(result.qualification.no_auth, false);
  });

  it('flags bosh as auth-requiring (Cloud Foundry director auth)', () => {
    const result = qualifyCommand('bosh', [
      { gen: { script: 'bosh --json deployments' }, verdict: 'existing_transforms' },
    ]);
    assert.equal(result.qualification.no_auth, false);
  });
});
