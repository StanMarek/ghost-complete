import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import {
  qualifyCommand,
  assignVerdict,
  deriveSlugCandidate,
  assignSlugs,
  composeBucketKey,
} from '../scripts/run-spike.mjs';

const ZERO_SHAPE = {
  fingerprint: '',
  has_json_parse: false,
  has_regex_match: false,
  has_substring_or_slice: false,
  has_conditional: false,
  has_await: false,
};

function shape(overrides = {}) {
  return { ...ZERO_SHAPE, ...overrides };
}

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

describe('assignVerdict', () => {
  it('parse_error overrides every other signal', () => {
    const s = shape({
      has_await: true,
      has_json_parse: true,
      has_regex_match: true,
      has_substring_or_slice: true,
      has_conditional: true,
    });
    assert.equal(assignVerdict(s, true, true), 'hand_audit_required');
    assert.equal(assignVerdict(s, false, true), 'hand_audit_required');
  });

  it('fig_api_refs overrides shape flags', () => {
    const s = shape({ has_await: true, has_json_parse: true });
    assert.equal(assignVerdict(s, true, false), 'hand_audit_required');
  });

  it('has_await yields requires_runtime when no higher-priority flags match', () => {
    const s = shape({ has_await: true, has_json_parse: true, has_regex_match: true });
    assert.equal(assignVerdict(s, false, false), 'requires_runtime');
  });

  it('JSON.parse with a single .PROP hop yields existing_transforms', () => {
    const s = shape({
      has_json_parse: true,
      fingerprint: 'JSON.parse(...).PROP',
    });
    assert.equal(assignVerdict(s, false, false), 'existing_transforms');
  });

  it('JSON.parse with 2+ .PROP hops yields needs_dotted_path_json_extract', () => {
    const s = shape({
      has_json_parse: true,
      fingerprint: 'JSON.parse(...).PROP.PROP',
    });
    assert.equal(assignVerdict(s, false, false), 'needs_dotted_path_json_extract');
  });

  it('JSON.parse with 3 .PROP hops also yields needs_dotted_path_json_extract', () => {
    const s = shape({
      has_json_parse: true,
      fingerprint: 'JSON.parse(...).PROP.PROP.PROP',
    });
    assert.equal(assignVerdict(s, false, false), 'needs_dotted_path_json_extract');
  });

  it('pure line-oriented pipeline (all shape booleans false) yields existing_transforms', () => {
    const s = shape({ fingerprint: '.split(STR).filter(FN).map(FN)' });
    assert.equal(assignVerdict(s, false, false), 'existing_transforms');
  });

  it('has_regex_match yields needs_new_transform_regex_match', () => {
    const s = shape({ has_regex_match: true });
    assert.equal(assignVerdict(s, false, false), 'needs_new_transform_regex_match');
  });

  it('has_substring_or_slice yields needs_new_transform_substring_slice', () => {
    const s = shape({ has_substring_or_slice: true });
    assert.equal(assignVerdict(s, false, false), 'needs_new_transform_substring_slice');
  });

  it('has_conditional yields needs_new_transform_conditional_split', () => {
    const s = shape({ has_conditional: true });
    assert.equal(assignVerdict(s, false, false), 'needs_new_transform_conditional_split');
  });

  it('regex_match wins over substring_or_slice when both present', () => {
    const s = shape({ has_regex_match: true, has_substring_or_slice: true });
    assert.equal(assignVerdict(s, false, false), 'needs_new_transform_regex_match');
  });

  it('substring_or_slice wins over conditional when both present (no regex)', () => {
    const s = shape({ has_substring_or_slice: true, has_conditional: true });
    assert.equal(assignVerdict(s, false, false), 'needs_new_transform_substring_slice');
  });
});

describe('deriveSlugCandidate', () => {
  it('returns "empty" for the empty fingerprint', () => {
    assert.equal(deriveSlugCandidate(''), 'empty');
  });

  it('returns "parse-error" for the parse-error sentinel', () => {
    assert.equal(deriveSlugCandidate('<parse_error>'), 'parse-error');
  });

  it('extracts and dedups method-chain names into a hyphenated slug', () => {
    assert.equal(
      deriveSlugCandidate('.split(STR).map(FN).filter(FN).map(FN)'),
      'split-map-filter',
    );
  });

  it('falls back to alnum slug when no method calls are present', () => {
    const slug = deriveSlugCandidate('JSON.parse.PROP.PROP');
    assert.match(slug, /^[a-z0-9-]+$/);
    assert.notEqual(slug, 'empty');
    assert.notEqual(slug, 'parse-error');
  });

  it('returns "unknown" when the fingerprint contains no usable chars', () => {
    assert.equal(deriveSlugCandidate('!!!'), 'unknown');
  });
});

describe('assignSlugs', () => {
  it('keeps distinct base slugs unchanged across buckets', () => {
    const buckets = new Map([
      ['k1', { fingerprint: '.split(STR).map(FN)', hasFigApiRefs: false, count: 10 }],
      ['k2', { fingerprint: '.filter(FN).map(FN)', hasFigApiRefs: false, count: 5 }],
    ]);
    const slugs = assignSlugs(buckets);
    assert.equal(slugs.get('k1'), 'split-map');
    assert.equal(slugs.get('k2'), 'filter-map');
  });

  it('appends -2 when a second bucket derives the same base slug', () => {
    const buckets = new Map([
      ['k1', { fingerprint: '.split(STR).map(FN)', hasFigApiRefs: false, count: 20 }],
      ['k2', { fingerprint: '.split(NL).map(CB)', hasFigApiRefs: false, count: 10 }],
    ]);
    const slugs = assignSlugs(buckets);
    assert.equal(slugs.get('k1'), 'split-map');
    assert.equal(slugs.get('k2'), 'split-map-2');
  });

  it('appends -3 when three buckets share the same base slug', () => {
    const buckets = new Map([
      ['k1', { fingerprint: '.split(A).map(B)', hasFigApiRefs: false, count: 30 }],
      ['k2', { fingerprint: '.split(C).map(D)', hasFigApiRefs: false, count: 20 }],
      ['k3', { fingerprint: '.split(E).map(F)', hasFigApiRefs: false, count: 10 }],
    ]);
    const slugs = assignSlugs(buckets);
    assert.equal(slugs.get('k1'), 'split-map');
    assert.equal(slugs.get('k2'), 'split-map-2');
    assert.equal(slugs.get('k3'), 'split-map-3');
  });

  it('applies the with-fig-refs suffix to fig-api variants before numeric collision suffixes', () => {
    const buckets = new Map([
      ['plain-a', { fingerprint: '.split(STR).map(FN)', hasFigApiRefs: false, count: 20 }],
      ['refs-a', { fingerprint: '.split(X).map(Y)', hasFigApiRefs: true, count: 15 }],
      ['plain-b', { fingerprint: '.split(U).map(V)', hasFigApiRefs: false, count: 10 }],
    ]);
    const slugs = assignSlugs(buckets);
    assert.equal(slugs.get('plain-a'), 'split-map');
    assert.equal(slugs.get('refs-a'), 'split-map-with-fig-refs');
    assert.equal(slugs.get('plain-b'), 'split-map-2');
  });

  it('numeric collision applies per variant independently', () => {
    const buckets = new Map([
      ['refs-a', { fingerprint: '.split(A).map(B)', hasFigApiRefs: true, count: 40 }],
      ['refs-b', { fingerprint: '.split(C).map(D)', hasFigApiRefs: true, count: 30 }],
      ['plain-a', { fingerprint: '.split(E).map(F)', hasFigApiRefs: false, count: 20 }],
      ['plain-b', { fingerprint: '.split(G).map(H)', hasFigApiRefs: false, count: 10 }],
    ]);
    const slugs = assignSlugs(buckets);
    assert.equal(slugs.get('refs-a'), 'split-map-with-fig-refs');
    assert.equal(slugs.get('refs-b'), 'split-map-with-fig-refs-2');
    assert.equal(slugs.get('plain-a'), 'split-map');
    assert.equal(slugs.get('plain-b'), 'split-map-2');
  });
});

describe('composeBucketKey', () => {
  it('parse_error collapses into a single fixed key regardless of shape flags', () => {
    const k1 = composeBucketKey('<parse_error>', shape({
      has_json_parse: true,
      has_await: true,
      has_regex_match: true,
    }), true);
    const k2 = composeBucketKey('<parse_error>', shape({
      has_conditional: true,
      has_substring_or_slice: true,
    }), false);
    const k3 = composeBucketKey('<parse_error>', ZERO_SHAPE, false);
    assert.equal(k1, '<parse_error>|false');
    assert.equal(k2, '<parse_error>|false');
    assert.equal(k3, '<parse_error>|false');
  });

  it('distinct shape-flag combinations produce distinct keys for the same fingerprint', () => {
    const fp = '.split(STR).map(FN)';
    const k1 = composeBucketKey(fp, shape({ has_await: true }), false);
    const k2 = composeBucketKey(fp, shape({ has_await: false }), false);
    assert.notEqual(k1, k2);
  });

  it('has_fig_api_refs is part of the key for non-parse-error fingerprints', () => {
    const fp = '.split(STR).map(FN)';
    const withRefs = composeBucketKey(fp, ZERO_SHAPE, true);
    const withoutRefs = composeBucketKey(fp, ZERO_SHAPE, false);
    assert.notEqual(withRefs, withoutRefs);
  });

  it('same fingerprint + same flags yields identical keys', () => {
    const fp = 'JSON.parse(...).PROP.PROP';
    const s = shape({ has_json_parse: true });
    assert.equal(
      composeBucketKey(fp, s, false),
      composeBucketKey(fp, s, false),
    );
  });
});
