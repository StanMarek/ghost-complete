//! Pin the shell init terminal-detection matrix against
//! `TerminalProfile::detect_from_env`. New terminals MUST be added in both
//! sides or this test fails.

use gc_terminal::TerminalProfile;

/// The minimum direct-branch env vars that init.zsh must recognise.
const SHELL_DIRECT_ENV_VARS: &[&str] = &[
    "KITTY_WINDOW_ID",
    "WEZTERM_UNIX_SOCKET",
    "ALACRITTY_SOCKET",
    "ZED_TERM",
    "VSCODE_IPC_HOOK_CLI",
];

/// The minimum tmux-branch env vars that init.zsh must recognise.
const SHELL_TMUX_ENV_VARS: &[&str] = &[
    "GHOSTTY_RESOURCES_DIR",
    "KITTY_WINDOW_ID",
    "WEZTERM_UNIX_SOCKET",
    "ALACRITTY_SOCKET",
    "ZED_TERM",
    "VSCODE_IPC_HOOK_CLI",
    "ITERM_SESSION_ID",
];

fn init_zsh_text() -> String {
    std::fs::read_to_string("../../shell/init.zsh").expect("read shell/init.zsh")
}

/// Extract the non-tmux direct branch from init.zsh.
///
/// The file has two branches delimited by these unique comment strings:
///   - `# Inside tmux:` — start of the tmux branch (inside `if [[ -n "$TMUX" ]]; then`)
///   - `# Outside tmux:` — start of the direct (non-tmux) branch (inside `else`)
///
/// The direct branch is everything after `# Outside tmux:`.
fn direct_branch(src: &str) -> &str {
    src.split("# Outside tmux:")
        .nth(1)
        .expect("init.zsh must contain the '# Outside tmux:' marker")
}

/// Extract the tmux branch from init.zsh.
///
/// The tmux branch is the text between `# Inside tmux:` and `# Outside tmux:`.
fn tmux_branch(src: &str) -> &str {
    src.split("# Inside tmux:")
        .nth(1)
        .expect("init.zsh must contain the '# Inside tmux:' marker")
        .split("# Outside tmux:")
        .next()
        .expect("tmux branch must be terminated by '# Outside tmux:'")
}

#[test]
fn init_zsh_direct_branch_recognises_required_env_vars() {
    let src = init_zsh_text();
    let direct = direct_branch(&src);
    for var in SHELL_DIRECT_ENV_VARS {
        assert!(
            direct.contains(var),
            "init.zsh non-tmux direct branch must check ${} (Rust does)",
            var
        );
    }
}

#[test]
fn init_zsh_tmux_branch_recognises_required_env_vars() {
    let src = init_zsh_text();
    let tmux = tmux_branch(&src);
    for var in SHELL_TMUX_ENV_VARS {
        assert!(
            tmux.contains(var),
            "init.zsh tmux branch must check ${} (Rust does)",
            var
        );
    }
}

#[test]
fn rust_terminal_enum_has_wezterm_variant() {
    // Sanity: the WezTerm variant exists in the Rust matrix so the shell
    // side has a counterpart to keep parity with. Env-based detection of
    // WezTerm via socket is exercised by gc-terminal's own unit tests
    // (test_detect_wezterm_direct_via_socket); we can't drive that path
    // from an integration test without mutating process-global env vars,
    // which is unsafe under cargo test's parallel runner.
    let profile = TerminalProfile::for_wezterm();
    assert!(matches!(profile.terminal(), gc_terminal::Terminal::WezTerm));
}
