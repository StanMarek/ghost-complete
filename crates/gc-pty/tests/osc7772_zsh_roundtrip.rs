//! End-to-end OSC 7772 round-trip: real zsh emitter → real Rust parser.
//!
//! Spawns an actual `zsh -c` for each fixture, sources the production
//! `shell/ghost-complete.zsh`, calls `_gc_report_buffer` with the fixture
//! bytes set as `$BUFFER`, captures stdout, and feeds the bytes through
//! `gc_parser::TerminalParser`. The reconstructed buffer must equal the
//! input. Every fixture asserts CWD did not change before/after the
//! round-trip; this invariant is meaningful only for the OSC-injection
//! fixture (others have no embedded OSC 7), but all fixtures share the
//! same assertion path — proving the decoded bytes never re-entered
//! the VTE state machine. See ADR 0003.
//!
//! Silently skipped on local dev systems without `zsh` on PATH; panics
//! under CI (`CI` env var set) to fail loud if zsh is missing in a
//! controlled environment.

use std::path::PathBuf;
use std::process::Command;

use gc_parser::TerminalParser;

/// Encode arbitrary bytes into a zsh `$'…'` literal. Every byte goes
/// through as `\xXX` — verbose but unambiguous, no quoting edge cases.
fn to_zsh_literal(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 4 + 3);
    out.push_str("$'");
    for &b in bytes {
        out.push_str(&format!("\\x{b:02X}"));
    }
    out.push('\'');
    out
}

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .expect("gc-pty crate should be two dirs deep")
        .to_path_buf()
}

fn zsh_available() -> bool {
    Command::new("zsh")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run the production emitter for `(buffer, cursor)` and return raw stdout.
fn emit_via_real_zsh(buffer: &[u8], cursor: usize) -> Vec<u8> {
    let zsh_init = repo_root().join("shell/ghost-complete.zsh");
    assert!(zsh_init.exists(), "shell/ghost-complete.zsh not found");

    // Single-line zsh script: source the integration, set BUFFER and
    // CURSOR, invoke the reporter. `$'…'` escapes through every byte
    // literally — no quoting hazards.
    // GHOST_COMPLETE_ACTIVE=1 must be set so _gc_report_buffer does not
    // early-return (the gate added in Task 7 guards against leaking
    // OSC 7772 to terminals when the proxy is absent).
    let script = format!(
        "GHOST_COMPLETE_ACTIVE=1; source {init_q}; BUFFER={buf_lit}; CURSOR={cursor}; _gc_report_buffer",
        init_q = shell_quote(zsh_init.to_str().expect("path is utf-8")),
        buf_lit = to_zsh_literal(buffer),
    );

    let output = Command::new("zsh")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("zsh -c failed to launch");
    assert!(
        output.status.success(),
        "zsh exited non-zero: stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// Run the production zsh env reporter after applying the supplied shell body.
fn emit_env_via_real_zsh(setup: &str) -> Vec<u8> {
    let zsh_init = repo_root().join("shell/ghost-complete.zsh");
    assert!(zsh_init.exists(), "shell/ghost-complete.zsh not found");

    let script = format!(
        "source {init_q}; {setup}; _gc_report_env",
        init_q = shell_quote(zsh_init.to_str().expect("path is utf-8")),
    );

    let output = Command::new("zsh")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("zsh -c failed to launch");
    assert!(
        output.status.success(),
        "zsh exited non-zero: stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// Run an arbitrary zsh script body inside a single subshell that has
/// already sourced `shell/ghost-complete.zsh`. The script body runs
/// AFTER the source completes — so `_gc_install_zle_hook` has already
/// been unset and any state established by sourcing is observable.
fn run_zsh_after_source(body: &str) -> Vec<u8> {
    let zsh_init = repo_root().join("shell/ghost-complete.zsh");
    assert!(zsh_init.exists(), "shell/ghost-complete.zsh not found");

    let script = format!(
        "source {init_q}; {body}",
        init_q = shell_quote(zsh_init.to_str().expect("path is utf-8")),
    );

    let output = Command::new("zsh")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("zsh -c failed to launch");
    assert!(
        output.status.success(),
        "zsh exited non-zero: stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// Run a zsh script body BEFORE sourcing `shell/ghost-complete.zsh`,
/// so the script can stage preconditions (e.g. pre-register a non-user
/// `zle-line-pre-redraw` widget) that the integration observes at source
/// time. The `before` body runs first, then the integration is sourced.
fn run_zsh_before_source(before: &str) -> Vec<u8> {
    let zsh_init = repo_root().join("shell/ghost-complete.zsh");
    assert!(zsh_init.exists(), "shell/ghost-complete.zsh not found");

    let script = format!(
        "{before}; source {init_q}",
        init_q = shell_quote(zsh_init.to_str().expect("path is utf-8")),
    );

    let output = Command::new("zsh")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("zsh -c failed to launch");
    assert!(
        output.status.success(),
        "zsh exited non-zero: stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// Count non-overlapping occurrences of `needle` in `haystack`.
fn count_subslices(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut i = 0;
    while i + needle.len() <= haystack.len() {
        if &haystack[i..i + needle.len()] == needle {
            count += 1;
            i += needle.len();
        } else {
            i += 1;
        }
    }
    count
}

/// POSIX-quote a string for safe interpolation as one zsh argument.
fn shell_quote(s: &str) -> String {
    // Single-quote everything; close-quote, escape any `'` as `'\''`,
    // reopen. Defensive even for paths we control — tempdirs are fine
    // but the repo path could in principle contain a quote.
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str(r"'\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn assert_roundtrips(label: &str, fixture: &[u8]) {
    let cursor = std::str::from_utf8(fixture)
        .expect("fixture is valid UTF-8")
        .chars()
        .count();
    let stdout = emit_via_real_zsh(fixture, cursor);

    let mut p = TerminalParser::new(24, 80);
    let cwd_before = p.state().cwd().cloned();
    p.process_bytes(&stdout);

    let actual = p.state().command_buffer();
    let expected = std::str::from_utf8(fixture).unwrap();
    assert_eq!(
        actual,
        Some(expected),
        "[{label}] reconstruction failed; raw zsh stdout = {stdout:02X?}"
    );
    assert_eq!(
        p.state().buffer_cursor(),
        cursor,
        "[{label}] buffer cursor not preserved through real-zsh round-trip"
    );
    assert_eq!(
        p.state().cwd().cloned(),
        cwd_before,
        "[{label}] OSC 7772 dispatch must not change cwd"
    );
}

#[test]
fn osc7772_real_zsh_roundtrip() {
    if !zsh_available() {
        if std::env::var_os("CI").is_some() {
            panic!(
                "zsh not on PATH but running under CI — install zsh or mark this test #[ignore] explicitly"
            );
        }
        eprintln!("skipping osc7772_real_zsh_roundtrip: zsh not on PATH (local dev)");
        return;
    }

    // Each fixture exercises a different failure mode of the legacy
    // raw 7770 framing: ';' splitting, BEL early-terminate, ESC[…m
    // colour codes, multi-byte UTF-8, and a deliberate OSC 7 smuggle.
    assert_roundtrips("plain semicolon", b"echo a; ls -la");
    assert_roundtrips("compound", b"if true; then echo a; fi");
    assert_roundtrips("bel inside", b"x\x07y");
    assert_roundtrips("ansi colour", b"\x1b[31mred\x1b[0m");
    assert_roundtrips("cjk + semicolon", "日本語; cmd".as_bytes());

    // Smuggle attempt: the buffer LOOKS like OSC 7 (CWD update). After
    // round-trip the `cwd` MUST remain unchanged — the decoded bytes go
    // straight to `set_command_buffer`, not back through the VTE parser.
    assert_roundtrips("osc7 smuggle attempt", b"\x1b]7;file:///etc/passwd\x07");

    // Additional ADR-threat-model fixtures verified against the real zsh
    // emitter: NUL byte, ESC+ST envelope terminator, already-percent-
    // encoded text (round-trip-once invariant), and the empty buffer.
    assert_roundtrips("embedded NUL", b"a\x00b");
    assert_roundtrips("embedded ESC+ST", b"foo\x1b\\bar");
    assert_roundtrips("all-encoded percent", b"abc%20def");
    assert_roundtrips("empty buffer", b"");
}

#[test]
fn osc7773_real_zsh_env_roundtrip() {
    if !zsh_available() {
        if std::env::var_os("CI").is_some() {
            panic!(
                "zsh not on PATH but running under CI — install zsh or mark this test #[ignore] explicitly"
            );
        }
        eprintln!("skipping osc7773_real_zsh_env_roundtrip: zsh not on PATH (local dev)");
        return;
    }

    let stdout = emit_env_via_real_zsh(
        "export GHOST_COMPLETE_ACTIVE=1; export AWS_PROFILE=dev; export EMPTY=; export FOO=$'a;b\\nc'; unset UNEXPORTED",
    );

    let mut p = TerminalParser::new(24, 80);
    p.process_bytes(&stdout);
    let env = p.state().shell_env().expect("OSC 7773 env report expected");

    assert_eq!(env.get("AWS_PROFILE").map(String::as_str), Some("dev"));
    assert_eq!(env.get("EMPTY").map(String::as_str), Some(""));
    assert_eq!(env.get("FOO").map(String::as_str), Some("a;b\nc"));
    assert_eq!(env.get("UNEXPORTED"), None);
}

#[test]
fn osc7773_per_value_cap_drops_oversized_value_keeps_others() {
    if !zsh_available() {
        if std::env::var_os("CI").is_some() {
            panic!(
                "zsh not on PATH but running under CI — install zsh or mark this test #[ignore] explicitly"
            );
        }
        eprintln!(
            "skipping osc7773_per_value_cap_drops_oversized_value_keeps_others: zsh not on PATH (local dev)"
        );
        return;
    }

    // 20000 bytes > _GC_ENV_PER_VALUE_CAP (16384). Build the oversized
    // value entirely in zsh to avoid bloating the Rust-side command line.
    let stdout = emit_env_via_real_zsh(
        "export GHOST_COMPLETE_ACTIVE=1; export FOO=${(l:20000::x:)}; export BAR=baz",
    );

    let mut p = TerminalParser::new(24, 80);
    p.process_bytes(&stdout);
    let env = p.state().shell_env().expect("OSC 7773 env report expected");

    assert_eq!(env.get("BAR").map(String::as_str), Some("baz"));
    assert!(
        env.get("FOO").is_none(),
        "FOO ({} bytes) should have been dropped by per-value cap",
        env.get("FOO").map(String::len).unwrap_or(0)
    );
}

#[test]
fn osc7773_total_budget_drops_late_entries_keeps_essentials() {
    if !zsh_available() {
        if std::env::var_os("CI").is_some() {
            panic!(
                "zsh not on PATH but running under CI — install zsh or mark this test #[ignore] explicitly"
            );
        }
        eprintln!(
            "skipping osc7773_total_budget_drops_late_entries_keeps_essentials: zsh not on PATH (local dev)"
        );
        return;
    }

    // 200 vars × ~4 KiB each ≫ _GC_ENV_TOTAL_BUDGET (512 KiB) — guaranteed
    // total-budget exhaustion. Set PATH and HOME explicitly so essentials
    // emission has known values that must survive.
    let stdout = emit_env_via_real_zsh(
        "export GHOST_COMPLETE_ACTIVE=1; \
         export PATH=/usr/bin:/bin; \
         export HOME=/tmp/gc-test-home; \
         local i; for i in {1..200}; do export GC_BUDGET_VAR_${i}=${(l:4096::y:)}; done",
    );

    let mut p = TerminalParser::new(24, 80);
    p.process_bytes(&stdout);
    let env = p.state().shell_env().expect("OSC 7773 env report expected");

    assert_eq!(
        env.get("PATH").map(String::as_str),
        Some("/usr/bin:/bin"),
        "essentials (PATH) must survive total-budget exhaustion"
    );
    assert_eq!(
        env.get("HOME").map(String::as_str),
        Some("/tmp/gc-test-home"),
        "essentials (HOME) must survive total-budget exhaustion"
    );

    let kept: usize = (1..=200)
        .filter(|i| env.contains_key(&format!("GC_BUDGET_VAR_{i}")))
        .count();
    assert!(
        kept < 200,
        "expected total-budget pressure to drop at least one GC_BUDGET_VAR_* (kept {kept}/200)"
    );
}

#[test]
fn osc7773_auth_prefix_vars_survive_budget_pressure() {
    if !zsh_available() {
        if std::env::var_os("CI").is_some() {
            panic!(
                "zsh not on PATH but running under CI — install zsh or mark this test #[ignore] explicitly"
            );
        }
        eprintln!(
            "skipping osc7773_auth_prefix_vars_survive_budget_pressure: zsh not on PATH (local dev)"
        );
        return;
    }

    // 200 filler vars alphabetized BEFORE `AWS_` (`AAA_*` < `AWS_*`),
    // each ~4 KiB, exhaust the catch-all `(ok)parameters` sweep long
    // before it reaches the AWS_/GITHUB_ entries. Without the
    // auth_prefixes priority loop in _gc_report_env, AWS_PROFILE and
    // GITHUB_TOKEN would be dropped silently. Pin the priority intent.
    let stdout = emit_env_via_real_zsh(
        "export GHOST_COMPLETE_ACTIVE=1; \
         export AWS_PROFILE=dev; \
         export GITHUB_TOKEN=tok; \
         local i; for i in {1..200}; do export AAA_FILLER_${i}=${(l:4096::x:)}; done",
    );

    let mut p = TerminalParser::new(24, 80);
    p.process_bytes(&stdout);
    let env = p.state().shell_env().expect("OSC 7773 env report expected");

    assert_eq!(
        env.get("AWS_PROFILE").map(String::as_str),
        Some("dev"),
        "auth-prefixed AWS_PROFILE must survive budget pressure via the auth_prefixes priority loop"
    );
    assert_eq!(
        env.get("GITHUB_TOKEN").map(String::as_str),
        Some("tok"),
        "auth-prefixed GITHUB_TOKEN must survive budget pressure via the auth_prefixes priority loop"
    );

    // Sanity: the filler set must actually have pushed the catch-all
    // past the budget — otherwise the assertion above is vacuous.
    let kept_fillers: usize = (1..=200)
        .filter(|i| env.contains_key(&format!("AAA_FILLER_{i}")))
        .count();
    assert!(
        kept_fillers < 200,
        "expected catch-all sweep to exhaust budget on AAA_FILLER_* (kept {kept_fillers}/200); \
         without budget pressure this test does not exercise the priority loop"
    );
}

#[test]
fn osc7773_emits_env_truncated_diagnostic_when_dropping() {
    if !zsh_available() {
        if std::env::var_os("CI").is_some() {
            panic!(
                "zsh not on PATH but running under CI — install zsh or mark this test #[ignore] explicitly"
            );
        }
        eprintln!(
            "skipping osc7773_emits_env_truncated_diagnostic_when_dropping: zsh not on PATH (local dev)"
        );
        return;
    }

    let stdout = emit_env_via_real_zsh(
        "export GHOST_COMPLETE_ACTIVE=1; export FOO=${(l:20000::x:)}; export BAR=baz",
    );

    let needle = b"\x1b]7774;env_truncated;";
    assert!(
        count_subslices(&stdout, needle) >= 1,
        "expected at least one OSC 7774 env_truncated frame, got stdout = {stdout:02X?}"
    );
}

#[test]
fn osc7773_excludes_gc_internal_vars() {
    if !zsh_available() {
        if std::env::var_os("CI").is_some() {
            panic!(
                "zsh not on PATH but running under CI — install zsh or mark this test #[ignore] explicitly"
            );
        }
        eprintln!("skipping osc7773_excludes_gc_internal_vars: zsh not on PATH (local dev)");
        return;
    }

    let stdout = emit_env_via_real_zsh(
        "export GHOST_COMPLETE_ACTIVE=1; \
         export GHOST_COMPLETE_PANE=42; \
         typeset -gx _GC_LAST_ENV_PAYLOAD=anything; \
         export REGULAR_VAR=ok",
    );

    let mut p = TerminalParser::new(24, 80);
    p.process_bytes(&stdout);
    let env = p.state().shell_env().expect("OSC 7773 env report expected");

    assert_eq!(env.get("REGULAR_VAR").map(String::as_str), Some("ok"));
    assert!(
        env.get("GHOST_COMPLETE_ACTIVE").is_none(),
        "GHOST_COMPLETE_ACTIVE must not leak into the env snapshot"
    );
    assert!(
        env.get("GHOST_COMPLETE_PANE").is_none(),
        "GHOST_COMPLETE_PANE must not leak into the env snapshot"
    );
    assert!(
        env.get("_GC_LAST_ENV_PAYLOAD").is_none(),
        "_GC_LAST_ENV_PAYLOAD must not leak into the env snapshot"
    );
}

#[test]
fn osc7774_env_truncated_emits_at_most_once_per_session() {
    if !zsh_available() {
        if std::env::var_os("CI").is_some() {
            panic!(
                "zsh not on PATH but running under CI — install zsh or mark this test #[ignore] explicitly"
            );
        }
        eprintln!(
            "skipping osc7774_env_truncated_emits_at_most_once_per_session: zsh not on PATH (local dev)"
        );
        return;
    }

    // Two report cycles in the same zsh session, both with conditions
    // that force `truncated=1`. The second call mutates BAR so the
    // payload differs from the cached `_GC_LAST_ENV_PAYLOAD` (otherwise
    // the dedup guard short-circuits before the 7774 emission branch is
    // even reached). The one-shot latch must still suppress the second
    // diagnostic.
    let stdout = run_zsh_after_source(
        "export GHOST_COMPLETE_ACTIVE=1; \
         export FOO=${(l:20000::x:)}; \
         export BAR=first; \
         _gc_report_env; \
         export BAR=second; \
         _gc_report_env",
    );

    let needle = b"\x1b]7774;env_truncated;";
    let count = count_subslices(&stdout, needle);
    assert_eq!(
        count, 1,
        "expected exactly one OSC 7774 env_truncated diagnostic per session, got {count}: stdout = {stdout:02X?}"
    );
}

#[test]
fn osc7774_zle_hook_disabled_emits_for_non_user_widget_when_active() {
    if !zsh_available() {
        if std::env::var_os("CI").is_some() {
            panic!(
                "zsh not on PATH but running under CI — install zsh or mark this test #[ignore] explicitly"
            );
        }
        eprintln!(
            "skipping osc7774_zle_hook_disabled_emits_for_non_user_widget_when_active: zsh not on PATH (local dev)"
        );
        return;
    }

    // Pre-register a non-user widget at the zle-line-pre-redraw slot
    // (`zle -C` produces a `completion:…:…` descriptor) BEFORE sourcing
    // the integration. With GHOST_COMPLETE_ACTIVE set, the installer's
    // else-branch must emit `\e]7774;zle_hook_disabled;<encoded>\a`.
    let stdout = run_zsh_before_source(
        "export GHOST_COMPLETE_ACTIVE=1; \
         zmodload zsh/zle; \
         zmodload zsh/complete; \
         zle -C zle-line-pre-redraw complete-word _bash_complete-word",
    );

    let prefix = b"\x1b]7774;zle_hook_disabled;";
    assert!(
        count_subslices(&stdout, prefix) == 1,
        "expected exactly one OSC 7774 zle_hook_disabled frame, stdout = {stdout:02X?}"
    );
    // Verify percent-encoding of the `:` byte made it into the payload.
    assert!(
        count_subslices(&stdout, b"completion%3A") >= 1,
        "zle_hook_disabled detail should percent-encode `:` as `%3A`, stdout = {stdout:02X?}"
    );
    // Frame must be BEL-terminated, matching every other 777x emission.
    let frame_start = stdout
        .windows(prefix.len())
        .position(|w| w == prefix)
        .expect("prefix located above");
    assert!(
        stdout[frame_start..].contains(&0x07),
        "zle_hook_disabled frame must terminate with BEL (\\x07)"
    );
}

#[test]
fn osc7774_zle_hook_disabled_silent_when_inactive() {
    if !zsh_available() {
        if std::env::var_os("CI").is_some() {
            panic!(
                "zsh not on PATH but running under CI — install zsh or mark this test #[ignore] explicitly"
            );
        }
        eprintln!(
            "skipping osc7774_zle_hook_disabled_silent_when_inactive: zsh not on PATH (local dev)"
        );
        return;
    }

    // Same setup as the active case but WITHOUT GHOST_COMPLETE_ACTIVE.
    // The installer must still no-op (no clobber, no chain) AND must not
    // leak a 7774 frame to the terminal.
    let stdout = run_zsh_before_source(
        "unset GHOST_COMPLETE_ACTIVE; \
         zmodload zsh/zle; \
         zmodload zsh/complete; \
         zle -C zle-line-pre-redraw complete-word _bash_complete-word",
    );

    assert_eq!(
        count_subslices(&stdout, b"\x1b]7774;"),
        0,
        "no 7774 frame should be emitted when GHOST_COMPLETE_ACTIVE is unset; stdout = {stdout:02X?}"
    );
}

#[test]
fn osc7773_silent_when_inactive() {
    if !zsh_available() {
        if std::env::var_os("CI").is_some() {
            panic!(
                "zsh not on PATH but running under CI — install zsh or mark this test #[ignore] explicitly"
            );
        }
        eprintln!("skipping osc7773_silent_when_inactive: zsh not on PATH (local dev)");
        return;
    }

    // OSC 7773 carries an env snapshot (PATH, AWS_PROFILE, GITHUB_TOKEN, …).
    // With GHOST_COMPLETE_ACTIVE unset the proxy is not watching, and the
    // raw OSC frame would otherwise render into the terminal's scrollback.
    // Static `report_env_is_gated_on_active` pins the source-level gate;
    // this runtime negative test guards against the gate being present but
    // wired incorrectly (e.g. `[[ -z … ]] || return`).
    //
    // The gate uses `|| return`, so `_gc_report_env` returns 1 when inactive.
    // `run_zsh_after_source` asserts the script exits zero, so we discard
    // the reporter's exit status — we care about its stdout, not its rc.
    let stdout =
        run_zsh_after_source("unset GHOST_COMPLETE_ACTIVE; export FOO=x; _gc_report_env || true");

    assert_eq!(
        count_subslices(&stdout, b"\x1b]7773;"),
        0,
        "no 7773 frame should be emitted when GHOST_COMPLETE_ACTIVE is unset; stdout = {stdout:02X?}"
    );
}
