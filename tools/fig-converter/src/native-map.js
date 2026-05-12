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
 * Cargo uses `cargo metadata` for multiple package-spec domains. Only the
 * `--no-deps` package projection is a workspace-member list; metadata calls
 * without `--no-deps` include the full dependency graph and must stay JS-backed.
 */
function isCargoWorkspaceMembersGenerator(scriptArgv, postProcessSource) {
  if (!Array.isArray(scriptArgv)) return false;
  if (scriptArgv[0] !== 'cargo' || scriptArgv[1] !== 'metadata') return false;
  if (!scriptArgv.includes('--no-deps')) return false;
  if (typeof postProcessSource !== 'string') return false;
  if (/\.dependencies\b/.test(postProcessSource)) return false;
  return /JSON\.parse[\s\S]*\.packages\.map\s*\(/.test(postProcessSource);
}

function cargoTargetKind(postProcessSource) {
  if (typeof postProcessSource !== 'string') return null;
  if (!/JSON\.parse[\s\S]*\.packages[\s\S]*\.targets/.test(postProcessSource)) return null;
  for (const kind of ['bin', 'example', 'test', 'bench', 'lib']) {
    if (new RegExp(`kind\\s*\\.\\s*includes\\s*\\(\\s*["'\`]${kind}["'\`]\\s*\\)`).test(postProcessSource)) {
      return kind;
    }
    if (new RegExp(`kind\\s*===\\s*["'\`]${kind}["'\`]`).test(postProcessSource)) {
      return kind;
    }
  }
  return null;
}

function npmLocalProvider(postProcessSource) {
  if (typeof postProcessSource !== 'string') return null;
  if (!/JSON\.parse/.test(postProcessSource) || !/package\.json/.test(postProcessSource)) {
    return null;
  }
  const readsDeps = /\.dependencies\b/.test(postProcessSource);
  const readsDevDeps = /\.devDependencies\b/.test(postProcessSource);
  const readsOptionalDeps = /\.optionalDependencies\b/.test(postProcessSource);
  if (readsDeps && (readsDevDeps || readsOptionalDeps || /Object\.assign/.test(postProcessSource))) {
    return { type: 'npm_all_dependencies' };
  }
  if (readsDevDeps) return { type: 'npm_dev_dependencies' };
  if (readsDeps) return { type: 'npm_dependencies' };
  return null;
}

function awsProfileProvider(scriptArgv) {
  if (!Array.isArray(scriptArgv)) return null;
  if (
    scriptArgv.length === 3
    && scriptArgv[0] === 'aws'
    && scriptArgv[1] === 'configure'
    && scriptArgv[2] === 'list-profiles'
  ) {
    return { type: 'aws_profile_names' };
  }
  return null;
}

function dockerProvider(scriptArgv) {
  if (!Array.isArray(scriptArgv) || scriptArgv.length < 2) return null;
  const binary = scriptArgv[0];
  if (!['docker', 'podman'].includes(binary)) return null;
  const params = binary === 'docker' ? undefined : { binary };
  const withParams = (type) => (params ? { type, params } : { type });
  const sub = scriptArgv[1];
  const third = scriptArgv[2];
  if (sub === 'images' || (sub === 'image' && third === 'ls')) return withParams('docker_images');
  if (sub === 'ps' || (sub === 'container' && third === 'ls')) {
    const joined = scriptArgv.join(' ');
    if (/status=running/.test(joined)) return withParams('docker_running_containers');
    return withParams('docker_containers');
  }
  if (sub === 'network' && (third === 'ls' || third === 'list')) return withParams('docker_networks');
  if (sub === 'volume' && (third === 'ls' || third === 'list')) return withParams('docker_volumes');
  return null;
}

function kubectlProvider(scriptArgv) {
  if (!Array.isArray(scriptArgv) || scriptArgv.length < 2 || scriptArgv[0] !== 'kubectl') {
    return null;
  }
  if (scriptArgv[1] === 'api-resources') return { type: 'k8s_resources' };
  if (scriptArgv[1] === 'config' && scriptArgv[2] === 'get-contexts') {
    return { type: 'k8s_contexts' };
  }
  if (scriptArgv[1] !== 'get') return null;
  const resource = scriptArgv[2];
  const map = {
    pod: 'k8s_pods',
    pods: 'k8s_pods',
    namespace: 'k8s_namespaces',
    namespaces: 'k8s_namespaces',
    ns: 'k8s_namespaces',
    node: 'k8s_nodes',
    nodes: 'k8s_nodes',
    service: 'k8s_services',
    services: 'k8s_services',
    svc: 'k8s_services',
  };
  return map[resource] ? { type: map[resource] } : null;
}

function smallToolProvider(scriptArgv) {
  if (!Array.isArray(scriptArgv) || scriptArgv.length < 2) return null;
  const [binary, sub] = scriptArgv;
  if (binary === 'tmux') {
    if (sub === 'list-sessions' || sub === 'ls') return { type: 'tmux_sessions' };
    if (sub === 'list-windows' || sub === 'lsw') return { type: 'tmux_windows' };
    if (sub === 'list-panes' || sub === 'lsp') return { type: 'tmux_panes' };
    if (sub === 'list-clients' || sub === 'lsc') return { type: 'tmux_clients' };
  }
  if (binary === 'systemctl' && sub === 'list-units') {
    if (scriptArgv.includes('--user')) return { type: 'systemd_user_units' };
    if (scriptArgv.includes('--state=active') || scriptArgv.includes('--state') && scriptArgv.includes('active')) {
      return { type: 'systemd_active_units' };
    }
    return { type: 'systemd_units' };
  }
  if (binary === 'brew') {
    if (sub === 'list' && scriptArgv.includes('--cask')) return { type: 'brew_casks_installed' };
    if (sub === 'list') return { type: 'brew_formulae_installed' };
    if (sub === 'search') return { type: 'brew_formulae_searchable' };
  }
  if (binary === 'dscl' && scriptArgv[1] === '.' && scriptArgv[2] === 'list') {
    if (scriptArgv[3] === '/Users') return { type: 'dscl_users' };
    if (scriptArgv[3] === '/Groups') return { type: 'dscl_groups' };
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

  if (key === 'cargo metadata') {
    const kind = cargoTargetKind(postProcessSource);
    if (kind) {
      return { type: 'cargo_targets', params: { kind } };
    }
    if (
      specName === 'cargo'
      && isCargoWorkspaceMembersGenerator(scriptArgv, postProcessSource)
    ) {
      return { type: 'cargo_workspace_members' };
    }
    return null;
  }

  if (key === 'npm prefix') {
    const provider = npmLocalProvider(postProcessSource);
    if (provider) return provider;
  }

  const awsProfile = awsProfileProvider(scriptArgv);
  if (awsProfile) return awsProfile;

  const docker = dockerProvider(scriptArgv);
  if (docker) return docker;

  const kubectl = kubectlProvider(scriptArgv);
  if (kubectl) return kubectl;

  const smallTool = smallToolProvider(scriptArgv);
  if (smallTool) return smallTool;

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
  if (
    (specName === 'kubectl' || specName === 'kubecolor')
    && /kubectl[\s\S]*config[\s\S]*get-contexts/.test(jsSource)
  ) {
    return { type: 'k8s_contexts' };
  }
  return null;
}
