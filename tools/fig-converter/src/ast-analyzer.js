// AST analyzer scaffold for the Phase 1 classification spike.
//
// Given a JS generator's source, this module returns:
//   * a `shape` fingerprint that buckets structurally-equivalent generators
//     regardless of string-literal differences;
//   * a list of Fig-API references, each tagged with how the binding was
//     introduced (`direct`, `destructured`, `aliased`, `member-aliased`,
//     `reexported`, `free`);
//   * a `parse_error` string when the source can't be parsed (so callers
//     can bucket parse failures without the analyzer throwing).
//
// This file is plumbing — it does NOT run against the 709-spec corpus.
// That's the human-gated spike (plan §1.2). Tests in
// `ast-analyzer.test.js` drive the contract.
//
// References:
//   Plan §1.1  — analyzer contract + M1-v4 test matrix
//   Plan §1.3  — scope-aware detection + allowlist governance (M2-v4)
//   Plan §1.7  — Phase 1 test matrix rollup

import { parse } from '@babel/parser';
import traverseModule from '@babel/traverse';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';

// @babel/traverse ships a CJS default-export that shows up as .default
// under Node's ESM interop. Normalize once.
const traverse = traverseModule.default ?? traverseModule;

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);

const allowlistRaw = JSON.parse(
  readFileSync(resolve(__dirname, 'js-builtin-allowlist.json'), 'utf8'),
);

// Flatten globals + module_globals into a single Set for O(1) lookup.
// `schema_version` exists so future shape changes (e.g., per-member gating
// for `process.env`) don't silently break consumers.
const BUILTIN_ALLOWLIST = new Set([
  ...(allowlistRaw.globals ?? []),
  ...(allowlistRaw.module_globals ?? []),
]);

// Hand-curated list of Fig API names — an identifier matching one of these,
// when not shadowed by a local binding, is a Fig API reference.
// Source: plan §1.3 seed list. Expand as the spike surfaces more.
const FIG_API_NAMES = new Set([
  'executeShellCommand',
  'executeCommand',
  'runCommand',
  'context',
  'currentWorkingDirectory',
  'currentProcess',
  'environmentVariables',
  'searchTerm',
  'tokens',
  'fig',
]);

// Module specifiers that re-export the Fig API surface. Binding any name
// from one of these sources flags that name as a `reexported` Fig ref.
const FIG_MODULE_NAMES = new Set([
  'fig',
  '@withfig/api',
  '@withfig/autocomplete',
]);

// Zero-filled shape struct used when parsing fails or when there is nothing
// to analyse. Kept as a factory so callers never share a mutable ref.
function zeroShape() {
  return {
    fingerprint: '',
    has_json_parse: false,
    has_regex_match: false,
    has_substring_or_slice: false,
    has_conditional: false,
    has_await: false,
  };
}

/**
 * Entry point: analyse a generator's JS source.
 *
 * @param {string} jsSource  Raw source code. May be a top-level expression
 *                           (e.g. `(out) => out.split("\n")`), a module,
 *                           or a CJS snippet.
 * @returns {{
 *   shape: {
 *     fingerprint: string,
 *     has_json_parse: boolean,
 *     has_regex_match: boolean,
 *     has_substring_or_slice: boolean,
 *     has_conditional: boolean,
 *     has_await: boolean,
 *   },
 *   fig_api_refs: Array<{name: string, kind: 'direct'|'destructured'|'aliased'|'member-aliased'|'reexported'|'free'}>,
 *   parse_error: string|null,
 * }}
 */
export function analyzeGenerator(jsSource) {
  let ast;
  try {
    ast = parse(jsSource, {
      sourceType: 'module',
      allowReturnOutsideFunction: true,
      plugins: [],
    });
  } catch (errModule) {
    // Second attempt: wrap in parens to force expression context. Recovers
    // bare function expressions (`function(a){...}`) and object-literal
    // returns (`{ name: "x" }`) that are invalid at statement position
    // under sourceType: 'module' but valid inside a parenthesized expression.
    try {
      ast = parse(`(${jsSource})`, {
        sourceType: 'module',
        allowReturnOutsideFunction: true,
        plugins: [],
      });
    } catch (_errWrapped) {
      // Return the ORIGINAL module-mode error so diagnostics point at the
      // true token position, not the wrapped offset.
      return {
        shape: zeroShape(),
        fig_api_refs: [],
        parse_error: errModule.message,
      };
    }
  }

  const shape = zeroShape();

  // Accumulates `{name, kind}` entries. A given name may appear multiple
  // times (e.g., a destructured ref that is then aliased); we dedupe at
  // the end so the output is stable.
  const rawRefs = [];

  // Map binding-name -> kind classification derived from the binding's
  // init expression. Populated in a first pass so call sites can resolve
  // aliases without re-walking the AST.
  //
  // Keys are binding names in a *shared* scope heuristic — Phase 1 uses
  // the program-level scope as a single namespace because generators are
  // typically tiny. If the spike surfaces scoping bugs, revisit.
  //
  // Values: one of 'destructured' | 'aliased' | 'member-aliased' |
  // 'reexported'. (`direct` and `free` are determined at reference sites.)
  const bindingKinds = new Map();

  // Names imported from Fig modules (via `require` or `import`). Any
  // binding produced by one of these is tagged `reexported`.
  const figModuleBindings = new Set();

  // --- Pass 1: collect bindings from require/import + VariableDeclarators.
  traverse(ast, {
    // CJS: const lib = require('fig');   → lib is a Fig module binding.
    //      const { foo } = require('fig'); → foo is reexported.
    VariableDeclarator(path) {
      const id = path.node.id;
      const init = path.node.init;
      if (!init) return;

      // require('fig')
      if (isRequireCall(init, FIG_MODULE_NAMES)) {
        if (id.type === 'Identifier') {
          figModuleBindings.add(id.name);
          bindingKinds.set(id.name, 'reexported');
        } else if (id.type === 'ObjectPattern') {
          for (const prop of id.properties) {
            if (prop.type === 'ObjectProperty' && prop.value.type === 'Identifier') {
              bindingKinds.set(prop.value.name, 'reexported');
            }
          }
        }
        return;
      }

      // const { executeShellCommand } = fig; — destructured from a Fig ref.
      if (id.type === 'ObjectPattern' && init.type === 'Identifier') {
        const rootName = init.name;
        const isFigSource =
          FIG_API_NAMES.has(rootName) || figModuleBindings.has(rootName);
        if (isFigSource) {
          for (const prop of id.properties) {
            if (
              prop.type === 'ObjectProperty' &&
              prop.value.type === 'Identifier' &&
              prop.key.type === 'Identifier' &&
              FIG_API_NAMES.has(prop.key.name)
            ) {
              bindingKinds.set(prop.value.name, 'destructured');
            }
          }
        }
        return;
      }

      if (id.type !== 'Identifier') return;

      // const cmd = executeShellCommand; — aliased rename.
      if (init.type === 'Identifier') {
        // Resolve whether init refers to a known Fig API name at the
        // program level (either a seed API name, an earlier destructured
        // binding, or an already-aliased binding).
        const initName = init.name;
        const initKind = bindingKinds.get(initName);
        const initIsFigApi =
          FIG_API_NAMES.has(initName) ||
          initKind === 'destructured' ||
          initKind === 'aliased' ||
          initKind === 'member-aliased' ||
          initKind === 'reexported';
        if (initIsFigApi) {
          bindingKinds.set(id.name, 'aliased');
        }
        return;
      }

      // const cmd = fig.executeShellCommand; — member-expression rename.
      if (init.type === 'MemberExpression') {
        const root = memberRoot(init);
        if (
          root &&
          root.type === 'Identifier' &&
          (FIG_API_NAMES.has(root.name) || figModuleBindings.has(root.name))
        ) {
          bindingKinds.set(id.name, 'member-aliased');
        }
      }
    },

    // ESM: import { executeShellCommand } from 'fig';
    //      import lib from 'fig';
    //      import * as lib from 'fig';
    ImportDeclaration(path) {
      const source = path.node.source.value;
      if (!FIG_MODULE_NAMES.has(source)) return;
      for (const spec of path.node.specifiers) {
        // Named: import { X } from 'fig'
        if (spec.type === 'ImportSpecifier') {
          bindingKinds.set(spec.local.name, 'reexported');
        } else if (spec.type === 'ImportDefaultSpecifier') {
          // Default: import lib from 'fig'
          figModuleBindings.add(spec.local.name);
          bindingKinds.set(spec.local.name, 'reexported');
        } else if (spec.type === 'ImportNamespaceSpecifier') {
          // Namespace: import * as lib from 'fig'
          figModuleBindings.add(spec.local.name);
          bindingKinds.set(spec.local.name, 'reexported');
        }
      }
    },
  });

  // --- Pass 2: reference-site classification + shape accumulation.
  let primaryTopLevelExpr = null;

  traverse(ast, {
    Program(path) {
      primaryTopLevelExpr = primaryExpression(path.node);
    },

    AwaitExpression() {
      shape.has_await = true;
    },

    ConditionalExpression() {
      shape.has_conditional = true;
    },

    IfStatement() {
      shape.has_conditional = true;
    },

    LogicalExpression(path) {
      // && and || in *expression position* within a body count as control
      // flow; plain `a || b` used as a default value also counts (it's
      // still a branch). Nullish coalescing (??) lives here too.
      const op = path.node.operator;
      if (op === '&&' || op === '||' || op === '??') {
        shape.has_conditional = true;
      }
    },

    CallExpression(path) {
      const callee = path.node.callee;

      // JSON.parse(...)
      if (
        callee.type === 'MemberExpression' &&
        callee.object.type === 'Identifier' &&
        callee.object.name === 'JSON' &&
        callee.property.type === 'Identifier' &&
        callee.property.name === 'parse'
      ) {
        shape.has_json_parse = true;
      }

      // x.match(/regex/) — only count when the first arg is a regex literal.
      if (
        callee.type === 'MemberExpression' &&
        callee.property.type === 'Identifier' &&
        callee.property.name === 'match' &&
        path.node.arguments.length > 0 &&
        path.node.arguments[0].type === 'RegExpLiteral'
      ) {
        shape.has_regex_match = true;
      }

      // x.substring(...) or x.slice(...)
      if (
        callee.type === 'MemberExpression' &&
        callee.property.type === 'Identifier' &&
        (callee.property.name === 'substring' || callee.property.name === 'slice')
      ) {
        shape.has_substring_or_slice = true;
      }

      // Direct Fig API call:  ctx.executeShellCommand(...) or executeShellCommand(...)
      if (callee.type === 'MemberExpression') {
        const prop = callee.property;
        if (
          !callee.computed &&
          prop.type === 'Identifier' &&
          FIG_API_NAMES.has(prop.name)
        ) {
          // Gap-A guard: if the object root resolves to a NESTED
          // callback-param binding (e.g. `x` in `.map(x => x.foo(y))`),
          // `.foo` is a method call on input data, NOT a Fig API ref.
          // Distinguish from the canonical `(ctx) => ctx.executeShellCommand(...)`
          // shape: a top-level arrow param has its owning function nested
          // directly under the Program scope, while a callback param's
          // owning function is nested inside another function.
          const root = memberRoot(callee);
          const rootName = root && root.type === 'Identifier' ? root.name : null;
          const rootBinding =
            rootName ? path.scope.getBinding(rootName) : null;
          if (rootBinding && isNestedCallbackParamBinding(rootBinding)) {
            // Skip — property-access on a callback param is not Fig API.
          } else {
            // If the root resolves to a known local binding we classified,
            // that alias is reported separately; still record the call's
            // *property* as `direct` because the member name itself is the
            // Fig API surface being used.
            rawRefs.push({ name: prop.name, kind: 'direct' });
            // Intentional fall-through — see below for the ref-site pass
            // over Identifier, which catches the alias-at-callsite case.
          }
        }
      } else if (callee.type === 'Identifier' && FIG_API_NAMES.has(callee.name)) {
        // Bare call: executeShellCommand(...) with no receiver. Whether
        // this resolves to a destructured binding, an alias, or a free
        // reference is determined by Pass 1's bindingKinds map, handled
        // by the Identifier visitor below.
        // We deliberately DON'T push `direct` here to avoid duplicating
        // the Identifier visitor's classification.
      }
    },

    // Reference-site pass for bare identifiers. Handles:
    //   - destructured / aliased / member-aliased / reexported bindings
    //     that are *referenced* (called, read, passed as arg) anywhere
    //   - free identifiers that aren't in the allowlist and aren't
    //     locally bound anywhere — flagged as `free` (probable Fig API).
    ReferencedIdentifier(path) {
      const name = path.node.name;

      // Skip identifiers that are property keys in member expressions;
      // those are handled by the CallExpression visitor above (direct
      // Fig API detection).
      const parent = path.parent;
      if (
        parent.type === 'MemberExpression' &&
        parent.property === path.node &&
        !parent.computed
      ) {
        return;
      }

      // If the name was classified in Pass 1, surface that classification.
      const kind = bindingKinds.get(name);
      if (kind) {
        rawRefs.push({ name, kind });
        return;
      }

      // Allowlisted JS/Node builtins.
      if (BUILTIN_ALLOWLIST.has(name)) return;

      // Parameters, locally-declared variables, and function names are
      // NEVER Fig API refs — they're user bindings.
      const binding = path.scope.getBinding(name);
      if (binding) {
        // It resolves to a binding we didn't classify as Fig-related in
        // Pass 1. That makes it a plain user binding — not a Fig ref.
        return;
      }

      // Directly-named Fig API at a reference site. (The CallExpression
      // visitor handles the member-expression case; this covers bare
      // references like `fig` or `searchTerm`.)
      if (FIG_API_NAMES.has(name)) {
        rawRefs.push({ name, kind: 'direct' });
        return;
      }

      // Unresolved free identifier that isn't allowlisted. Plan §1.3:
      // "false positives favour correctness." Flag as `free`.
      rawRefs.push({ name, kind: 'free' });
    },
  });

  shape.fingerprint = primaryTopLevelExpr
    ? fingerprintExpression(primaryTopLevelExpr)
    : '';

  return {
    shape,
    fig_api_refs: dedupeRefs(rawRefs),
    parse_error: null,
  };
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Is this init expression a `require('<fig module>')` call? */
function isRequireCall(node, moduleSet) {
  return (
    node.type === 'CallExpression' &&
    node.callee.type === 'Identifier' &&
    node.callee.name === 'require' &&
    node.arguments.length === 1 &&
    node.arguments[0].type === 'StringLiteral' &&
    moduleSet.has(node.arguments[0].value)
  );
}

/** Walk to the root identifier of a MemberExpression chain. */
function memberRoot(node) {
  let cursor = node;
  while (cursor && cursor.type === 'MemberExpression') {
    cursor = cursor.object;
  }
  return cursor;
}

/**
 * Return true iff the binding is a parameter of a function that is itself
 * nested inside another function — i.e. a lexical binding from a callback
 * like `.map(x => …)` or `.filter((a, b) => …)`.
 *
 * The canonical Fig-generator shape `(ctx) => ctx.executeShellCommand(...)`
 * has the arrow at program top level, so its parameters are NOT classified
 * as callback-param bindings here (and their `.FIG_API_NAME(...)` calls
 * continue to flag as direct Fig refs — see case 1 of the test matrix).
 */
function isNestedCallbackParamBinding(binding) {
  if (!binding || binding.kind !== 'param') return false;
  const ownerScope = binding.scope;
  if (!ownerScope) return false;
  const ownerBlock = ownerScope.block;
  if (
    !ownerBlock ||
    (ownerBlock.type !== 'ArrowFunctionExpression' &&
      ownerBlock.type !== 'FunctionExpression' &&
      ownerBlock.type !== 'FunctionDeclaration')
  ) {
    return false;
  }
  // If the function owning this param has another function in its scope
  // chain above it (before hitting Program), it's a nested callback.
  let cursor = ownerScope.parent;
  while (cursor) {
    const block = cursor.block;
    if (!block) break;
    if (
      block.type === 'ArrowFunctionExpression' ||
      block.type === 'FunctionExpression' ||
      block.type === 'FunctionDeclaration' ||
      block.type === 'ObjectMethod' ||
      block.type === 'ClassMethod'
    ) {
      return true;
    }
    cursor = cursor.parent;
  }
  return false;
}

/** Dedupe refs while preserving first occurrence + kind precedence. */
function dedupeRefs(refs) {
  // Precedence: a name seen as `direct` + `free` resolves to `direct`.
  // Precedence order (highest first):
  //   reexported > member-aliased > aliased > destructured > direct > free
  const precedence = [
    'reexported',
    'member-aliased',
    'aliased',
    'destructured',
    'direct',
    'free',
  ];
  const bestKind = new Map();
  const firstSeen = new Map();

  for (let i = 0; i < refs.length; i++) {
    const r = refs[i];
    if (!firstSeen.has(r.name)) firstSeen.set(r.name, i);
    const cur = bestKind.get(r.name);
    if (!cur) {
      bestKind.set(r.name, r.kind);
    } else if (precedence.indexOf(r.kind) < precedence.indexOf(cur)) {
      bestKind.set(r.name, r.kind);
    }
  }

  return [...bestKind.entries()]
    .sort((a, b) => firstSeen.get(a[0]) - firstSeen.get(b[0]))
    .map(([name, kind]) => ({ name, kind }));
}

// ---------------------------------------------------------------------------
// Fingerprinting
// ---------------------------------------------------------------------------
//
// The fingerprint collapses a generator's "interesting" top-level expression
// into a string where:
//   - string/number/regex/function/object/array literals become STR/NUM/
//     REGEX/FN/OBJ/ARR placeholder tokens;
//   - variable/parameter references in arg positions become `...`;
//   - call chains preserve order and the full `.method(ARGS)` spelling;
//   - property access becomes `.PROP` (name dropped — we only care about
//     the shape, not the field).
//
// The *subject* of the top-level expression (e.g., `out` in
// `out.split(...)`) is dropped; the fingerprint is a suffix-style chain.
// This is deliberate — generators bucket well on the operations they run,
// not the name of the lambda parameter.

/** Pick the primary expression from a Program. */
function primaryExpression(programNode) {
  // Prefer `module.exports = EXPR` / `export default EXPR`.
  for (const stmt of programNode.body) {
    if (stmt.type === 'ExportDefaultDeclaration') {
      return stmt.declaration;
    }
    if (
      stmt.type === 'ExpressionStatement' &&
      stmt.expression.type === 'AssignmentExpression' &&
      stmt.expression.left.type === 'MemberExpression' &&
      stmt.expression.left.object.type === 'Identifier' &&
      stmt.expression.left.object.name === 'module' &&
      stmt.expression.left.property.type === 'Identifier' &&
      stmt.expression.left.property.name === 'exports'
    ) {
      return stmt.expression.right;
    }
  }

  // Otherwise, last ExpressionStatement — captures `(out) => …` as a
  // trailing expression. Fallback to first.
  const exprs = programNode.body.filter((s) => s.type === 'ExpressionStatement');
  if (exprs.length > 0) return exprs[exprs.length - 1].expression;

  // Otherwise, the first VariableDeclarator's init — best-effort.
  for (const stmt of programNode.body) {
    if (stmt.type === 'VariableDeclaration') {
      for (const d of stmt.declarations) {
        if (d.init) return d.init;
      }
    }
  }
  return null;
}

/** Fingerprint entry point — handles function/arrow wrappers. */
function fingerprintExpression(node) {
  if (!node) return '';

  // Unwrap arrow / function expressions: fingerprint their *body*.
  if (
    node.type === 'ArrowFunctionExpression' ||
    node.type === 'FunctionExpression' ||
    node.type === 'FunctionDeclaration'
  ) {
    const body = node.body;
    if (body.type === 'BlockStatement') {
      // Walk statements and pick the first `return EXPR;` we find.
      for (const stmt of body.body) {
        if (stmt.type === 'ReturnStatement' && stmt.argument) {
          return fingerprintChain(stmt.argument);
        }
      }
      return '';
    }
    return fingerprintChain(body);
  }

  return fingerprintChain(node);
}

/** Fingerprint a call/member chain or a leaf expression. */
function fingerprintChain(node) {
  if (!node) return '';

  // AwaitExpression — prefix "await " + underlying.
  if (node.type === 'AwaitExpression') {
    return 'await ' + fingerprintChain(node.argument);
  }

  // ConditionalExpression — collapse to `(COND ? A : B)`.
  if (node.type === 'ConditionalExpression') {
    return (
      '(' +
      fingerprintChain(node.test) +
      ' ? ' +
      fingerprintChain(node.consequent) +
      ' : ' +
      fingerprintChain(node.alternate) +
      ')'
    );
  }

  // LogicalExpression — `A || B`, `A ?? B`, `A && B`.
  if (node.type === 'LogicalExpression') {
    return (
      '(' +
      fingerprintChain(node.left) +
      ' ' +
      node.operator +
      ' ' +
      fingerprintChain(node.right) +
      ')'
    );
  }

  // Call or MemberExpression → walk the chain root-first.
  if (node.type === 'CallExpression' || node.type === 'MemberExpression') {
    return fingerprintCallChain(node);
  }

  // Leaves.
  return leafToken(node);
}

/**
 * Walk a call/member chain root → tail, producing a single fingerprint
 * string. Drops the root subject (e.g. `out`) when it's a bare identifier
 * so `out.split(X)` and `parsed.split(X)` bucket together; keeps it when
 * it's semantically load-bearing (JSON.parse, Object.keys, etc.).
 */
function fingerprintCallChain(node) {
  // Flatten the chain into ordered segments.
  const segments = [];
  let cursor = node;
  while (cursor) {
    if (cursor.type === 'CallExpression') {
      const callee = cursor.callee;
      if (callee.type === 'MemberExpression') {
        const propName =
          !callee.computed && callee.property.type === 'Identifier'
            ? callee.property.name
            : '[COMPUTED]';
        segments.push({
          kind: 'method-call',
          name: propName,
          args: cursor.arguments,
        });
        cursor = callee.object;
      } else if (callee.type === 'Identifier') {
        segments.push({
          kind: 'bare-call',
          name: callee.name,
          args: cursor.arguments,
        });
        cursor = null;
      } else {
        // E.g., (await x)(); rare in generators. Stash as a generic token.
        segments.push({ kind: 'opaque-call', args: cursor.arguments });
        cursor = null;
      }
    } else if (cursor.type === 'MemberExpression') {
      if (!cursor.computed && cursor.property.type === 'Identifier') {
        // `.foo` — structural-only, name collapses to PROP.
        // Special-case well-known namespace roots (JSON, Object, Array,
        // Math) later when we emit.
        segments.push({
          kind: 'property',
          name: cursor.property.name,
        });
      } else {
        segments.push({ kind: 'computed-property' });
      }
      cursor = cursor.object;
    } else {
      // Reached the root.
      segments.push({ kind: 'root', node: cursor });
      cursor = null;
    }
  }

  segments.reverse();

  // Emit.
  const parts = [];
  for (let i = 0; i < segments.length; i++) {
    const seg = segments[i];
    switch (seg.kind) {
      case 'root': {
        const n = seg.node;
        if (n.type === 'Identifier') {
          // Keep well-known namespace roots verbatim; drop plain params.
          if (
            n.name === 'JSON' ||
            n.name === 'Object' ||
            n.name === 'Array' ||
            n.name === 'Math' ||
            n.name === 'String' ||
            n.name === 'Number' ||
            n.name === 'Promise'
          ) {
            parts.push(n.name);
          } else {
            // Plain param/var — dropped so unrelated generators bucket.
          }
        } else if (n.type === 'AwaitExpression') {
          parts.push('await ' + fingerprintChain(n.argument));
        } else {
          parts.push(leafToken(n));
        }
        break;
      }
      case 'property': {
        // Preserve property name when it rides a well-known root; else PROP.
        const priorRoot = segments[0];
        const rootKeepsNames =
          priorRoot &&
          priorRoot.kind === 'root' &&
          priorRoot.node.type === 'Identifier' &&
          (priorRoot.node.name === 'JSON' ||
            priorRoot.node.name === 'Object' ||
            priorRoot.node.name === 'Array' ||
            priorRoot.node.name === 'Math' ||
            priorRoot.node.name === 'String' ||
            priorRoot.node.name === 'Number' ||
            priorRoot.node.name === 'Promise');
        // Only the IMMEDIATE property after a well-known root keeps its
        // real name (e.g. `JSON.parse`). Further chained props collapse.
        if (
          rootKeepsNames &&
          i === 1 // immediate child of root
        ) {
          parts.push('.' + seg.name);
        } else {
          parts.push('.PROP');
        }
        break;
      }
      case 'computed-property':
        parts.push('[COMPUTED]');
        break;
      case 'method-call': {
        // `.foo(ARGS)` — preserve method name, collapse args.
        // Exception: if this is the immediate child of a well-known root
        // (e.g., `JSON.parse(...)`), keep args as `...` sentinel.
        const priorRoot = segments[0];
        const rootKeepsNames =
          priorRoot &&
          priorRoot.kind === 'root' &&
          priorRoot.node.type === 'Identifier' &&
          (priorRoot.node.name === 'JSON' ||
            priorRoot.node.name === 'Object' ||
            priorRoot.node.name === 'Array' ||
            priorRoot.node.name === 'Math' ||
            priorRoot.node.name === 'String' ||
            priorRoot.node.name === 'Number' ||
            priorRoot.node.name === 'Promise');
        if (rootKeepsNames && i === 1) {
          parts.push('.' + seg.name + '(...)');
        } else {
          parts.push('.' + seg.name + '(' + argsToken(seg.args) + ')');
        }
        break;
      }
      case 'bare-call':
        parts.push(seg.name + '(' + argsToken(seg.args) + ')');
        break;
      case 'opaque-call':
        parts.push('(...)(' + argsToken(seg.args) + ')');
        break;
    }
  }

  // If the root was a plain param (nothing pushed), the chain starts with
  // a dot — that's the canonical suffix form we want.
  let out = parts.join('');
  // Suffix-form chains: keep leading dot as-is.
  return out;
}

/** Summarise a call's argument list as a comma-separated placeholder list. */
function argsToken(args) {
  return args.map(leafToken).join(',');
}

/** Map a leaf node to its placeholder token. */
function leafToken(node) {
  if (!node) return '...';
  switch (node.type) {
    case 'StringLiteral':
      return 'STR';
    case 'NumericLiteral':
      return 'NUM';
    case 'BooleanLiteral':
      return 'BOOL';
    case 'NullLiteral':
      return 'NULL';
    case 'RegExpLiteral':
      return 'REGEX';
    case 'TemplateLiteral':
      return 'STR';
    case 'ArrowFunctionExpression':
    case 'FunctionExpression':
      return 'FN';
    case 'ObjectExpression':
      return 'OBJ';
    case 'ArrayExpression':
      return 'ARR';
    case 'Identifier':
      // Preserve well-known builtins as themselves (Boolean, JSON, etc.)
      // because they're semantically meaningful at arg position
      // (e.g., `.filter(Boolean)`).
      if (
        node.name === 'Boolean' ||
        node.name === 'Number' ||
        node.name === 'String' ||
        node.name === 'JSON' ||
        node.name === 'Array' ||
        node.name === 'Object'
      ) {
        return 'FN';
      }
      return '...';
    default:
      return '...';
  }
}
