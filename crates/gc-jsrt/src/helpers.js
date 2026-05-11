// Fig-compatible single-letter helpers vendored from @withfig/autocomplete.
//
// Source reference: @withfig/autocomplete generators in
//   build/aws/*.js
// The upstream repository bundles each sub-spec independently, so the
// minifier assigns single-letter names (`l`, `p`, `c`, `d`, `h`, `f`)
// per sub-spec. All AWS post_process bodies reference these letters
// directly; without them defined, the QuickJS sandbox throws
// `ReferenceError` and the engine silently produces zero suggestions.
//
// Helpers are PURE — they touch no host APIs (no executeShellCommand,
// no fetch). They only do JSON parsing + property access. Behaviour
// derived empirically from the call sites in specs/aws.json.
//
// Pinned upstream commit: derived empirically from
// @withfig/autocomplete bundle output observed in specs/aws.json
// (53 MB, regenerated 2026-05-11). Refresh helpers.js whenever the
// upstream bundle changes shape.
//
// Shapes:
//   l(stdout, "Field", "Sub")  -> [{name: row[Sub]}] for row in stdout.Field
//   l(stdout, "Field")          -> [{name: x}] for x in stdout.Field
//   p, c, d, h                  -> aliases for l (same shape; bundler renames)
//   f(stdout, "principalDomain")
//      filters Roles whose AssumeRolePolicyDocument permits the given
//      Principal.Service. Used by `aws iam list-roles` bodies.

(function (globalScope) {
  function listExtract(stdout, arrayField, nameField) {
    let parsed;
    try {
      parsed = JSON.parse(stdout);
    } catch (_e) {
      return [];
    }
    if (!parsed || typeof parsed !== "object") return [];
    const arr = parsed[arrayField];
    if (!Array.isArray(arr)) return [];
    if (nameField === undefined) {
      const out = [];
      for (const v of arr) {
        if (v == null) continue;
        out.push({ name: String(v) });
      }
      return out;
    }
    const out = [];
    for (const item of arr) {
      if (item == null || typeof item !== "object") continue;
      const v = item[nameField];
      if (v == null) continue;
      out.push({ name: String(v) });
    }
    return out;
  }

  globalScope.l = listExtract;
  globalScope.p = listExtract;
  globalScope.c = listExtract;
  globalScope.d = listExtract;
  globalScope.h = listExtract;

  globalScope.f = function (stdout, principal) {
    let parsed;
    try {
      parsed = JSON.parse(stdout);
    } catch (_e) {
      return [];
    }
    if (!parsed || typeof parsed !== "object") return [];
    const roles = parsed.Roles;
    if (!Array.isArray(roles)) return [];
    const out = [];
    for (const role of roles) {
      if (!role || typeof role !== "object" || role.RoleName == null) continue;
      let doc = role.AssumeRolePolicyDocument;
      if (typeof doc === "string") {
        try {
          doc = JSON.parse(decodeURIComponent(doc));
        } catch (_e) {
          continue;
        }
      }
      const statements = doc && doc.Statement;
      if (!Array.isArray(statements)) continue;
      let matched = false;
      for (const s of statements) {
        const svc = s && s.Principal && s.Principal.Service;
        if (Array.isArray(svc)) {
          if (svc.indexOf(principal) !== -1) { matched = true; break; }
        } else if (svc === principal) {
          matched = true;
          break;
        }
      }
      if (matched) out.push({ name: String(role.RoleName) });
    }
    return out;
  };
})(globalThis);
