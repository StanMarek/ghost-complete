//! Real-zsh OSC 7 round-trip test: a directory whose name contains a
//! semicolon must survive end-to-end. vte splits OSC parameters on ';',
//! so the shell-side encoder must percent-encode ';' even though RFC 3986
//! allows it as a sub-delimiter in URI paths.

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
fn osc7_roundtrip_with_semicolon_in_path() {
    if !zsh_available() {
        if std::env::var_os("CI").is_some() {
            panic!("zsh not found on CI runner");
        }
        eprintln!("zsh not available; skipping");
        return;
    }

    let zsh_src = std::fs::read_to_string("../../shell/ghost-complete.zsh")
        .expect("read shell/ghost-complete.zsh");

    // Drive the encoder directly with a path containing ';'.
    let path = "/tmp/foo;bar/baz";
    let script = format!(
        r#"set -e
{src}
printf 'OSC7=%s\n' "$(_gc_urlencode_path {path:?})"
"#,
        src = zsh_src,
        path = path,
    );
    let out = Command::new("zsh")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run zsh");
    assert!(
        out.status.success(),
        "zsh failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let encoded = stdout
        .lines()
        .find_map(|l| l.strip_prefix("OSC7="))
        .expect("encoder output");

    assert!(
        !encoded.contains(';'),
        "encoder must percent-encode ';' (got {encoded:?})",
    );

    // Feed the OSC 7 framing through the parser and check the decoded CWD.
    let osc7 = format!("\x1b]7;file://localhost{encoded}\x07");
    let mut parser = TerminalParser::new(24, 80);
    parser.process_bytes(osc7.as_bytes());
    let cwd = parser.state().cwd().cloned().expect("OSC 7 must set CWD");
    assert_eq!(cwd, std::path::PathBuf::from(path));
}
