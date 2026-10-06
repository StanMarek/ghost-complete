//! When the terminal's own zsh integration is loaded in the shell (Ghostty,
//! kitty; `init.zsh` re-arms it behind the proxy, #186), the terminal marks
//! prompts itself, and its precmd hook runs last: our `133;A` would reach the
//! terminal before its `133;D` for the previous command. Our hooks must then
//! send the proxy only the private OSC 7771, which never reaches the
//! terminal, and otherwise keep sending exactly what they send today.

use std::process::Command;

const PRIVATE_ONLY: &str = "\x1b]7771;A\x07\x1b]7771;C\x07";

fn zsh_available() -> bool {
    Command::new("zsh")
        .arg("-c")
        .arg("exit 0")
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Sources `ghost-complete.zsh`, runs `setup`, then one prompt and one
/// command through our hooks with `env` as the whole environment. Returns
/// what the hooks printed, or `None` when zsh is missing outside CI.
fn hook_output(setup: &str, env: &[(&str, &str)]) -> Option<String> {
    if !zsh_available() {
        if std::env::var_os("CI").is_some() {
            panic!("zsh not found on CI runner");
        }
        eprintln!("zsh not available; skipping");
        return None;
    }
    let zsh_src = std::fs::read_to_string("../../shell/ghost-complete.zsh")
        .expect("read shell/ghost-complete.zsh");
    let script = format!("{zsh_src}\n{setup}\n_gc_precmd\n_gc_preexec\n");
    let out = Command::new("zsh")
        .arg("-f")
        .arg("-c")
        .arg(&script)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .envs(env.iter().copied())
        .output()
        .expect("run zsh");
    assert!(
        out.status.success(),
        "zsh failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8(out.stdout).unwrap())
}

#[test]
fn ghostty_integration_gets_only_private_marks() {
    for hook in ["_ghostty_precmd", "_ghostty_deferred_init"] {
        let Some(out) = hook_output(
            &format!("{hook}() {{ :; }}"),
            &[("TERM_PROGRAM", "ghostty")],
        ) else {
            return;
        };
        assert_eq!(out, PRIVATE_ONLY, "with {hook} defined");
    }
}

#[test]
fn kitty_integration_gets_only_private_marks() {
    for hook in ["_ksi_precmd", "_ksi_deferred_init"] {
        let Some(out) = hook_output(&format!("{hook}() {{ :; }}"), &[("KITTY_WINDOW_ID", "1")])
        else {
            return;
        };
        assert_eq!(out, PRIVATE_ONLY, "with {hook} defined");
    }
}

/// Unchanged: Ghostty without its integration (`shell-integration = none`)
/// parses our OSC 133 itself.
#[test]
fn ghostty_without_its_integration_keeps_osc133() {
    let Some(out) = hook_output("", &[("TERM_PROGRAM", "ghostty")]) else {
        return;
    };
    assert_eq!(out, "\x1b]133;A\x07\x1b]133;C\x07");
}

/// Unchanged: a terminal without native OSC 133 also gets OSC 7771.
#[test]
fn terminal_without_native_osc133_gets_both() {
    let Some(out) = hook_output("", &[("TERM_PROGRAM", "Apple_Terminal")]) else {
        return;
    };
    assert_eq!(
        out,
        "\x1b]133;A\x07\x1b]7771;A\x07\x1b]133;C\x07\x1b]7771;C\x07"
    );
}
