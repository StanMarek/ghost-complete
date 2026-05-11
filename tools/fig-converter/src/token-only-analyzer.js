import { parse } from '@babel/parser';
import traverseModule from '@babel/traverse';

// @babel/traverse is CommonJS under Node ESM interop.
const traverse = traverseModule.default ?? traverseModule;

const CAPABILITY_IDENTIFIERS = new Set([
  'fetch',
  'XMLHttpRequest',
  'executeShellCommand',
  'process',
  'require',
]);

const FIG_CAPABILITY_NAMESPACES = new Set([
  'fs',
  'path',
  'keychain',
  'ipc',
  'ui',
]);

/**
 * Classify whether a JS generator body can run in the token-only sandbox.
 *
 * Token-only deliberately allows unresolved/free identifiers: without host
 * capability bindings they can throw, but they cannot escape the sandbox.
 * The only converter-side hard rejects are known capability globals and the
 * common `await freeIdentifier(...)` shell/network helper shape.
 *
 * @param {string} jsSource
 * @returns {{
 *   token_only: boolean,
 *   rejection: null | {code: string, name?: string, message: string},
 *   parse_error: string | null,
 * }}
 */
export function analyzeTokenOnly(jsSource) {
  if (!jsSource || typeof jsSource !== 'string') {
    return rejected('invalid_source', {
      message: 'expected non-empty JS source string',
      parse_error: null,
    });
  }

  const parsed = parseSource(jsSource);
  if (parsed.parse_error) {
    return rejected('parse_error', {
      message: parsed.parse_error,
      parse_error: parsed.parse_error,
    });
  }

  let rejection = null;
  const rejectOnce = (code, name, message) => {
    if (rejection) return;
    rejection = { code, message };
    if (name) rejection.name = name;
  };
  const rejectIdentifierCall = (argument, code, verb, { requireFree } = { requireFree: true }) => {
    if (rejection || !argument.isCallExpression()) return;

    const callee = argument.get('callee');
    if (!callee.isIdentifier()) return;

    const name = callee.node.name;
    if (!requireFree || !callee.scope.getBinding(name)) {
      rejectOnce(
        code,
        name,
        `${verb} calls free identifier ${name}`,
      );
    }
  };

  traverse(parsed.ast, {
    AwaitExpression(path) {
      if (rejection) return;
      rejectIdentifierCall(
        path.get('argument'),
        'await_free_identifier_call',
        'await',
        { requireFree: true },
      );
    },

    YieldExpression(path) {
      if (rejection) return;
      rejectIdentifierCall(
        path.get('argument'),
        'yield_free_identifier_call',
        'yield',
        { requireFree: false },
      );
    },

    ReferencedIdentifier(path) {
      if (rejection) return;

      const name = path.node.name;
      if (CAPABILITY_IDENTIFIERS.has(name)) {
        rejectOnce(
          'capability_identifier',
          name,
          `references capability-bearing identifier ${name}`,
        );
      }
    },

    MemberExpression(path) {
      if (rejection) return;

      const namespace = figCapabilityNamespace(path.node);
      if (namespace) {
        rejectOnce(
          'capability_namespace',
          namespace,
          `references capability-bearing namespace ${namespace}`,
        );
      }
    },
  });

  if (rejection) {
    return { token_only: false, rejection, parse_error: null };
  }

  return { token_only: true, rejection: null, parse_error: null };
}

function parseSource(jsSource) {
  try {
    return {
      ast: parse(jsSource, {
        sourceType: 'module',
        allowReturnOutsideFunction: true,
        plugins: [],
      }),
      parse_error: null,
    };
  } catch (errModule) {
    try {
      return {
        ast: parse(`(${jsSource})`, {
          sourceType: 'module',
          allowReturnOutsideFunction: true,
          plugins: [],
        }),
        parse_error: null,
      };
    } catch (_errWrapped) {
      return {
        ast: null,
        parse_error: errModule.message,
      };
    }
  }
}

function rejected(code, { name = null, message, parse_error }) {
  const rejection = { code, message };
  if (name) rejection.name = name;
  return { token_only: false, rejection, parse_error };
}

function figCapabilityNamespace(node) {
  const chain = memberChain(node);
  if (!chain || chain.root !== 'fig' || chain.properties.length === 0) {
    return null;
  }

  const namespace = chain.properties[0];
  if (!FIG_CAPABILITY_NAMESPACES.has(namespace)) return null;
  return `fig.${namespace}`;
}

function memberChain(node) {
  const properties = [];
  let cursor = node;

  while (cursor && cursor.type === 'MemberExpression') {
    const property = memberPropertyName(cursor);
    if (!property) return null;
    properties.unshift(property);
    cursor = cursor.object;
  }

  if (!cursor || cursor.type !== 'Identifier') return null;
  return { root: cursor.name, properties };
}

function memberPropertyName(node) {
  if (!node.computed && node.property.type === 'Identifier') {
    return node.property.name;
  }
  if (node.property.type === 'StringLiteral') {
    return node.property.value;
  }
  return null;
}
