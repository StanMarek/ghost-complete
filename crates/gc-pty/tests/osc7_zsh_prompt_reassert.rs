//! Real-zsh test: every prompt must re-report the interactive shell's cwd.
//!
//! zsh runs `chpwd` hooks inside subshells, so `( cd /tmp; make )` reports
//! `/tmp` even though the interactive shell never left. The proxy follows
//! OSC 7 with a real `chdir` (#172), so without a per-prompt report it would
//! stay in `/tmp` — wrong directory for new Zellij/tmux panes, and a busy
//! mount that cannot be ejected.

use gc_parser::TerminalParser;
use std::process::Command;

fn zsh_available() -> bool {
    Command::new("zsh")
        .arg("-c")
        .arg("exit 0")
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[test]
fn osc7_prompt_reasserts_cwd_after_subshell_cd() {
    if !zsh_available() {
        if std::env::var_os("CI").is_some() {
            panic!("zsh not found on CI runner");
        }
        eprintln!("zsh not available; skipping");
        return;
    }

    let zsh_src = std::fs::read_to_string("../../shell/ghost-complete.zsh")
        .expect("read shell/ghost-complete.zsh");

    // Two simulated prompts around a subshell `cd`: the first prompt is the
    // one the old self-removing precmd hook still handled.
    let script = format!(
        r#"{zsh_src}
cd /
for f in $precmd_functions; do $f; done
( cd /tmp )
for f in $precmd_functions; do $f; done
"#
    );
    let out = Command::new("zsh")
        .arg("-f")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run zsh");
    assert!(
        out.status.success(),
        "zsh failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let mut parser = TerminalParser::new(24, 80);
    parser.process_bytes(&out.stdout);
    assert_eq!(
        parser.state().cwd(),
        Some(&std::path::PathBuf::from("/")),
        "the prompt after a subshell `cd` must re-report the shell's own cwd"
    );
}
