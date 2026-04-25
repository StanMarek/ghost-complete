import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { matchPostProcess, CORRECTED_IN_VERSION } from './post-process-matcher.js';

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

    it('JSON.parse without resolvable field is marked requires_js (no silent "name" fallback)', () => {
      // JSON.parse is present but the extracted value isn't bound to a
      // `name:` key via any of the strategies (direct, bracket, or variable
      // assignment + name: access). The old matcher guessed `name: "name"`
      // here — wrong for inputs like j.metadata.id. New behaviour: defer to JS.
      const fn = `(out) => out.split("\\n").filter(Boolean).map(line => { const j = JSON.parse(line); return j.metadata.id; })`;
      const result = matchPostProcess(fn);
      assert.equal(result.requires_js, true);
      assert.equal(result.transforms, null);
      assert.equal(result.js_source, fn);
      // This is a corrected path — marker must be present so doctor can
      // surface the behaviour change to users after upgrade.
      assert.equal(result._corrected_in, CORRECTED_IN_VERSION);
      assert.equal(result._corrected_in, 'v0.10.0');
      // Specifically: it must NOT fall back to {type: 'json_extract', name: 'name'}.
      assert.ok(
        !Array.isArray(result.transforms) ||
          !result.transforms.some(
            t => typeof t === 'object' && t.type === 'json_extract' && t.name === 'name'
          )
      );
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
    it('substring extraction is marked requires_js', () => {
      // .substring(0, N) is a byte-offset slice, NOT the whitespace-delimited
      // column_extract transform. The old matcher emitted column_extract
      // here and produced wrong completions at runtime — the correct
      // behaviour is to defer to JS.
      const fn = `(out) => out.split("\\n").map(line => ({ name: line.substring(0, 7) }))`;
      const result = matchPostProcess(fn);
      assert.equal(result.requires_js, true);
      assert.equal(result.transforms, null);
      assert.equal(result.js_source, fn);
      // Corrected path — marker present for doctor surfacing.
      assert.equal(result._corrected_in, CORRECTED_IN_VERSION);
      assert.equal(result._corrected_in, 'v0.10.0');
    });

    it('slice extraction is marked requires_js', () => {
      const fn = `(out) => out.split("\\n").map(line => ({ name: line.slice(0, 12) }))`;
      const result = matchPostProcess(fn);
      assert.equal(result.requires_js, true);
      assert.equal(result.transforms, null);
      assert.equal(result.js_source, fn);
      // Corrected path — marker present.
      assert.equal(result._corrected_in, CORRECTED_IN_VERSION);
      assert.equal(result._corrected_in, 'v0.10.0');
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

    it('does NOT set _corrected_in on un-matched-from-day-one patterns', () => {
      // Explicit for-loop — this has always been requires_js, it was never
      // silently mis-converted. Only the specific bug-class paths (substring/
      // slice and JSON.parse unresolvable field) get the corrected-in marker.
      const fn = `function(out) { const items = []; for (const line of out.split("\\n")) { items.push({name: line}); } return items; }`;
      const result = matchPostProcess(fn);
      assert.equal(result.requires_js, true);
      assert.equal(result._corrected_in, undefined);
    });

    it('does NOT set _corrected_in on functions without a split pattern', () => {
      // No split — bails in Phase 2, which is also not a corrected path.
      const fn = `(out) => [{ name: out.trim() }]`;
      const result = matchPostProcess(fn);
      assert.equal(result.requires_js, true);
      assert.equal(result._corrected_in, undefined);
    });

    it('does NOT set _corrected_in on null/undefined input', () => {
      // Defensive early-return — never a corrected path.
      assert.equal(matchPostProcess(null)._corrected_in, undefined);
      assert.equal(matchPostProcess(undefined)._corrected_in, undefined);
    });

    it('does NOT set _corrected_in on complex-logic bail-outs (Set usage)', () => {
      const fn = `function(out) { const seen = new Set(); return out.split("\\n").filter(l => { if (seen.has(l)) return false; seen.add(l); return true; }).map(l => ({ name: l })); }`;
      const result = matchPostProcess(fn);
      assert.equal(result.requires_js, true);
      assert.equal(result._corrected_in, undefined);
    });
  });

  describe('JSON.parse dotted-path array extraction (json_extract_array)', () => {
    it('matches parse-map-6 shape: .project.schemes.map(e => ({name: e}))', () => {
      const fn = `t=>JSON.parse(t).project.schemes.map(e=>({name:e}))`;
      const result = matchPostProcess(fn);
      assert.equal(result.requires_js, false);
      assert.equal(result.transforms.length, 1);
      assert.deepStrictEqual(result.transforms[0], {
        type: 'json_extract_array',
        path: 'project.schemes',
      });
    });

    it('matches parse-map-6 with different callback param names', () => {
      const fn = `e=>JSON.parse(e).project.configurations.map(t=>({name:t}))`;
      const result = matchPostProcess(fn);
      assert.equal(result.requires_js, false);
      assert.deepStrictEqual(result.transforms, [
        { type: 'json_extract_array', path: 'project.configurations' },
      ]);
    });

    it('matches parse-map-split shape with split(" ")[0] in callback', () => {
      const fn = `function(e){return JSON.parse(e).workspace.members.map(n=>n.split(" ")[0])}`;
      const result = matchPostProcess(fn);
      assert.equal(result.requires_js, false);
      assert.deepStrictEqual(result.transforms, [
        {
          type: 'json_extract_array',
          path: 'workspace.members',
          split_on: ' ',
          split_index: 0,
        },
      ]);
    });

    it('supports element-sub-field extraction: .map(e => ({name: e.label}))', () => {
      const fn = `t=>JSON.parse(t).data.items.map(e=>({name:e.label}))`;
      const result = matchPostProcess(fn);
      assert.equal(result.requires_js, false);
      assert.deepStrictEqual(result.transforms, [
        {
          type: 'json_extract_array',
          path: 'data.items',
          item_name: 'label',
        },
      ]);
    });

    it('emits a terminal pipeline (no split_lines / filter_empty)', () => {
      // json_extract_array consumes raw output, so it must NOT be
      // accompanied by split_lines in the pipeline — that would shred the
      // JSON blob into unparseable fragments.
      const fn = `t=>JSON.parse(t).project.schemes.map(e=>({name:e}))`;
      const result = matchPostProcess(fn);
      assert.ok(!result.transforms.includes('split_lines'));
      assert.ok(!result.transforms.includes('filter_empty'));
    });

    it('does NOT match single-segment dotted paths', () => {
      // Only 2+ segment paths go through json_extract_array. Single-segment
      // shapes (`.items.map(...)`) either hit other matchers or fall through
      // to requires_js — we don't want to steal them here.
      const fn = `t=>JSON.parse(t).items.map(e=>({name:e}))`;
      const result = matchPostProcess(fn);
      // No split pattern either → falls through to requires_js.
      assert.equal(result.requires_js, true);
    });

    it('does NOT match unknown callback shapes', () => {
      // `.map(e => complicated_expression)` — we don't know how to extract
      // from this, must defer.
      const fn = `t=>JSON.parse(t).a.b.map(e=>doSomething(e))`;
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

    it('git error guard + split + substring is marked requires_js', () => {
      // This real-world Fig postProcess uses .substring(0,7) to grab a short
      // git SHA — a byte-offset slice that has no correct lowering to
      // column_extract. We must defer the whole function to JS rather than
      // silently produce wrong completions.
      const fn = 'function(e){let t=D(e);return t.startsWith("fatal:")?[]:t.split(`\n`).map(i=>({name:i.substring(0,7),icon:"fig://icon?type=node",description:i.substring(8)}))}';
      const result = matchPostProcess(fn);
      assert.equal(result.requires_js, true);
      assert.equal(result.transforms, null);
      assert.equal(result.js_source, fn);
    });
  });
});
