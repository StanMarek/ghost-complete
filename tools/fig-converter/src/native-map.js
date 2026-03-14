/**
 * native-map.js
 *
 * Maps known Fig generator scripts to Ghost Complete's native Rust generators.
 * When a Fig spec uses `script: ["git", "branch", ...]`, we can emit a much
 * faster native generator instead of running a shell command + transform pipeline.
 */

/**
 * Map of known script commands to native Ghost Complete generator types.
 * Keys are the first two elements of the script array joined by space.
 */
const NATIVE_GENERATOR_MAP = {
  'git branch': { type: 'git_branches' },
  'git tag': { type: 'git_tags' },
  'git remote': { type: 'git_remotes' },
};

/**
 * Check if a script command matches a native Ghost Complete generator.
 *
 * @param {string} specName - The spec name (for context, currently unused)
 * @param {string[]} scriptArgv - The script command as an array
 * @returns {object|null} Native generator spec (e.g., { type: 'git_branches' }) or null
 */
export function matchNativeGenerator(specName, scriptArgv) {
  if (!Array.isArray(scriptArgv) || scriptArgv.length < 2) return null;
  const key = scriptArgv.slice(0, 2).join(' ');
  return NATIVE_GENERATOR_MAP[key] || null;
}
