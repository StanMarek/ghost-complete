/**
 * post-process-matcher.js
 *
 * Analyzes Fig postProcess function bodies (as strings) and emits equivalent
 * Ghost Complete transform arrays. This is intentionally heuristic — unrecognized
 * patterns gracefully degrade to requires_js: true.
 *
 * IMPORTANT: Parameterized transforms use INTERNALLY-TAGGED format:
 *   {"type": "error_guard", "starts_with": "Error:"}
 *   {"type": "regex_extract", "pattern": "...", "name": 1}
 *   {"type": "json_extract", "name": "Name", "description": "Status"}
 * NOT externally-tagged like {"error_guard": {"starts_with": "Error:"}}.
 */

/**
 * Analyze a postProcess function body and return a match result.
 *
 * @param {string} fnSource - The function source code (from .toString())
 * @returns {{ transforms: Array, requires_js: boolean, js_source?: string }}
 */
export function matchPostProcess(fnSource) {
  if (!fnSource || typeof fnSource !== 'string') {
    return { transforms: null, requires_js: true, js_source: fnSource || '' };
  }

  // Extract the function body (strip the outer function(...) { ... } wrapper)
  const body = extractFunctionBody(fnSource);

  const transforms = [];

  // Phase 1: Check for error guard prefix
  const errorGuard = matchErrorGuard(body);
  if (errorGuard) {
    transforms.push(errorGuard);
  }

  // Phase 2: Look for the split pattern (required for most transforms)
  if (!hasSplitPattern(body)) {
    // No split found — this is likely a complex function we can't match
    return { transforms: null, requires_js: true, js_source: fnSource };
  }

  // Always add split_lines + filter_empty as base
  transforms.push('split_lines', 'filter_empty');

  // Phase 3: Check for trim patterns
  if (hasTrimPattern(body)) {
    transforms.push('trim');
  }

  // Phase 4: Check for JSON.parse extraction
  const jsonExtract = matchJsonExtract(body);
  if (jsonExtract) {
    transforms.push(jsonExtract);
    return { transforms, requires_js: false };
  }

  // Phase 5: Check for regex extraction
  const regexExtract = matchRegexExtract(body);
  if (regexExtract) {
    transforms.push(regexExtract);
    return { transforms, requires_js: false };
  }

  // Phase 6: Check for substring/column extraction
  const columnExtract = matchColumnExtract(body);
  if (columnExtract) {
    transforms.push(columnExtract);
    return { transforms, requires_js: false };
  }

  // Phase 7: Check if the body has complex logic we can't handle BEFORE
  // checking for simple split+map, to avoid false positives
  if (hasComplexLogic(body)) {
    return { transforms: null, requires_js: true, js_source: fnSource };
  }

  // Phase 8: If we have a simple split + map to {name: line} pattern, that's fine
  if (isSimpleSplitMap(body)) {
    return { transforms, requires_js: false };
  }

  // If we got here with just split+filter, that's still a valid basic match
  return { transforms, requires_js: false };
}

// --- Pattern matchers ---

/**
 * Extract the body of a function from its source string.
 * Handles: function(x) { ... }, (x) => { ... }, x => ...
 */
function extractFunctionBody(source) {
  // Remove leading/trailing whitespace
  let s = source.trim();

  // Arrow function with block body: (...) => { ... } or x => { ... }
  const arrowBlock = s.match(/^(?:\(.*?\)|[a-zA-Z_$]\w*)\s*=>\s*\{([\s\S]*)\}$/);
  if (arrowBlock) return arrowBlock[1];

  // Arrow function with expression body: (...) => expr or x => expr
  const arrowExpr = s.match(/^(?:\(.*?\)|[a-zA-Z_$]\w*)\s*=>\s*([\s\S]+)$/);
  if (arrowExpr) return arrowExpr[1];

  // Function declaration/expression: function(...) { ... }
  const funcBlock = s.match(/^function\s*\(.*?\)\s*\{([\s\S]*)\}$/);
  if (funcBlock) return funcBlock[1];

  // Named function: function name(...) { ... }
  const namedFunc = s.match(/^function\s+\w+\s*\(.*?\)\s*\{([\s\S]*)\}$/);
  if (namedFunc) return namedFunc[1];

  return s;
}

/**
 * Match error guard patterns on the RAW output (before split).
 * Must appear in a conditional context with return [] or ternary ? [] : ...
 *
 * Valid patterns:
 * - if (out.startsWith("fatal:")) return []
 * - if (out.includes("error")) return []
 * - out.startsWith("fatal:") ? [] : ...
 *
 * NOT matched: .filter(e => !e.includes("=")) — this is per-line filtering,
 * not an error guard on the raw output.
 */
function matchErrorGuard(body) {
  // Pattern 1: if (...startsWith("...")) return []
  // The key is: startsWith must appear in an if/ternary before .split(), not inside .filter()
  const startsWithGuard = body.match(
    /(?:if\s*\(|\.startsWith\s*\(\s*["'`]([^"'`]+)["'`]\s*\)\s*\?\s*\[)/
  );
  if (startsWithGuard) {
    const startsWithMatch = body.match(
      /\.startsWith\s*\(\s*["'`]([^"'`]+)["'`]\s*\)\s*(?:\)\s*return\s*\[\s*\]|\?\s*\[)/
    );
    if (startsWithMatch) {
      return { type: 'error_guard', starts_with: startsWithMatch[1] };
    }
  }

  // Pattern 2: if (...includes("...")) return []
  // Must be in a guard context (if/return[]), not inside .filter()
  const includesGuard = body.match(
    /if\s*\(\s*\w+\.includes\s*\(\s*["'`]([^"'`]+)["'`]\s*\)\s*\)\s*return\s*\[\s*\]/
  );
  if (includesGuard) {
    return { type: 'error_guard', contains: includesGuard[1] };
  }

  return null;
}

/**
 * Check if the body contains a split-by-newline pattern.
 */
function hasSplitPattern(body) {
  // .split("\n") or .split('\n') or .split(`\n`) or .split(/\n/)
  return /\.split\s*\(\s*(?:["'`]\\n["'`]|["'`]\n["'`]|\/\\n\/)\s*\)/.test(body);
}

/**
 * Check if the body contains a trim pattern.
 * e.g., .trim() on each line, or .filter(Boolean) which implicitly trims
 */
function hasTrimPattern(body) {
  return /\.trim\s*\(\s*\)/.test(body);
}

/**
 * Match JSON.parse extraction patterns.
 * e.g., JSON.parse(line) with field access like .Name, .Status
 */
function matchJsonExtract(body) {
  if (!body.includes('JSON.parse')) return null;

  // Strategy 1: Direct chained access — JSON.parse(x).Field
  const directField = body.match(
    /JSON\.parse\s*\([^)]+\)\s*\.(\w+)/
  );

  // Strategy 2: Bracket access — JSON.parse(x)["field"]
  const bracketField = body.match(
    /JSON\.parse\s*\([^)]+\)\s*\[\s*["'`](\w+)["'`]\s*\]/
  );

  // Strategy 3: Variable assignment — const/let/var x = JSON.parse(...); ... x.Field
  // Look for: (const|let|var) <name> = JSON.parse(...) then later <name>.<field>
  let varField = null;
  const varAssign = body.match(
    /(?:const|let|var)\s+(\w+)\s*=\s*JSON\.parse\s*\([^)]+\)/
  );
  if (varAssign) {
    const varName = varAssign[1];
    // Look for name: <varName>.<field> in the body
    const nameAccess = body.match(
      new RegExp(`name\\s*:\\s*${varName}\\.(\\w+)`)
    );
    if (nameAccess) {
      varField = nameAccess[1];
    }
  }

  const field = directField ? directField[1]
    : bracketField ? bracketField[1]
    : varField ? varField
    : null;

  if (field) {
    // Try to find a description field too
    const descMatch = body.match(
      /description\s*:\s*(?:\w+)\.(\w+)/
    );
    const result = { type: 'json_extract', name: field };
    if (descMatch && descMatch[1] !== field) {
      result.description = descMatch[1];
    }
    return result;
  }

  // Generic JSON.parse without clear field access
  return { type: 'json_extract', name: 'name' };
}

/**
 * Match regex extraction patterns.
 * e.g., line.match(/pattern/) with capture group access [1]
 */
function matchRegexExtract(body) {
  // Look for .match(/pattern/) patterns
  const regexMatch = body.match(
    /\.match\s*\(\s*\/([^/]+)\/[gimsuy]*\s*\)/
  );
  if (!regexMatch) return null;

  const pattern = regexMatch[1];

  // Look for capture group access: m[1], match[1], etc.
  const nameGroupMatch = body.match(
    /(?:name\s*:\s*)?(?:\w+)\s*\[\s*(\d+)\s*\]/
  );
  const nameGroup = nameGroupMatch ? parseInt(nameGroupMatch[1], 10) : 1;

  // Look for description group access
  const descGroupMatch = body.match(
    /description\s*:\s*\w+\s*\[\s*(\d+)\s*\]/
  );

  const result = { type: 'regex_extract', pattern, name: nameGroup };
  if (descGroupMatch) {
    result.description = parseInt(descGroupMatch[1], 10);
  }
  return result;
}

/**
 * Match column/substring extraction.
 * e.g., line.substring(0, 7), line.slice(0, 7)
 */
function matchColumnExtract(body) {
  const substringMatch = body.match(
    /\.(?:substring|slice)\s*\(\s*(\d+)\s*,\s*(\d+)\s*\)/
  );
  if (substringMatch) {
    return {
      type: 'column_extract',
      column: parseInt(substringMatch[1], 10),
    };
  }
  return null;
}

/**
 * Check if the body is a simple split + map to suggestion objects.
 * Matches patterns like:
 *   out.split("\n").map(line => ({ name: line }))
 *   out.split("\n").filter(Boolean).map(e => ({ name: e }))
 *   out.split("\n").filter(x => x !== "").map(...)
 */
function isSimpleSplitMap(body) {
  // Check for .map that produces {name: <var>} objects
  // This is intentionally loose — if we see split + map + name:, it's probably simple
  return /\.split\b/.test(body) && /\.map\b/.test(body) && /name\s*:/.test(body);
}

/**
 * Check if the function body contains complex logic that we can't convert
 * to a declarative transform chain.
 */
function hasComplexLogic(body) {
  // Multiple return statements (beyond error guard + main return)
  const returnCount = (body.match(/\breturn\b/g) || []).length;
  if (returnCount > 2) return true;

  // for/while loops (map/filter are fine, explicit loops suggest complexity)
  if (/\b(?:for|while)\s*\(/.test(body)) return true;

  // try/catch
  if (/\btry\s*\{/.test(body)) return true;

  // Multiple variable assignments suggesting state tracking
  const letCount = (body.match(/\blet\s+/g) || []).length;
  const varCount = (body.match(/\bvar\s+/g) || []).length;
  if (letCount + varCount > 3) return true;

  // Set/Map usage
  if (/\bnew\s+(?:Set|Map)\b/.test(body)) return true;

  return false;
}
