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
  'multipass list': { type: 'multipass_list' },
  'defaults domains': { type: 'defaults_domains' },
  'pandoc --list-input-formats': { type: 'pandoc_input_formats' },
  'pandoc --list-output-formats': { type: 'pandoc_output_formats' },
  'ansible-doc --list': { type: 'ansible_doc_modules' },
  // `cargo metadata --format-version 1 --no-deps` is the canonical
  // workspace-listing script across all `cargo run -p` / `cargo build
  // -p` / `cargo test -p` arg positions in the upstream fig spec. The
  // first two tokens (`cargo metadata`) match every variant.
  'cargo metadata': { type: 'cargo_workspace_members' },
};

/**
 * Spec-scoped mappings: `(specName, scriptKey)` → provider. Used when the
 * same subprocess appears in multiple specs but we only want to route one
 * of them through the native provider (e.g., `conda env list` appears in
 * both `mamba.json` and `conda.json`; we have a `mamba_envs` provider
 * but no `conda_envs` provider yet, so the conda spec stays requires_js).
 */
const SPEC_SCOPED_MAP = {
  mamba: {
    'conda env': { type: 'mamba_envs' },
  },
  // `npm run <TAB>`'s upstream generator is a `bash -c` wrapper that
  // walks up to the nearest `package.json`, then hands the file body
  // to a JS post-processor that projects `scripts` keys. The `bash -c`
  // key is too generic to live in `NATIVE_GENERATOR_MAP` (it would
  // false-match every shell-out spec), so it lives here gated by the
  // spec name AND a post-process-source regex confirming we're really
  // looking at the package.json scripts extractor — not some unrelated
  // bash invocation that happens to share the prefix.
  npm: {
    'bash -c': {
      type: 'npm_scripts',
      requirePostProcessMatch: /JSON\.parse[\s\S]*\.scripts/,
    },
  },
};

/**
 * Flags passed to the driver command that don't change the subprocess's output
 * shape and therefore should not affect which native generator matches. Upstream
 * fig specs use these as "safe" prefixes (e.g., `git --no-optional-locks branch`
 * to avoid updating `index.lock`), but for the purposes of native-map dispatch
 * they're semantically identical to the bare form.
 *
 * Keyed by the driver command (first argv token) so we only strip flags we've
 * explicitly audited as no-ops for THAT command.
 *
 * See docs/phase-minus-1-followups.md §1 for the rationale: the previous regen
 * deferred git.json because the fig source uses this prefix and the old matcher
 * couldn't see through it.
 */
const NO_OP_DRIVER_FLAGS = {
  git: new Set(['--no-optional-locks']),
};

/**
 * Derive the two-token matching key from a script argv, skipping any
 * driver-specific no-op flags between the driver command and its first real
 * subcommand. Returns `null` if the script is too short to produce a key.
 */
function deriveKey(scriptArgv) {
  if (!Array.isArray(scriptArgv) || scriptArgv.length < 2) return null;
  const driver = scriptArgv[0];
  const noops = NO_OP_DRIVER_FLAGS[driver];
  if (!noops) return scriptArgv.slice(0, 2).join(' ');
  for (let i = 1; i < scriptArgv.length; i++) {
    if (!noops.has(scriptArgv[i])) {
      return `${driver} ${scriptArgv[i]}`;
    }
  }
  return null;
}

/**
 * Check if a script command matches a native Ghost Complete generator.
 *
 * @param {string} specName - The spec name (used for spec-scoped mappings and arduino disambiguation)
 * @param {string[]} scriptArgv - The script command as an array
 * @param {string} [postProcessSource] - Stringified postProcess function, used to
 *   disambiguate generators that share the same script (e.g., arduino-cli board list
 *   is used for both fqbn and port suggestions).
 * @returns {object|null} Native generator spec (e.g., { type: 'git_branches' }) or null
 */
export function matchNativeGenerator(specName, scriptArgv, postProcessSource) {
  const key = deriveKey(scriptArgv);
  if (key === null) return null;

  // arduino-cli: boards vs ports share the same key, disambiguated by postProcess.
  if (key === 'arduino-cli board' && typeof postProcessSource === 'string') {
    // FQBN shape: description templates include `port.address` but the
    // suggestion NAME is the fqbn. Be specific — match on the fqbn token
    // in the `name:` position so we don't mistake port-extractor sources
    // that ALSO mention fqbn for context.
    if (/name:\s*[a-zA-Z_$]+\.matching_boards\[0\]\.fqbn/.test(postProcessSource)) {
      return { type: 'arduino_cli_boards' };
    }
    // Port-address shape: suggestion NAME is port.address, description
    // contains the "port connection" substring (exact match on the JS
    // source from the real fig spec).
    if (
      /name:\s*[a-zA-Z_$]+\.port\.address/.test(postProcessSource)
      && postProcessSource.includes('port connection')
    ) {
      return { type: 'arduino_cli_ports' };
    }
    return null;
  }

  // Spec-scoped lookup (overrides global for specific specs).
  const scoped = SPEC_SCOPED_MAP[specName];
  if (scoped && scoped[key]) {
    const entry = scoped[key];
    // Optional post-process-source predicate: when set, the entry only
    // matches if the post-process JS actually reads what we expect.
    // Prevents false-firing on `bash -c` for unrelated specs.
    if (entry.requirePostProcessMatch) {
      if (
        typeof postProcessSource !== 'string'
        || !entry.requirePostProcessMatch.test(postProcessSource)
      ) {
        return null;
      }
      const { requirePostProcessMatch, ...rest } = entry;
      return rest;
    }
    return entry;
  }

  return NATIVE_GENERATOR_MAP[key] || null;
}

/**
 * Maps `_scriptFunction` generators (where the upstream fig spec used
 * `script: () => "..."`) to a native provider by inspecting the
 * stringified JS source. Used for the cases where there is no
 * `script` array to key on at all — the entire generator was a JS
 * function in the source.
 *
 * Currently handles `make`'s `make -qp | awk ...` shape, which is the
 * only `_scriptFunction` upstream generator that has a known native
 * Rust replacement.
 *
 * @param {string} specName - The spec name (e.g., 'make').
 * @param {string} jsSource - Stringified JS function from `_scriptSource`.
 * @returns {object|null} Native generator spec or null.
 */
export function matchNativeFromJsSource(specName, jsSource) {
  if (typeof jsSource !== 'string' || jsSource.length === 0) return null;
  if (specName === 'make' && /make\s+-qp/.test(jsSource)) {
    return { type: 'makefile_targets' };
  }
  return null;
}
