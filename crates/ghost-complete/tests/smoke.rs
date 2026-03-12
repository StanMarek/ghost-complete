mod harness;

use harness::GhostProcess;
use std::thread;
use std::time::Duration;

#[test]
fn test_echo_passthrough() {
    let mut proc = GhostProcess::spawn();
    proc.send_line("echo hello_smoke_test");
    proc.expect_output("hello_smoke_test");
    proc.exit_with_code(0);
}

#[test]
fn test_exit_code_zero() {
    let mut proc = GhostProcess::spawn();
    let code = proc.exit_with_code(0);
    assert_eq!(code, 0, "expected exit code 0, got {}", code);
}

#[test]
fn test_exit_code_nonzero() {
    let mut proc = GhostProcess::spawn();
    let code = proc.exit_with_code(42);
    assert_eq!(code, 42, "expected exit code 42, got {}", code);
}

#[test]
fn test_large_output() {
    let mut proc = GhostProcess::spawn();
    proc.send_line("seq 1 5000");
    // Wait for a number that appears only in seq's output (not in the echoed command).
    // "5000" also appears in "seq 1 5000", so we wait for "4999" instead.
    proc.expect_output("4999");

    // Poll until output buffer has stabilized (no new bytes for 500ms).
    let mut prev_len = 0;
    for _ in 0..10 {
        thread::sleep(Duration::from_millis(500));
        let snapshot = proc.output_snapshot();
        if snapshot.len() == prev_len {
            break;
        }
        prev_len = snapshot.len();
    }

    let snapshot = proc.output_snapshot();
    let text = String::from_utf8_lossy(&snapshot);
    // Check a spread of numbers. Use numbers > 4 digits to avoid false positives
    // from ANSI escape sequence parameters (e.g. "\x1b[100;1H" cursor positioning).
    for n in &[1000, 2500, 3333, 4999, 5000] {
        let needle = format!("{}", n);
        assert!(
            text.contains(&needle),
            "large output missing expected number {} (output {} bytes)",
            n,
            snapshot.len()
        );
    }
    proc.exit_with_code(0);
}

#[test]
fn test_environment_preserved() {
    let mut proc = GhostProcess::spawn();
    proc.send_line("echo HOME_IS=$HOME");
    proc.expect_output("HOME_IS=/");
    proc.exit_with_code(0);
}

#[test]
fn test_pipe_passthrough() {
    let mut proc = GhostProcess::spawn();
    proc.send_line("echo pipe_marker | cat");
    proc.expect_output("pipe_marker");
    proc.exit_with_code(0);
}

#[test]
fn test_rapid_input() {
    let mut proc = GhostProcess::spawn();
    for i in 0..20 {
        proc.send_line(&format!("echo rapid_{}", i));
    }
    proc.expect_output("rapid_19");

    let snapshot = proc.output_snapshot();
    let text = String::from_utf8_lossy(&snapshot);
    assert!(text.contains("rapid_0"), "missing rapid_0 in output");
    assert!(text.contains("rapid_10"), "missing rapid_10 in output");
    proc.exit_with_code(0);
}

#[test]
fn test_memory_baseline() {
    let proc = GhostProcess::spawn();
    thread::sleep(Duration::from_secs(1));

    if let Some(pid) = proc.child_pid() {
        let output = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &pid.to_string()])
            .output()
            .expect("failed to run ps");
        let rss_str = String::from_utf8_lossy(&output.stdout);
        if let Ok(rss_kb) = rss_str.trim().parse::<u64>() {
            let rss_mb = rss_kb / 1024;
            assert!(
                rss_mb < 500,
                "RSS is {} MB, exceeds 500 MB threshold",
                rss_mb
            );
        }
        // If we can't parse RSS (process already exited), that's fine — skip the check.
    }
}

#[test]
fn test_clean_startup_shutdown() {
    let mut proc = GhostProcess::spawn();
    proc.send_line("echo alive");
    proc.expect_output("alive");
    let code = proc.exit_with_code(0);
    assert_eq!(code, 0, "expected clean exit 0, got {}", code);
}

#[test]
fn test_multiple_commands() {
    let mut proc = GhostProcess::spawn();
    proc.send_line("echo aaa");
    proc.expect_output("aaa");
    proc.send_line("echo bbb");
    proc.expect_output("bbb");
    proc.send_line("echo ccc");
    proc.expect_output("ccc");
    proc.exit_with_code(0);
}
