import { parse } from '@babel/parser';
import { matchPostProcess } from './post-process-matcher.js';

const FILESYSTEM_COMMANDS = new Set(['cat', 'ls']);
const SHELL_COMMANDS = new Set(['bash', 'sh', 'zsh']);
const NETWORK_COMMANDS = new Set(['curl', 'wget']);

/**
 * Detect Fig custom/script functions that only wrap one literal subprocess
 * call and then post-process stdout. Returns a conversion result or a
 * structured decline; callers decide whether to preserve the old JS path.
 */
export function extractSubprocessGenerator(source) {
  const parsed = parseFunctionNode(source);
  if (!parsed) return decline('parse-error');

  const { fnNode, code } = parsed;
  if (!['ArrowFunctionExpression', 'FunctionExpression'].includes(fnNode.type)) {
    return decline('not-function');
  }
  if (!fnNode.async) return decline('not-async');

  const runnerName = identifierParamName(fnNode.params?.[1]);
  if (!runnerName) return decline('missing-runner-param');

  if (source.includes('fetch(') || source.includes('fetch (')) {
    return decline('fetch-reference');
  }

  const awaits = [];
  walk(fnNode.body, (node) => {
    if (node.type === 'AwaitExpression') awaits.push(node);
  });

  if (awaits.length === 0) return decline('no-await');
  if (awaits.length > 1) return decline('multiple-awaits');

  const awaitNode = awaits[0];
  const call = awaitNode.argument;
  if (
    call?.type !== 'CallExpression' ||
    call.callee?.type !== 'Identifier' ||
    call.callee.name !== runnerName
  ) {
    return decline('no-subprocess-await');
  }

  const callArg = call.arguments?.[0];
  if (callArg?.type !== 'ObjectExpression') {
    return decline('nonliteral-subprocess-descriptor');
  }

  const commandNode = objectPropertyValue(callArg, 'command');
  const argsNode = objectPropertyValue(callArg, 'args');
  if (!commandNode) return decline('missing-command');
  if (!argsNode) return decline('missing-args');

  const tokensName = identifierParamName(fnNode.params?.[0]);
  if (tokensName && referencesIdentifier(commandNode, tokensName)) {
    return decline('tokens-derived-command');
  }
  if (tokensName && referencesIdentifier(argsNode, tokensName)) {
    return decline('tokens-derived-args');
  }

  const command = staticStringValue(commandNode);
  if (command === null) return decline('nonliteral-command');

  const args = staticStringArray(argsNode);
  if (args === null) return decline('nonliteral-args');

  const filesystemReason = filesystemDeclineReason(command, args);
  if (filesystemReason) return decline(filesystemReason);
  if (NETWORK_COMMANDS.has(command)) return decline('network-bound-command');
  if (command === 'git') return decline('git-subprocess-not-native');

  const block = fnNode.body;
  if (block?.type !== 'BlockStatement') return decline('expression-body');

  const awaitStmtIndex = block.body.findIndex((stmt) => containsNode(stmt, awaitNode));
  if (awaitStmtIndex < 0) return decline('await-not-top-level');

  const stdoutName = stdoutBindingName(block.body[awaitStmtIndex], awaitNode);
  if (!stdoutName) return decline('unsupported-stdout-binding');

  const postStatements = block.body.slice(awaitStmtIndex + 1);
  if (postStatements.length === 0) return decline('missing-postprocess');
  if (tokensName && postStatements.some((stmt) => referencesIdentifier(stmt, tokensName))) {
    return decline('tokens-derived-postprocess');
  }
  if (postStatements.some((stmt) => referencesIdentifier(stmt, runnerName))) {
    return decline('nested-subprocess-postprocess');
  }

  const bodySource = postStatements
    .map((stmt) => code.slice(stmt.start, stmt.end))
    .join('\n');
  const postProcessSource = `function(${stdoutName}) { ${bodySource} }`;
  const match = matchPostProcess(postProcessSource);
  if (!match.requires_js) {
    return {
      kind: 'native',
      script: [command, ...args],
      transforms: match.transforms,
      _static_extracted_subprocess: true,
    };
  }

  // SPEC § A point 4 — "If the post-processing exceeds what the
  // transform pipeline can express, leave the JS body but set
  // `kind: post_process` and emit a working `js_runtime`." We already
  // verified the post-statement body references neither `tokens` nor
  // the runner (lines 88-93), so what remains is a pure
  // stdout-to-suggestions JS function — exactly what the supported
  // PostProcess path expects. Lift the script natively; keep the body
  // as the post-process JS source.
  return {
    kind: 'fallback_post_process',
    script: [command, ...args],
    fallbackJsSource: postProcessSource,
  };
}

function decline(reason) {
  return { kind: 'declined', reason };
}

function parseFunctionNode(source) {
  for (const code of [
    `const __ghost_subprocess = ${source};`,
    `const __ghost_subprocess = (${source});`,
  ]) {
    try {
      const ast = parse(code, { sourceType: 'module' });
      let fnNode = null;
      walk(ast, (node) => {
        if (
          !fnNode &&
          node.type === 'VariableDeclarator' &&
          node.id?.type === 'Identifier' &&
          node.id.name === '__ghost_subprocess'
        ) {
          fnNode = node.init;
        }
      });
      if (fnNode) return { fnNode, code };
    } catch {
      // Try the parenthesized fallback.
    }
  }
  return null;
}

function identifierParamName(param) {
  if (!param) return null;
  if (param.type === 'Identifier') return param.name;
  if (param.type === 'AssignmentPattern' && param.left?.type === 'Identifier') {
    return param.left.name;
  }
  return null;
}

function objectPropertyValue(obj, keyName) {
  for (const prop of obj.properties ?? []) {
    if (prop.type !== 'ObjectProperty') continue;
    const key =
      prop.key?.type === 'Identifier'
        ? prop.key.name
        : prop.key?.type === 'StringLiteral'
          ? prop.key.value
          : null;
    if (key === keyName) return prop.value;
  }
  return null;
}

function staticStringValue(node) {
  if (node?.type === 'StringLiteral') return node.value;
  if (node?.type === 'TemplateLiteral' && node.expressions.length === 0) {
    return node.quasis[0]?.value?.cooked ?? node.quasis[0]?.value?.raw ?? '';
  }
  return null;
}

function staticStringArray(node) {
  if (node?.type !== 'ArrayExpression') return null;
  const values = [];
  for (const el of node.elements ?? []) {
    const value = staticStringValue(el);
    if (value === null) return null;
    values.push(value);
  }
  return values;
}

function filesystemDeclineReason(command, args) {
  if (FILESYSTEM_COMMANDS.has(command)) return 'filesystem-read';
  if (SHELL_COMMANDS.has(command)) {
    const script = args.join(' ');
    if (/(^|[;&|`$()\s])(?:cat|ls)(?:\s|$|[`;&|)])/.test(script)) {
      return 'filesystem-read';
    }
  }
  return null;
}

function stdoutBindingName(stmt, awaitNode) {
  if (stmt.type !== 'VariableDeclaration') return null;
  for (const decl of stmt.declarations ?? []) {
    if (decl.init !== awaitNode) continue;
    if (decl.id?.type !== 'ObjectPattern') return null;
    for (const prop of decl.id.properties ?? []) {
      if (prop.type !== 'ObjectProperty') continue;
      const key =
        prop.key?.type === 'Identifier'
          ? prop.key.name
          : prop.key?.type === 'StringLiteral'
            ? prop.key.value
            : null;
      if (key === 'stdout' && prop.value?.type === 'Identifier') {
        return prop.value.name;
      }
    }
  }
  return null;
}

function containsNode(root, target) {
  let found = false;
  walk(root, (node) => {
    if (node === target) found = true;
  });
  return found;
}

function referencesIdentifier(root, name) {
  let found = false;
  walk(root, (node) => {
    if (node.type === 'Identifier' && node.name === name) {
      found = true;
    }
  });
  return found;
}

function walk(node, visitor) {
  if (!node || typeof node !== 'object') return;
  if (Array.isArray(node)) {
    for (const item of node) walk(item, visitor);
    return;
  }

  visitor(node);
  for (const [key, value] of Object.entries(node)) {
    if (
      key === 'loc' ||
      key === 'start' ||
      key === 'end' ||
      key === 'leadingComments' ||
      key === 'trailingComments' ||
      key === 'innerComments' ||
      key === 'extra'
    ) {
      continue;
    }
    walk(value, visitor);
  }
}
