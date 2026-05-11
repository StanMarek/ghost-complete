import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { matchNativeFromJsSource, matchNativeGenerator } from './native-map.js';

// Real postProcess JS source strings taken from specs/arduino-cli.json (the
// pre-regen output). Used as fixtures for the arduino-cli disambiguation tests.
const ARDUINO_FQBN_POSTPROCESS = "t=>{try{return JSON.parse(t).filter(i=>i.matching_boards).map(i=>({name:i.matching_boards[0].fqbn,description:`${i.matching_boards[0].name} on port ${i.port.address}`}))}catch{return[]}}";
const ARDUINO_PORT_POSTPROCESS = "t=>{try{return JSON.parse(t).filter(i=>i.matching_boards).map(i=>({name:i.port.address,description:`${i.matching_boards[0].name} port connection`}))}catch{return[]}}";
const CARGO_WORKSPACE_POSTPROCESS = 'e=>JSON.parse(e).packages.map(a=>({icon:"pkg",name:a.name,description:a.version}))';
const CARGO_DEPENDENCY_POSTPROCESS = 'e=>{let t=JSON.parse(e),i=le(t).flatMap(n=>n.dependencies).map(n=>({name:n.name,description:n.req}));return[...new Map(i.map(n=>[n.name,n])).values()]}';
const CARGO_TARGET_BIN_POSTPROCESS = 'e=>JSON.parse(e).packages.flatMap(p=>p.targets).filter(t=>t.kind.includes("bin")).map(t=>({name:t.name}))';

describe('matchNativeGenerator', () => {
  it('maps git branch to git_branches', () => {
    const result = matchNativeGenerator('git', ['git', 'branch', '--no-color']);
    assert.deepStrictEqual(result, { type: 'git_branches' });
  });

  it('maps git tag to git_tags', () => {
    const result = matchNativeGenerator('git', ['git', 'tag']);
    assert.deepStrictEqual(result, { type: 'git_tags' });
  });

  it('maps git remote to git_remotes', () => {
    const result = matchNativeGenerator('git', ['git', 'remote']);
    assert.deepStrictEqual(result, { type: 'git_remotes' });
  });

  it('returns null for unmapped commands', () => {
    const result = matchNativeGenerator('brew', ['brew', 'outdated']);
    assert.equal(result, null);
  });

  it('returns null for empty/invalid input', () => {
    assert.equal(matchNativeGenerator('test', []), null);
    assert.equal(matchNativeGenerator('test', null), null);
    assert.equal(matchNativeGenerator('test', ['git']), null);
  });

  it('matches only first two elements of script array', () => {
    // git branch with extra flags should still match
    const result = matchNativeGenerator('git', [
      'git', 'branch', '--no-color', '--sort=-committerdate',
    ]);
    assert.deepStrictEqual(result, { type: 'git_branches' });
  });

  it('does not match partial commands', () => {
    // 'git branches' is not 'git branch'
    assert.equal(matchNativeGenerator('git', ['git', 'branches']), null);
  });

  it('maps multipass list to multipass_list', () => {
    const result = matchNativeGenerator('multipass', ['multipass', 'list', '--format', 'json']);
    assert.deepStrictEqual(result, { type: 'multipass_list' });
  });

  it('maps defaults domains to defaults_domains', () => {
    const result = matchNativeGenerator('defaults', ['defaults', 'domains']);
    assert.deepStrictEqual(result, { type: 'defaults_domains' });
  });

  it('maps pandoc --list-input-formats to pandoc_input_formats', () => {
    const result = matchNativeGenerator('pandoc', ['pandoc', '--list-input-formats']);
    assert.deepStrictEqual(result, { type: 'pandoc_input_formats' });
  });

  it('maps pandoc --list-output-formats to pandoc_output_formats', () => {
    const result = matchNativeGenerator('pandoc', ['pandoc', '--list-output-formats']);
    assert.deepStrictEqual(result, { type: 'pandoc_output_formats' });
  });

  it('maps ansible-doc --list to ansible_doc_modules', () => {
    // Short form: just `ansible-doc --list`.
    assert.deepStrictEqual(
      matchNativeGenerator('ansible-doc', ['ansible-doc', '--list']),
      { type: 'ansible_doc_modules' },
    );
    // Long form with --json: key is still built from the first two elements.
    assert.deepStrictEqual(
      matchNativeGenerator('ansible-doc', ['ansible-doc', '--list', '--json']),
      { type: 'ansible_doc_modules' },
    );
  });

  it('maps conda env list to mamba_envs only for mamba spec', () => {
    // In the mamba spec, `conda env list` routes to our mamba_envs provider.
    assert.deepStrictEqual(
      matchNativeGenerator('mamba', ['conda', 'env', 'list']),
      { type: 'mamba_envs' },
    );
    // In the conda spec, the same script stays unmapped (no conda_envs provider yet).
    assert.equal(
      matchNativeGenerator('conda', ['conda', 'env', 'list']),
      null,
    );
  });

  it('maps arduino-cli board list to arduino_cli_boards when postProcess is fqbn-extracting', () => {
    const result = matchNativeGenerator(
      'arduino-cli',
      ['arduino-cli', 'board', 'list', '--format', 'json'],
      ARDUINO_FQBN_POSTPROCESS,
    );
    assert.deepStrictEqual(result, { type: 'arduino_cli_boards' });
  });

  it('maps arduino-cli board list to arduino_cli_ports when postProcess is port-extracting', () => {
    const result = matchNativeGenerator(
      'arduino-cli',
      ['arduino-cli', 'board', 'list', '--format', 'json'],
      ARDUINO_PORT_POSTPROCESS,
    );
    assert.deepStrictEqual(result, { type: 'arduino_cli_ports' });
  });

  it('returns null for arduino-cli board with missing or unrecognized postProcess', () => {
    // No postProcess at all — can't disambiguate, return null rather than guess.
    assert.equal(
      matchNativeGenerator('arduino-cli', ['arduino-cli', 'board', 'list', '--format', 'json']),
      null,
    );
    // Unrelated postProcess (doesn't match either the fqbn or port pattern).
    assert.equal(
      matchNativeGenerator(
        'arduino-cli',
        ['arduino-cli', 'board', 'list', '--format', 'json'],
        't => t.split("\\n")',
      ),
      null,
    );
  });

  it('new third argument does not affect existing git mappings', () => {
    // Regression check: passing an extraneous postProcess source for a non-arduino
    // key should not interfere with the normal lookup.
    const result = matchNativeGenerator('git', ['git', 'branch'], 'some js source');
    assert.deepStrictEqual(result, { type: 'git_branches' });
  });

  it('strips git --no-optional-locks prefix before matching', () => {
    // Upstream fig git spec uses `["git", "--no-optional-locks", "branch", ...]`
    // as the canonical script prefix (avoids touching index.lock). Without
    // stripping this driver-level no-op, every git generator would miss the
    // native map. See docs/phase-minus-1-followups.md §1 for the deferred
    // bug this fix closes.
    assert.deepStrictEqual(
      matchNativeGenerator(
        'git',
        ['git', '--no-optional-locks', 'branch', '-a', '--no-color', '--sort=-committerdate'],
      ),
      { type: 'git_branches' },
    );
    assert.deepStrictEqual(
      matchNativeGenerator('git', ['git', '--no-optional-locks', 'tag', '--list']),
      { type: 'git_tags' },
    );
    assert.deepStrictEqual(
      matchNativeGenerator('git', ['git', '--no-optional-locks', 'remote']),
      { type: 'git_remotes' },
    );
  });

  it('only strips no-op flags for their driver command', () => {
    // `--no-optional-locks` is a git-specific no-op. If another command happens
    // to use a literal argv element of that name, we must NOT skip over it.
    assert.equal(
      matchNativeGenerator('other', ['other', '--no-optional-locks', 'branch']),
      null,
    );
  });

  it('maps cargo metadata to cargo_workspace_members for no-deps package projections', () => {
    assert.deepStrictEqual(
      matchNativeGenerator(
        'cargo',
        ['cargo', 'metadata', '--format-version', '1', '--no-deps'],
        CARGO_WORKSPACE_POSTPROCESS,
      ),
      { type: 'cargo_workspace_members' },
    );
  });

  it('does not map cargo metadata dependency-graph generators to workspace members', () => {
    assert.deepStrictEqual(
      matchNativeGenerator(
        'cargo',
        ['cargo', 'metadata', '--format-version', '1'],
        CARGO_WORKSPACE_POSTPROCESS,
      ),
      null,
    );
    assert.deepStrictEqual(
      matchNativeGenerator(
        'cargo',
        ['cargo', 'metadata', '--format-version', '1'],
        CARGO_DEPENDENCY_POSTPROCESS,
      ),
      null,
    );
    assert.deepStrictEqual(
      matchNativeGenerator(
        'cargo',
        ['cargo', 'metadata', '--format-version', '1', '--no-deps'],
        CARGO_DEPENDENCY_POSTPROCESS,
      ),
      null,
    );
    assert.deepStrictEqual(
      matchNativeGenerator('cargo', ['cargo', 'metadata']),
      null,
    );
  });

  it('maps cargo metadata target projections to cargo_targets with kind params', () => {
    assert.deepStrictEqual(
      matchNativeGenerator(
        'cargo',
        ['cargo', 'metadata', '--format-version', '1', '--no-deps'],
        CARGO_TARGET_BIN_POSTPROCESS,
      ),
      { type: 'cargo_targets', params: { kind: 'bin' } },
    );
  });

  it('maps npm bash-c with package.json post-process to npm_scripts', () => {
    // Real-world post-process snippet copied from the pre-regen npm spec
    // — projects `package.json#scripts` into Fig suggestions.
    const npmPostProcess = 'function(n,[s]){let c=JSON.parse(n),p=c.scripts;return Object.entries(p)}';
    assert.deepStrictEqual(
      matchNativeGenerator(
        'npm',
        ['bash', '-c', "until [[ -f package.json ]] || [[ $PWD = '/' ]]; do cd ..; done; cat package.json"],
        npmPostProcess,
      ),
      { type: 'npm_scripts' },
    );
  });

  it('maps npm package dependency projectors to npm_local providers', () => {
    const depsPostProcess = 'async function(n,s){let{stdout:p}=await s({command:"cat",args:["package.json"]}),t=JSON.parse(p),r=t.dependencies??{};return Object.keys(r).map(d=>({name:d}))}';
    const devDepsPostProcess = 'async function(n,s){let{stdout:p}=await s({command:"cat",args:["package.json"]}),t=JSON.parse(p),r=t.devDependencies??{};return Object.keys(r).map(d=>({name:d}))}';
    const allDepsPostProcess = 'async function(n,s){let{stdout:p}=await s({command:"cat",args:["package.json"]}),t=JSON.parse(p),r=t.dependencies??{},h=t.devDependencies,g=t.optionalDependencies??{};return Object.assign(r,h,g),Object.keys(r).map(d=>({name:d}))}';

    assert.deepStrictEqual(
      matchNativeGenerator('npm', ['npm', 'prefix'], depsPostProcess),
      { type: 'npm_dependencies' },
    );
    assert.deepStrictEqual(
      matchNativeGenerator('npm', ['npm', 'prefix'], devDepsPostProcess),
      { type: 'npm_dev_dependencies' },
    );
    assert.deepStrictEqual(
      matchNativeGenerator('npm', ['npm', 'prefix'], allDepsPostProcess),
      { type: 'npm_all_dependencies' },
    );
  });

  it('maps container and cluster tool scripts to native provider types', () => {
    assert.deepStrictEqual(
      matchNativeGenerator('docker', ['docker', 'images', '--format', '{{json .}}']),
      { type: 'docker_images' },
    );
    assert.deepStrictEqual(
      matchNativeGenerator('podman', ['podman', 'ps', '--format', '{{json .}}']),
      { type: 'docker_containers', params: { binary: 'podman' } },
    );
    assert.deepStrictEqual(
      matchNativeGenerator('docker', ['docker', 'network', 'list', '--format', '{{json .}}']),
      { type: 'docker_networks' },
    );
    assert.deepStrictEqual(
      matchNativeGenerator('podman', ['podman', 'volume', 'list', '--format', '{{json .}}']),
      { type: 'docker_volumes', params: { binary: 'podman' } },
    );
    assert.deepStrictEqual(
      matchNativeGenerator('kubectl', ['kubectl', 'api-resources', '-o', 'name']),
      { type: 'k8s_resources' },
    );
    assert.deepStrictEqual(
      matchNativeGenerator('kubecolor', ['kubectl', 'get', 'pods', '-o', 'json']),
      { type: 'k8s_pods' },
    );
  });

  it('maps small system tools to native provider types', () => {
    assert.deepStrictEqual(
      matchNativeGenerator('tmux', ['tmux', 'list-sessions', '-F', '#{session_name}']),
      { type: 'tmux_sessions' },
    );
    assert.deepStrictEqual(
      matchNativeGenerator('tmux', ['tmux', 'ls', '-F', '#{session_name}']),
      { type: 'tmux_sessions' },
    );
    assert.deepStrictEqual(
      matchNativeGenerator('systemctl', ['systemctl', 'list-units', '--all']),
      { type: 'systemd_units' },
    );
    assert.deepStrictEqual(
      matchNativeGenerator('brew', ['brew', 'list', '--formula']),
      { type: 'brew_formulae_installed' },
    );
    assert.deepStrictEqual(
      matchNativeGenerator('dscl', ['dscl', '.', 'list', '/Users']),
      { type: 'dscl_users' },
    );
  });

  it('does not map npm bash-c when post-process does not look like the package.json scripts shape', () => {
    // No post-process at all, or one that doesn't read package.json#scripts —
    // we MUST NOT route to npm_scripts (the bash invocation might be unrelated).
    assert.equal(
      matchNativeGenerator(
        'npm',
        ['bash', '-c', 'echo hello'],
      ),
      null,
    );
    assert.equal(
      matchNativeGenerator(
        'npm',
        ['bash', '-c', 'curl https://example.com'],
        'function(t){return t.split("\\n")}',
      ),
      null,
    );
  });

  it('does not map bash-c for non-npm specs even when post-process matches', () => {
    const npmShape = 'function(n){let c=JSON.parse(n);return c.scripts}';
    assert.equal(
      matchNativeGenerator('yarn', ['bash', '-c', 'cat package.json'], npmShape),
      null,
    );
  });
});

describe('matchNativeFromJsSource', () => {
  it('maps make `make -qp` JS source to makefile_targets', () => {
    // Upstream make.json's generators are `script: () => "make -qp ..."`,
    // i.e. they have no `script` array — the entire generator is a JS
    // function. The stringified function source is what we match on.
    const makeJsSource = 'async(f,a)=>{let{stdout:m}=await a({command:"bash",args:["-c","make -qp | awk -F\':\' \'/^[a-zA-Z0-9]/{print $1}\'"]})';
    assert.deepStrictEqual(
      matchNativeFromJsSource('make', makeJsSource),
      { type: 'makefile_targets' },
    );
  });

  it('maps kubectl context script functions to k8s_contexts', () => {
    const contextJsSource = 'function(tokens){return ["kubectl","config","get-contexts","-o","name"]}';
    assert.deepStrictEqual(
      matchNativeFromJsSource('kubectl', contextJsSource),
      { type: 'k8s_contexts' },
    );
    assert.deepStrictEqual(
      matchNativeFromJsSource('kubecolor', contextJsSource),
      { type: 'k8s_contexts' },
    );
  });

  it('does not map non-make specs even with the same JS shape', () => {
    const makeJsSource = 'something with make -qp embedded';
    assert.equal(matchNativeFromJsSource('git', makeJsSource), null);
  });

  it('returns null for empty/non-string input', () => {
    assert.equal(matchNativeFromJsSource('make', null), null);
    assert.equal(matchNativeFromJsSource('make', undefined), null);
    assert.equal(matchNativeFromJsSource('make', ''), null);
  });

  it('does not map make sources that do not invoke make -qp', () => {
    // Only the recognized `make -qp` invocation is safe to rewrite —
    // other make generators may have unrelated semantics.
    assert.equal(
      matchNativeFromJsSource('make', 'function(){return ["always"]}'),
      null,
    );
  });
});
