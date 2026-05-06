/**
 * serialize.js
 *
 * Canonical JSON serializer used by the fig-converter emit path so that
 * running the converter twice on the same input produces byte-identical
 * spec files (and therefore a byte-identical `specs/corpus-hash.txt`).
 *
 * Canonical form:
 *   - object keys sorted lexicographically (on JS string code points)
 *   - 2-space indent by default (matches the legacy
 *     `JSON.stringify(spec, null, 2)` output the corpus has shipped under)
 *   - arrays preserve their original order: in Fig specs, array order is
 *     semantic (e.g. `subcommands` order influences completion priority,
 *     `options` order is the documented order operators see)
 *
 * This module mirrors the algorithm used by
 * `scripts/snapshot-specs.mjs::canonicalize` so both code paths agree on
 * what "canonical" means. If you change the rule here, change it there.
 *
 * No dependencies beyond Node builtins.
 */

/**
 * Recursively return a structural copy of `value` with object keys sorted
 * lexicographically. Arrays keep their original element order. Primitives
 * (string/number/boolean/null/undefined) and special objects we don't
 * traverse (Date, etc.) are returned as-is — `JSON.stringify` will handle
 * the leaf encoding.
 *
 * @param {unknown} value
 * @returns {unknown}
 */
export function canonicalize(value) {
  if (Array.isArray(value)) {
    return value.map(canonicalize);
  }
  if (value !== null && typeof value === 'object') {
    const sortedKeys = Object.keys(value).sort();
    const out = {};
    for (const k of sortedKeys) {
      out[k] = canonicalize(value[k]);
    }
    return out;
  }
  return value;
}

/**
 * Stringify `value` to JSON with object keys sorted recursively. Output
 * matches `JSON.stringify(value, null, indent)` byte-for-byte after key
 * reordering — same indent rules, same string escaping, same number
 * formatting.
 *
 * Note: this function deliberately does NOT append a trailing newline.
 * Call sites that emit files (e.g. `index.js`) keep the legacy
 * `+ '\n'` behaviour explicit at the write site, matching the prior
 * `JSON.stringify(spec, null, 2) + '\n'` shape.
 *
 * @param {unknown} value
 * @param {number|string} [indent=2] - Forwarded to `JSON.stringify`'s third
 *   argument; pass `0` or `''` for compact output.
 * @returns {string}
 */
export function stringifySorted(value, indent = 2) {
  return JSON.stringify(canonicalize(value), null, indent);
}
