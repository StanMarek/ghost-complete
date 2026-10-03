//! Startup must fail open (#186): when the proxy cannot start, the user still
//! gets their shell, exec'd in place of the proxy. `init.zsh` has already
//! `exec`'d the original shell away by the time the proxy runs, so exiting
//! here would leave the tab with no shell at all.
//!
//! The "shell" in these tests is `/bin/sh -c REPORT`, which prints its pid
//! and the two environment variables the fallback is responsible for.

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// The fallback execs, so the shell keeps the proxy's pid. `aws` is the
/// IMDS opt-out the proxy sets for its own AWS SDK calls; the user's shell
/// must not inherit it.
const REPORT: &str = r#"printf 'pid=%s marker=%s active=%s aws=%s\n' "$$" "${GHOST_COMPLETE_FALLBACK_PID-unset}" "${GHOST_COMPLETE_ACTIVE-unset}" "${AWS_EC2_METADATA_DISABLED-unset}"; exit 7"#;

/// Exit code of `REPORT`, so a pass-through is distinguishable from any exit
/// code the proxy itself produces.
const REPORT_EXIT: i32 = 7;

const DEADLINE: Duration = Duration::from_secs(20);

/// A config `GhostConfig::load` rejects.
const MALFORMED_CONFIG: &str = "this is = = not toml\n";

/// Run `ghost-complete <cli_args> -- /bin/sh -c REPORT` with a scrubbed
/// environment: `HOME` points at `home`, nothing identifies a terminal except
/// `term_program`, and `GHOST_COMPLETE_ACTIVE=1` is set the way `init.zsh`
/// exports it before exec'ing the proxy. stdin is not a terminal.
fn run(home: &Path, term_program: &str, cli_args: &[&str]) -> (u32, Output) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ghost-complete"));
    cmd.args(cli_args)
        .args(["--", "/bin/sh", "-c", REPORT])
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env("TERM", "xterm-256color")
        .env("TERM_PROGRAM", term_program)
        .env("GHOST_COMPLETE_ACTIVE", "1")
        .current_dir(home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    run_to_completion(cmd)
}

fn run_to_completion(mut cmd: Command) -> (u32, Output) {
    let mut child = cmd.spawn().expect("spawn ghost-complete");
    let pid = child.id();
    let start = Instant::now();
    while child.try_wait().expect("try_wait").is_none() {
        if start.elapsed() > DEADLINE {
            let _ = child.kill();
            panic!("ghost-complete did not exit within {DEADLINE:?}");
        }
        thread::sleep(Duration::from_millis(20));
    }
    (pid, child.wait_with_output().expect("collect output"))
}

fn write_config(home: &Path, contents: &str) -> String {
    let path = home.join("config.toml");
    std::fs::write(&path, contents).expect("write config");
    path.to_str().expect("utf-8 temp path").to_owned()
}

/// The shell ran in the proxy's place, marked as a fallback and without the
/// proxy's recursion guard.
fn assert_fell_back(pid: u32, output: &Output) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(REPORT_EXIT),
        "the shell's exit code must pass through; stdout: {stdout:?} stderr: {stderr:?}"
    );
    assert_eq!(
        stdout,
        format!("pid={pid} marker={pid} active=unset aws=unset\n")
    );
}

#[test]
fn malformed_config_falls_back_to_plain_shell() {
    let home = tempfile::tempdir().unwrap();
    let config = write_config(home.path(), MALFORMED_CONFIG);

    let (pid, output) = run(
        home.path(),
        "ghostty",
        &["--config", &config, "--log-file", "/dev/null"],
    );

    assert_fell_back(pid, &output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&config),
        "the user must be told which config broke startup; stderr: {stderr:?}"
    );
}

#[test]
fn invalid_keybinding_falls_back_to_plain_shell() {
    let home = tempfile::tempdir().unwrap();
    let config = write_config(home.path(), "[keybindings]\naccept = \"ctrl+q\"\n");

    let (pid, output) = run(
        home.path(),
        "ghostty",
        &["--config", &config, "--log-file", "/dev/null"],
    );

    assert_fell_back(pid, &output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ctrl+q"),
        "the user must be told which keybinding broke startup; stderr: {stderr:?}"
    );
}

#[test]
fn unknown_terminal_falls_back_to_plain_shell() {
    let home = tempfile::tempdir().unwrap();

    let (pid, output) = run(
        home.path(),
        "not-a-real-terminal",
        &["--log-file", "/dev/null"],
    );

    assert_fell_back(pid, &output);
}

/// Start the proxy, with a malformed config, from `/bin/sh -c launcher`.
/// The launcher marks itself as a fallback shell (`$$`), then runs
/// `"$0" --config "$1" --log-file /dev/null -- /bin/sh -c "$2"`, the last
/// argument being `REPORT`.
fn run_from_marked_shell(launcher: &str) -> Output {
    let home = tempfile::tempdir().unwrap();
    let config = write_config(home.path(), MALFORMED_CONFIG);

    let mut cmd = Command::new("/bin/sh");
    cmd.args([
        "-c",
        launcher,
        env!("CARGO_BIN_EXE_ghost-complete"),
        &config,
        REPORT,
    ])
    .env_clear()
    .env("HOME", home.path())
    .env("PATH", "/usr/bin:/bin")
    .env("TERM_PROGRAM", "ghostty")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    run_to_completion(cmd).1
}

/// An `init.zsh` from before the fallback marker sends the shell we exec'd
/// straight back into the proxy, in the same process. Exec'ing the shell
/// again from there would loop forever, so the proxy must give up instead.
#[test]
fn fallback_refuses_to_run_twice_in_one_process() {
    // `$$` is the pid `exec` hands to ghost-complete, so this reproduces
    // the state the proxy sees on its second start in the same process.
    let output = run_from_marked_shell(
        r#"export GHOST_COMPLETE_FALLBACK_PID=$$; exec "$0" --config "$1" --log-file /dev/null -- /bin/sh -c "$2""#,
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "",
        "the shell ran a second time; stderr: {stderr:?}"
    );
    assert_ne!(output.status.code(), Some(0));
    assert_ne!(output.status.code(), Some(REPORT_EXIT));
}

/// The same loop one fork removed: the fallback shell is a `$SHELL` wrapper
/// that runs zsh as a child, and that zsh's outdated `init.zsh` starts the
/// proxy again.
#[test]
fn fallback_refuses_to_run_from_a_child_of_the_fallback_shell() {
    let output = run_from_marked_shell(
        r#"export GHOST_COMPLETE_FALLBACK_PID=$$; "$0" --config "$1" --log-file /dev/null -- /bin/sh -c "$2"; echo "proxy exit=$?""#,
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with("proxy exit="),
        "the shell ran a second time; stdout: {stdout:?} stderr: {stderr:?}"
    );
    assert_ne!(stdout, "proxy exit=0\n");
}

/// The fallback shell sources the installed `init.zsh`, and only a current
/// one knows the fallback marker. So installed scripts are brought up to date
/// before anything that can fail, config loading included.
#[test]
fn stale_installed_scripts_are_refreshed_before_the_config_loads() {
    let home = tempfile::tempdir().unwrap();
    let shell_dir = home.path().join(".config/ghost-complete/shell");
    std::fs::create_dir_all(&shell_dir).unwrap();
    let installed_init = shell_dir.join("init.zsh");
    std::fs::write(&installed_init, "# installed by an older ghost-complete\n").unwrap();
    let config = write_config(home.path(), MALFORMED_CONFIG);

    let (pid, output) = run(
        home.path(),
        "ghostty",
        &["--config", &config, "--log-file", "/dev/null"],
    );

    assert_fell_back(pid, &output);
    let shipped_init = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../shell/init.zsh");
    assert_eq!(
        std::fs::read_to_string(&installed_init).unwrap(),
        std::fs::read_to_string(shipped_init).unwrap()
    );
}

#[test]
fn unopenable_log_file_does_not_stop_startup() {
    let home = tempfile::tempdir().unwrap();
    let log_file = home.path().join("missing-dir/ghost-complete.log");

    // An unknown terminal makes the proxy hand over to the shell as soon as
    // it gets that far, so reaching the shell proves startup went past
    // logging setup.
    let (pid, output) = run(
        home.path(),
        "not-a-real-terminal",
        &["--log-file", log_file.to_str().unwrap()],
    );

    assert_fell_back(pid, &output);
}
