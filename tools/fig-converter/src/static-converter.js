/**
 * static-converter.js
 *
 * Converts a Fig spec object (from @withfig/autocomplete) to Ghost Complete's
 * JSON spec format. Handles the static structure: subcommands, options, args,
 * descriptions, and templates. Generators are passed through for further
 * processing by the full pipeline.
 */

/**
 * Convert a Fig spec object to Ghost Complete JSON format.
 * @param {object} figSpec - The raw Fig spec object (default export from a .js file)
 * @returns {object} Ghost Complete spec object
 */
export function convertSpec(figSpec) {
  if (!figSpec || typeof figSpec !== 'object') {
    throw new Error('Invalid spec: expected an object');
  }

  const result = {};

  // Name can be string or string[]
  if (figSpec.name !== undefined) {
    result.name = normalizeName(figSpec.name);
  }

  if (figSpec.description) {
    result.description = String(figSpec.description);
  }

  // Convert subcommands
  if (figSpec.subcommands && Array.isArray(figSpec.subcommands)) {
    result.subcommands = figSpec.subcommands
      .map(convertSubcommand)
      .filter(Boolean);
  }

  // Convert options
  if (figSpec.options && Array.isArray(figSpec.options)) {
    result.options = figSpec.options.map(convertOption).filter(Boolean);
  }

  // Convert args
  if (figSpec.args !== undefined) {
    result.args = convertArgs(figSpec.args);
  }

  return result;
}

/**
 * Convert a Fig subcommand to Ghost Complete format.
 */
function convertSubcommand(figSub) {
  if (!figSub || typeof figSub !== 'object') return null;

  const result = {};

  if (figSub.name !== undefined) {
    result.name = normalizeName(figSub.name);
  }

  if (figSub.description) {
    result.description = String(figSub.description);
  }

  if (typeof figSub.priority === 'number') {
    result.priority = figSub.priority;
  }

  // Recurse into nested subcommands
  if (figSub.subcommands && Array.isArray(figSub.subcommands)) {
    result.subcommands = figSub.subcommands
      .map(convertSubcommand)
      .filter(Boolean);
  }

  // Convert options
  if (figSub.options && Array.isArray(figSub.options)) {
    result.options = figSub.options.map(convertOption).filter(Boolean);
  }

  // Convert args
  if (figSub.args !== undefined) {
    result.args = convertArgs(figSub.args);
  }

  // Preserve loadSpec reference for later resolution
  if (figSub.loadSpec !== undefined) {
    result._loadSpec = figSub.loadSpec;
  }

  return result;
}

/**
 * Convert a Fig option to Ghost Complete format.
 * Ghost Complete expects: { name: ["-f", "--flag"], description: "...", args: ... }
 */
function convertOption(figOpt) {
  if (!figOpt || typeof figOpt !== 'object') return null;

  const result = {};

  // Fig option name can be string or string[] — Ghost Complete always uses an array
  result.name = normalizeNameArray(figOpt.name);

  if (figOpt.description) {
    result.description = String(figOpt.description);
  }

  if (typeof figOpt.priority === 'number') {
    result.priority = figOpt.priority;
  }

  // Convert option args
  if (figOpt.args !== undefined) {
    result.args = convertArgs(figOpt.args);
  }

  return result;
}

/**
 * Convert Fig args (single object or array) to Ghost Complete format.
 * Returns an array if the input is an array, a single object otherwise.
 */
function convertArgs(figArgs) {
  if (Array.isArray(figArgs)) {
    return figArgs.map(convertSingleArg).filter(Boolean);
  }
  return convertSingleArg(figArgs);
}

/**
 * Convert a single Fig arg to Ghost Complete format.
 */
function convertSingleArg(figArg) {
  if (!figArg || typeof figArg !== 'object') return null;

  const result = {};

  if (figArg.name) {
    result.name = String(figArg.name);
  }

  if (figArg.description) {
    result.description = String(figArg.description);
  }

  // Template: "filepaths", "folders", or array of templates
  if (figArg.template) {
    result.template = normalizeTemplate(figArg.template);
  }

  // isVariadic
  if (figArg.isVariadic) {
    result.isVariadic = true;
  }

  // isOptional
  if (figArg.isOptional) {
    result.isOptional = true;
  }

  // Suggestions (static list)
  if (figArg.suggestions && Array.isArray(figArg.suggestions)) {
    result.suggestions = figArg.suggestions
      .map(convertSuggestion)
      .filter(Boolean);
  }

  // Generators — convert to Ghost Complete format
  // These will be further processed by the full pipeline (native map, postProcess matcher, etc.)
  if (figArg.generators) {
    result.generators = convertGenerators(figArg.generators);
  }

  return result;
}

/**
 * Convert generators. At this stage we do a structural conversion,
 * preserving script/postProcess/splitOn/custom for the full pipeline to handle.
 */
function convertGenerators(figGens) {
  const gens = Array.isArray(figGens) ? figGens : [figGens];
  return gens.map(convertSingleGenerator).filter(Boolean);
}

/**
 * Convert a single Fig generator to an intermediate Ghost Complete generator.
 *
 * Possible Fig generator shapes:
 * - { template: "filepaths" } — template-only generator
 * - { script: [...], postProcess: fn } — shell command + transform
 * - { script: [...], splitOn: "\n" } — shell command + split
 * - { script: fn, ... } — dynamic script (requires JS)
 * - { custom: fn } — async custom generator (requires JS)
 * - { type: "..." } — direct type reference (already Ghost Complete format)
 */
function convertSingleGenerator(figGen) {
  if (!figGen || typeof figGen !== 'object') return null;

  const result = {};

  // Template-only generator
  if (figGen.template) {
    result.template = normalizeTemplate(figGen.template);
  }

  // Script: can be string[], string, or function
  if (figGen.script !== undefined) {
    if (typeof figGen.script === 'function') {
      result._scriptFunction = true;
      result._scriptSource = figGen.script.toString();
    } else if (Array.isArray(figGen.script)) {
      result.script = figGen.script;
    } else if (typeof figGen.script === 'string') {
      // Fig sometimes uses a single string command — split on spaces
      // but preserve it as an array for Ghost Complete
      result.script = figGen.script.split(/\s+/);
    }
  }

  // postProcess function — preserve for pattern matching
  if (typeof figGen.postProcess === 'function') {
    result._postProcess = figGen.postProcess;
    result._postProcessSource = figGen.postProcess.toString();
  }

  // splitOn — trivial transform
  if (figGen.splitOn !== undefined) {
    result._splitOn = figGen.splitOn;
  }

  // Custom async generator — requires JS
  if (typeof figGen.custom === 'function') {
    result._custom = true;
    result._customSource = figGen.custom.toString();
  }

  // getQueryTerm — used for filterStrategy, not critical for conversion
  if (figGen.filterTerm) {
    result._filterTerm = figGen.filterTerm;
  }

  // Cache configuration
  if (figGen.cache) {
    if (typeof figGen.cache === 'object') {
      result.cache = {};
      if (figGen.cache.ttl !== undefined) {
        // Fig uses milliseconds, Ghost Complete uses seconds
        result.cache.ttl_seconds = Math.ceil(figGen.cache.ttl / 1000);
      }
      if (figGen.cache.cacheByDirectory !== undefined) {
        result.cache.cache_by_directory = figGen.cache.cacheByDirectory;
      }
    }
  }

  // scriptTimeout
  if (figGen.scriptTimeout !== undefined) {
    result._scriptTimeout = figGen.scriptTimeout;
  }

  return result;
}

/**
 * Convert a Fig suggestion to Ghost Complete format.
 */
function convertSuggestion(figSug) {
  if (typeof figSug === 'string') {
    return { name: figSug };
  }
  if (!figSug || typeof figSug !== 'object') return null;

  const result = {};

  if (figSug.name !== undefined) {
    result.name = normalizeName(figSug.name);
  }

  if (figSug.description) {
    result.description = String(figSug.description);
  }

  // Strip icon (Ghost Complete uses kind chars, not icons)

  return result;
}

// --- Helpers ---

/**
 * Normalize a Fig name (string or string[]) to a single string.
 * For the top-level spec name, we want a single string.
 * For subcommands with aliases (e.g. ["ls", "list"]), use the first name.
 */
function normalizeName(name) {
  if (Array.isArray(name)) {
    // Use first element as the canonical name
    return String(name[0]);
  }
  return String(name);
}

/**
 * Normalize a name to always be an array (for options).
 */
function normalizeNameArray(name) {
  if (Array.isArray(name)) {
    return name.map(String);
  }
  return [String(name)];
}

/**
 * Normalize Fig template to Ghost Complete format.
 * Fig uses: "filepaths", "folders", or ["filepaths", "folders"]
 * Ghost Complete uses the same.
 */
function normalizeTemplate(template) {
  if (Array.isArray(template)) {
    return template.map(String);
  }
  return String(template);
}
