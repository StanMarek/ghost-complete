// Fixture: exercises every identifier in js-builtin-allowlist.json.
// The analyzer must report ZERO fig_api_refs against this file.
// See plan §1.3 M2-v4: allowlist smoke test.

export default function allowlistSmoke(out) {
  // `globals` section
  const arr = new Array(3);
  const obj = new Object();
  const str = String(out);
  const num = Number(str.length);
  const bool = Boolean(num);
  const mathy = Math.max(1, 2);
  const parsed = JSON.parse('{"k":1}');
  const m = new Map();
  const s = new Set();
  const wm = new WeakMap();
  const ws = new WeakSet();
  const p = Promise.resolve(1);
  const re = new RegExp('^foo');
  const err = new Error('boom');
  const te = new TypeError('bad');
  const re2 = new RangeError('range');
  const i = parseInt('1', 10);
  const f = parseFloat('1.5');
  const fin = isFinite(1);
  const n = isNaN(1);
  console.log('hi');
  const buf = Buffer.from('x');
  const u = undefined;
  const nn = NaN;
  const inf = Infinity;
  const sym = Symbol('s');
  const d = new Date();
  const px = new Proxy({}, {});
  const keys = Reflect.ownKeys({});
  const i8 = new Int8Array(1);
  const i16 = new Int16Array(1);
  const i32 = new Int32Array(1);
  const u8 = new Uint8Array(1);
  const u16 = new Uint16Array(1);
  const u32 = new Uint32Array(1);
  const u8c = new Uint8ClampedArray(1);
  const f32 = new Float32Array(1);
  const f64 = new Float64Array(1);
  const bi = BigInt(1);
  const bi64 = new BigInt64Array(1);
  const bu64 = new BigUint64Array(1);
  const se = new SyntaxError('se');
  const rfe = new ReferenceError('rfe');
  const ev = new EvalError('ev');
  const uri = new URIError('uri');
  const enc = encodeURIComponent('a b');
  const dec = decodeURIComponent(enc);
  const enc2 = encodeURI('a b');
  const dec2 = decodeURI(enc2);
  const gt = globalThis;

  // `module_globals` section — every identifier must be referenced in
  // real (non-comment) code so the CI smoke-check covers removals.
  // `typeof X` is the cleanest probe: it's syntactically valid on unbound
  // names (no ReferenceError at runtime) and still surfaces the identifier
  // to the analyzer as a ReferencedIdentifier, exercising the allowlist.
  const env = process.env.HOME;
  const moduleGlobals = {
    hasRequire: typeof require === 'function',
    hasModule: typeof module === 'object',
    hasExports: typeof exports === 'object',
    hasDirname: typeof __dirname === 'string',
    hasFilename: typeof __filename === 'string',
  };

  // Use the locals so an aggressive future analyzer doesn't dead-code-drop.
  return [
    arr, obj, str, num, bool, mathy, parsed, m, s, wm, ws, p, re, err, te, re2,
    i, f, fin, n, buf, u, nn, inf, sym, d, px, keys,
    i8, i16, i32, u8, u16, u32, u8c, f32, f64, bi, bi64, bu64,
    se, rfe, ev, uri, enc, dec, enc2, dec2, gt, env,
  ].map((v) => ({ name: String(v), info: moduleGlobals }));
}
