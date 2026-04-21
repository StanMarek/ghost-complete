// Fixture: uses an identifier (`fakeFigThing`) that looks Fig-ish but is
// neither in FIG_API_NAMES nor in the JS/Node builtins allowlist.
// The analyzer must flag it as `kind: 'free'` per plan §1.3 —
// "false positives favour correctness."

export default function suspiciousRefs(out) {
  return fakeFigThing(out).split('\n').map((l) => ({ name: l }));
}
