import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { matchPostProcess } from './post-process-matcher.js';

describe('matchPostProcess', () => {
  describe('split by newline patterns', () => {
    it('matches simple split + map to name', () => {
      const fn = `function(out) { return out.split("\\n").map(line => ({ name: line })) }`;
      const result = matchPostProcess(fn);
      assert.equal(result.requires_js, false);
      assert.deepStrictEqual(result.transforms, [
        'split_lines',
        'filter_empty',
      ]);
    });

    it('matches arrow function split + map', () => {
      const fn = `(out) => out.split("\\n").map(e => ({ name: e }))`;
      const result = matchPostProcess(fn);
      assert.equal(result.requires_js, false);
      assert.deepStrictEqual(result.transforms, [
        'split_lines',
        'filter_empty',
      ]);
    });

    it('matches real Fig brew postProcess (minified)', () => {
      const fn = 'function(a){return a.split(`\n`).filter(e=>!e.includes("=")).map(e=>({name:e,icon:"\\u{1F37A}",description:"Installed formula"}))}';
      const result = matchPostProcess(fn);
      assert.equal(result.requires_js, false);
      assert.ok(result.transforms.includes('split_lines'));
      assert.ok(result.transforms.includes('filter_empty'));
    });
  });

  describe('split + filter patterns', () => {
    it('matches split + filter(Boolean)', () => {
      const fn = `(out) => out.split("\\n").filter(Boolean).map(x => ({ name: x }))`;
      const result = matchPostProcess(fn);
      assert.equal(result.requires_js, false);
      assert.deepStrictEqual(result.transforms, [
        'split_lines',
        'filter_empty',
      ]);
    });
  });

  describe('split + trim patterns', () => {
    it('includes trim when .trim() is present', () => {
      const fn = `(out) => out.split("\\n").map(l => l.trim()).filter(Boolean).map(l => ({ name: l }))`;
      const result = matchPostProcess(fn);
      assert.equal(result.requires_js, false);
      assert.ok(result.transforms.includes('trim'));
    });
  });

  describe('JSON.parse extraction', () => {
    it('matches split + JSON.parse with field access', () => {
      const fn = `(out) => out.split("\\n").filter(Boolean).map(line => { const obj = JSON.parse(line); return { name: obj.Name }; })`;
      const result = matchPostProcess(fn);
      assert.equal(result.requires_js, false);
      assert.ok(result.transforms.some(
        t => typeof t === 'object' && t.type === 'json_extract' && t.name === 'Name'
      ));
    });

    it('matches JSON.parse with bracket access', () => {
      const fn = `(out) => out.split("\\n").filter(Boolean).map(line => ({ name: JSON.parse(line)["ID"] }))`;
      const result = matchPostProcess(fn);
      assert.equal(result.requires_js, false);
      assert.ok(result.transforms.some(
        t => typeof t === 'object' && t.type === 'json_extract' && t.name === 'ID'
      ));
    });

    it('uses internally-tagged format for json_extract', () => {
      const fn = `(out) => out.split("\\n").map(l => ({ name: JSON.parse(l).Name }))`;
      const result = matchPostProcess(fn);
      const jsonTransform = result.transforms.find(t => typeof t === 'object');
      assert.equal(jsonTransform.type, 'json_extract');
      assert.equal(jsonTransform.name, 'Name');
      assert.equal(jsonTransform.json_extract, undefined);
    });
  });

  describe('regex extraction', () => {
    it('matches split + regex match with capture group', () => {
      const fn = String.raw`(out) => out.split("\n").map(line => { const m = line.match(/^(\S+)\s+(.*)/); return { name: m[1], description: m[2] }; })`;
      const result = matchPostProcess(fn);
      assert.equal(result.requires_js, false);
      const regexTransform = result.transforms.find(
        t => typeof t === 'object' && t.type === 'regex_extract'
      );
      assert.ok(regexTransform);
      assert.equal(regexTransform.pattern, String.raw`^(\S+)\s+(.*)`);
      assert.equal(regexTransform.name, 1);
      assert.equal(regexTransform.description, 2);
    });

    it('uses internally-tagged format for regex_extract', () => {
      const fn = String.raw`(out) => out.split("\n").map(l => { const m = l.match(/^(.+)/); return { name: m[1] }; })`;
      const result = matchPostProcess(fn);
      const regexTransform = result.transforms.find(t => typeof t === 'object');
      assert.equal(regexTransform.type, 'regex_extract');
      assert.equal(regexTransform.regex_extract, undefined);
    });
  });

  describe('column/substring extraction', () => {
    it('matches substring extraction', () => {
      const fn = `(out) => out.split("\\n").map(line => ({ name: line.substring(0, 7) }))`;
      const result = matchPostProcess(fn);
      assert.equal(result.requires_js, false);
      const colTransform = result.transforms.find(
        t => typeof t === 'object' && t.type === 'column_extract'
      );
      assert.ok(colTransform);
      assert.equal(colTransform.start, 0);
      assert.equal(colTransform.end, 7);
    });

    it('matches slice extraction', () => {
      const fn = `(out) => out.split("\\n").map(line => ({ name: line.slice(0, 12) }))`;
      const result = matchPostProcess(fn);
      assert.equal(result.requires_js, false);
      assert.ok(result.transforms.some(
        t => typeof t === 'object' && t.type === 'column_extract'
      ));
    });
  });

  describe('error guard + split', () => {
    it('matches startsWith error guard', () => {
      const fn = `function(out) { if (out.startsWith("fatal:")) return []; return out.split("\\n").map(l => ({ name: l })); }`;
      const result = matchPostProcess(fn);
      assert.equal(result.requires_js, false);
      assert.deepStrictEqual(result.transforms[0], {
        type: 'error_guard',
        starts_with: 'fatal:',
      });
      assert.ok(result.transforms.includes('split_lines'));
    });

    it('matches includes error guard', () => {
      const fn = `function(out) { if (out.includes("error")) return []; return out.split("\\n").map(l => ({ name: l })); }`;
      const result = matchPostProcess(fn);
      assert.equal(result.requires_js, false);
      assert.deepStrictEqual(result.transforms[0], {
        type: 'error_guard',
        contains: 'error',
      });
    });

    it('uses internally-tagged format for error_guard', () => {
      const fn = `function(out) { if (out.startsWith("ERR")) return []; return out.split("\\n").map(l => ({ name: l })); }`;
      const result = matchPostProcess(fn);
      const guard = result.transforms[0];
      assert.equal(guard.type, 'error_guard');
      assert.equal(guard.starts_with, 'ERR');
      assert.equal(guard.error_guard, undefined);
    });
  });

  describe('unrecognized patterns', () => {
    it('marks functions without split as requires_js', () => {
      const fn = `(out) => [{ name: out.trim() }]`;
      const result = matchPostProcess(fn);
      assert.equal(result.requires_js, true);
    });

    it('handles null/undefined input', () => {
      assert.equal(matchPostProcess(null).requires_js, true);
      assert.equal(matchPostProcess(undefined).requires_js, true);
    });

    it('marks functions with Set usage as requires_js', () => {
      const fn = `function(out) { const seen = new Set(); return out.split("\\n").filter(l => { if (seen.has(l)) return false; seen.add(l); return true; }).map(l => ({ name: l })); }`;
      const result = matchPostProcess(fn);
      assert.equal(result.requires_js, true);
    });

    it('marks functions with explicit for loops as requires_js', () => {
      const fn = `function(out) { const items = []; for (const line of out.split("\\n")) { items.push({name: line}); } return items; }`;
      const result = matchPostProcess(fn);
      assert.equal(result.requires_js, true);
    });
  });

  describe('real-world Fig postProcess functions', () => {
    it('matches typical brew formula list', () => {
      const fn = 'function(a){return a.split(`\n`).map(e=>({name:e,icon:"\\u{1F37A}",description:"Formula",priority:51}))}';
      const result = matchPostProcess(fn);
      assert.equal(result.requires_js, false);
      assert.ok(result.transforms.includes('split_lines'));
    });

    it('matches git error guard + split + substring pattern', () => {
      const fn = 'function(e){let t=D(e);return t.startsWith("fatal:")?[]:t.split(`\n`).map(i=>({name:i.substring(0,7),icon:"fig://icon?type=node",description:i.substring(8)}))}';
      const result = matchPostProcess(fn);
      assert.equal(result.requires_js, false);
      assert.deepStrictEqual(result.transforms[0], {
        type: 'error_guard',
        starts_with: 'fatal:',
      });
      assert.ok(result.transforms.includes('split_lines'));
      assert.ok(result.transforms.some(
        t => typeof t === 'object' && t.type === 'column_extract'
      ));
    });
  });
});
