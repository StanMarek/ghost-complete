import { parse } from '@babel/parser';
import traverseModule from '@babel/traverse';

const traverse = traverseModule.default ?? traverseModule;

/**
 * Match Fig's minified helper-shaped postProcess functions and lower the
 * list-extract aliases to native transforms.
 *
 * @param {string} fnSource
 * @param {object} helperRegistry
 * @returns {{ transforms: Array|null, requires_js: boolean, js_source?: string, lowered_from_requires_js?: boolean } | null}
 */
export function matchHelperLookup(fnSource, helperRegistry) {
  if (!fnSource || typeof fnSource !== 'string') return null;

  const call = parseTopLevelHelperCall(fnSource);
  if (!call) return null;

  const { helperName, args } = call;
  const helper = getHelperEntry(helperRegistry, helperName);
  if (!helper) return null;

  if (helper.kind === 'iam_role_principal_filter') {
    return {
      transforms: null,
      requires_js: true,
      js_source: fnSource,
    };
  }

  if (helper.kind !== 'list_extract') return null;

  const stringArgs = args.slice(1).map(staticStringValue);
  if (stringArgs.length < 1 || stringArgs.length > 3) return null;
  if (stringArgs.some((value) => value === null)) return null;

  const arrayPath = jsonPathFlatKey(stringArgs[0]);
  if (arrayPath === null) return null;

  const transform = {
    type: 'json_path_extract',
    array: arrayPath,
  };
  if (stringArgs[1] !== undefined) {
    const namePath = jsonPathFlatKey(stringArgs[1]);
    if (namePath === null) return null;
    transform.name_field = namePath;
  }
  if (stringArgs[2] !== undefined) {
    // 4-arity helper calls (e.g. `l(t,"Users","UserName","Arn")`) carry
    // an explicit description field. Both the lowered native transform
    // and the JS fallback in crates/gc-jsrt/src/helpers.js honor the
    // 3rd string arg as the description projection so the two paths
    // produce equivalent {name, description} suggestion shapes.
    const descriptionPath = jsonPathFlatKey(stringArgs[2]);
    if (descriptionPath === null) return null;
    transform.description_field = descriptionPath;
  }

  return {
    transforms: [transform],
    requires_js: false,
    lowered_from_requires_js: true,
  };
}

export function topLevelHelperCallName(fnSource) {
  return parseTopLevelHelperCall(fnSource)?.helperName ?? null;
}

function parseTopLevelHelperCall(fnSource) {
  const fnNode = parseFunctionNode(fnSource);
  if (!fnNode) return null;

  const stdoutParam = getFirstIdentifierParam(fnNode);
  if (!stdoutParam) return null;

  const returned = getReturnedExpression(fnNode);
  if (!returned || returned.type !== 'CallExpression') return null;
  if (!returned.callee || returned.callee.type !== 'Identifier') return null;
  if (!isIdentifierNamed(returned.arguments[0], stdoutParam)) return null;

  return { helperName: returned.callee.name, args: returned.arguments };
}

function parseFunctionNode(fnSource) {
  const source = fnSource.trim();
  if (source.length === 0) return null;

  for (const candidate of [
    `const __ghost_helper_matcher = ${source};`,
    `const __ghost_helper_matcher = (${source});`,
  ]) {
    try {
      const ast = parse(candidate, { sourceType: 'module' });
      let fnNode = null;
      traverse(ast, {
        VariableDeclarator(path) {
          if (path.node.id?.name === '__ghost_helper_matcher') {
            fnNode = path.node.init;
            path.stop();
          }
        },
      });

      if (
        fnNode &&
        (fnNode.type === 'FunctionExpression' || fnNode.type === 'ArrowFunctionExpression')
      ) {
        return fnNode;
      }
    } catch {
      // Try the parenthesized fallback below.
    }
  }

  return null;
}

function getFirstIdentifierParam(fnNode) {
  const first = fnNode.params?.[0];
  if (!first) return null;
  if (first.type === 'Identifier') return first.name;
  if (first.type === 'AssignmentPattern' && first.left?.type === 'Identifier') {
    return first.left.name;
  }
  return null;
}

function getReturnedExpression(fnNode) {
  if (fnNode.type === 'ArrowFunctionExpression' && fnNode.body.type !== 'BlockStatement') {
    return fnNode.body;
  }

  if (!fnNode.body || fnNode.body.type !== 'BlockStatement') return null;
  const returns = fnNode.body.body.filter((stmt) => stmt.type === 'ReturnStatement');
  if (returns.length !== 1) return null;
  return returns[0].argument;
}

function getHelperEntry(helperRegistry, helperName) {
  if (!helperRegistry || typeof helperRegistry !== 'object') return null;
  const helpers = helperRegistry.helpers;
  if (!helpers || typeof helpers !== 'object' || Array.isArray(helpers)) return null;
  return helpers[helperName] ?? null;
}

function isIdentifierNamed(node, name) {
  return node?.type === 'Identifier' && node.name === name;
}

function jsonPathFlatKey(value) {
  if (typeof value !== 'string') return null;
  if (value.length > 0 && !/^\d+$/.test(value) && !/[.[\]]/.test(value)) {
    return value;
  }
  if (!value.includes("'")) return `['${value}']`;
  if (!value.includes('"')) return `["${value}"]`;
  return null;
}

function staticStringValue(node) {
  if (!node) return null;
  if (node.type === 'StringLiteral') return node.value;
  if (node.type === 'TemplateLiteral' && node.expressions.length === 0) {
    return node.quasis[0]?.value?.cooked ?? node.quasis[0]?.value?.raw ?? '';
  }
  return null;
}
