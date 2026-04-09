use std::path::Path;

use anyhow::{bail, Result};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

const MAX_SUBSTITUTION_LEN: usize = 1024;
const SHELL_METACHARACTERS: &[char] = &['|', ';', '&', '`', '$'];

/// Hard cap on script generator stdout. Anything beyond this is dropped, the
/// child is killed, and a `tracing::warn!` is emitted. Prevents a runaway or
/// malicious generator from allocating arbitrary memory inside the timeout
/// window. See audit MED-17.
pub(crate) const MAX_GENERATOR_STDOUT_BYTES: usize = 1024 * 1024;

/// Execute a shell command as an array (no shell interpolation), return stdout.
///
/// Stdout is read into a 1 MiB-bounded buffer (`MAX_GENERATOR_STDOUT_BYTES`).
/// If the cap is hit the child is killed, a warning is logged, and the
/// truncated bytes are still returned so the downstream transform pipeline
/// can process what was collected.
///
/// Stderr is drained concurrently (also capped at 1 MiB) to avoid pipe-fill
/// deadlock. On non-zero exit with non-empty stderr, the stderr contents
/// are logged at `warn!` — this is the primary diagnostic path for
/// generator failures like `gh auth status` without credentials. On
/// non-zero exit with empty stderr, the exit code is logged at `debug!`
/// (the common "script legitimately exits non-zero" case).
///
/// The child inherits the full process environment (minus
/// `GHOST_COMPLETE_ACTIVE`) because generators like `gh`, `aws`, and
/// `kubectl` require auth tokens to produce useful completions. On
/// timeout the child is killed and an error is returned (stderr
/// captured before the timeout fires is dropped — the timeout's async
/// cancellation discards the inner state; a future improvement could
/// hoist the buffers via interior mutability).
/// Non-zero exit codes return an empty string (avoids error messages as suggestions).
pub async fn run_script(argv: &[&str], cwd: &Path, timeout_ms: u64) -> Result<String> {
    if argv.is_empty() {
        bail!("empty script command");
    }

    let mut cmd = Command::new(argv[0]);
    if argv.len() > 1 {
        cmd.args(&argv[1..]);
    }
    cmd.current_dir(cwd);
    // HIGH-13 (DEFERRED): env isolation via a deny-list was rejected because
    // it silently breaks authenticated completions for gh/aws/kubectl/npm and
    // similar tools that rely on inherited tokens (GH_TOKEN, AWS_PROFILE,
    // KUBECONFIG, NPM_TOKEN, ...). If ever revisited, use an allow-list with
    // explicit per-spec opt-in. See PR #66 body for the full rationale.
    //
    // Generators inherit the full process environment because many legitimate
    // completions require auth tokens (GITHUB_TOKEN for `gh`, AWS credentials
    // for `aws`, etc.). The specs are either embedded in the binary (trusted)
    // or user-installed. If an attacker can write to
    // ~/.config/ghost-complete/specs/, they already have shell access.
    //
    // The only var we strip is our own re-entry guard, so nested shells don't
    // think they're still inside the proxy.
    cmd.env_remove("GHOST_COMPLETE_ACTIVE");
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    cmd.kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("script execution failed for {:?}: {e}", argv))?;

    let mut stdout = child.stdout.take().expect("stdout was configured as piped");
    let mut stderr = child.stderr.take().expect("stderr was configured as piped");

    // Drive stdout and stderr concurrently with a hard byte cap on stdout.
    // Stderr must be drained in parallel — otherwise a chatty generator can
    // fill the stderr pipe and block on its next stderr write, deadlocking
    // our stdout reader. `kill_on_drop(true)` reaps the child if the future
    // is dropped on timeout.
    let read_result = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), async {
        let mut stdout_buf: Vec<u8> = Vec::with_capacity(8192);
        let mut stderr_buf: Vec<u8> = Vec::new();
        let mut out_chunk = [0u8; 8192];
        let mut err_chunk = [0u8; 4096];
        let mut stdout_done = false;
        let mut stderr_done = false;
        let mut truncated = false;

        while !(truncated || stdout_done && stderr_done) {
            tokio::select! {
                biased;
                r = stdout.read(&mut out_chunk), if !stdout_done => {
                    let n = r?;
                    if n == 0 {
                        stdout_done = true;
                    } else {
                        let remaining =
                            MAX_GENERATOR_STDOUT_BYTES.saturating_sub(stdout_buf.len());
                        let take = n.min(remaining);
                        stdout_buf.extend_from_slice(&out_chunk[..take]);
                        if stdout_buf.len() >= MAX_GENERATOR_STDOUT_BYTES {
                            truncated = true;
                        }
                    }
                }
                r = stderr.read(&mut err_chunk), if !stderr_done => {
                    let n = r?;
                    if n == 0 {
                        stderr_done = true;
                    } else {
                        // Drain the pipe but cap retained bytes so a
                        // chatty stderr can't grow unbounded either.
                        let cap = MAX_GENERATOR_STDOUT_BYTES;
                        if stderr_buf.len() < cap {
                            let take = n.min(cap - stderr_buf.len());
                            stderr_buf.extend_from_slice(&err_chunk[..take]);
                        }
                    }
                }
            }
        }
        Ok::<_, std::io::Error>((stdout_buf, stderr_buf, truncated))
    })
    .await;

    match read_result {
        Ok(Ok((stdout_buf, stderr_buf, truncated))) => {
            if truncated {
                tracing::warn!(
                    "script generator stdout exceeded {} bytes; truncating and killing process: {:?}",
                    MAX_GENERATOR_STDOUT_BYTES,
                    argv
                );
                let _ = child.kill().await;
                // Reap the zombie so the process table doesn't fill up.
                // Drop status — we're returning truncated bytes regardless.
                let _ = child.wait().await;
                return Ok(String::from_utf8_lossy(&stdout_buf).to_string());
            }

            let status = child
                .wait()
                .await
                .map_err(|e| anyhow::anyhow!("script wait error for {:?}: {e}", argv))?;

            if !status.success() {
                let code = status.code().unwrap_or(-1);
                if stderr_buf.is_empty() {
                    // Common case: script legitimately exits non-zero (e.g.,
                    // `git rev-parse --show-toplevel` in a non-repo). Debug level
                    // to avoid noise for the expected failure modes.
                    tracing::debug!("script {:?} exited with code {code} (no stderr)", argv);
                } else {
                    // Actionable failure: the script wrote to stderr. Surface it at
                    // warn level so users debugging "why are my completions empty"
                    // can see the real error. This is the whole reason the
                    // concurrent stderr drain exists — see the comment above
                    // the read loop for the rationale.
                    let stderr_str = String::from_utf8_lossy(&stderr_buf);
                    tracing::warn!(
                        "script {:?} exited with code {code}: {}",
                        argv,
                        stderr_str.trim_end()
                    );
                }
                return Ok(String::new());
            }

            if !stderr_buf.is_empty() {
                tracing::debug!(
                    "script stderr for {:?}: {}",
                    argv,
                    String::from_utf8_lossy(&stderr_buf)
                );
            }

            Ok(String::from_utf8_lossy(&stdout_buf).to_string())
        }
        Ok(Err(e)) => Err(anyhow::anyhow!("script I/O error for {:?}: {e}", argv)),
        Err(_) => {
            bail!("script timed out after {timeout_ms}ms: {:?}", argv);
        }
    }
}

/// Truncate a string to at most `max_bytes` without splitting a multi-byte
/// UTF-8 character. Returns the full string if it already fits.
fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Substitute `{prev_token}` and `{current_token}` placeholders in a command template.
///
/// Each substitution value is truncated to [`MAX_SUBSTITUTION_LEN`] bytes
/// (at a valid UTF-8 boundary). A warning is logged if the substituted result
/// contains shell metacharacters.
pub fn substitute_template(
    template: &[String],
    prev_token: Option<&str>,
    current_token: Option<&str>,
) -> Vec<String> {
    template
        .iter()
        .map(|part| {
            let mut result = part.clone();
            if let Some(prev) = prev_token {
                let truncated = truncate_utf8(prev, MAX_SUBSTITUTION_LEN);
                result = result.replace("{prev_token}", truncated);
            } else {
                result = result.replace("{prev_token}", "");
            }
            if let Some(current) = current_token {
                let truncated = truncate_utf8(current, MAX_SUBSTITUTION_LEN);
                result = result.replace("{current_token}", truncated);
            } else {
                result = result.replace("{current_token}", "");
            }
            if result != *part && result.chars().any(|c| SHELL_METACHARACTERS.contains(&c)) {
                tracing::warn!(
                    "shell metacharacter in substituted script argument: {:?}",
                    result
                );
            }
            result
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_run_script_echo() {
        let result = run_script(&["echo", "hello world"], Path::new("/tmp"), 5000)
            .await
            .unwrap();
        assert_eq!(result.trim(), "hello world");
    }

    #[tokio::test]
    async fn test_run_script_timeout() {
        let result = run_script(&["sleep", "10"], Path::new("/tmp"), 100).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("timeout") || err.contains("timed out"),
            "expected timeout error: {err}"
        );
    }

    #[tokio::test]
    async fn test_run_script_nonexistent_command() {
        let result = run_script(&["nonexistent_command_xyz"], Path::new("/tmp"), 5000).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_run_script_empty_command() {
        let result = run_script(&[], Path::new("/tmp"), 5000).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_run_script_multiline_output() {
        let result = run_script(&["printf", "foo\nbar\nbaz"], Path::new("/tmp"), 5000)
            .await
            .unwrap();
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines, vec!["foo", "bar", "baz"]);
    }

    #[test]
    fn test_substitute_template_prev_token() {
        let template = vec![
            "cmd".to_string(),
            "--flag".to_string(),
            "{prev_token}".to_string(),
        ];
        let result = substitute_template(&template, Some("value"), None);
        assert_eq!(result, vec!["cmd", "--flag", "value"]);
    }

    #[test]
    fn test_substitute_template_current_token() {
        let template = vec!["cmd".to_string(), "{current_token}".to_string()];
        let result = substitute_template(&template, None, Some("partial"));
        assert_eq!(result, vec!["cmd", "partial"]);
    }

    #[test]
    fn test_substitute_template_length_limit() {
        let long = "a".repeat(2000);
        let template = vec!["cmd".to_string(), "{prev_token}".to_string()];
        let result = substitute_template(&template, Some(&long), None);
        assert!(result[1].len() <= 1024);
    }

    #[test]
    fn test_substitute_template_multibyte_truncation() {
        // Each emoji is 4 bytes. 256 emojis = 1024 bytes exactly. 257 = 1028.
        let emojis = "\u{1F600}".repeat(257);
        assert_eq!(emojis.len(), 1028);
        let template = vec!["cmd".to_string(), "{prev_token}".to_string()];
        let result = substitute_template(&template, Some(&emojis), None);
        // Must truncate at a valid char boundary ≤ 1024
        assert!(result[1].len() <= 1024);
        assert!(result[1].is_char_boundary(result[1].len()));
    }

    #[tokio::test]
    async fn test_run_script_nonzero_exit_returns_empty() {
        let result = run_script(&["sh", "-c", "exit 1"], Path::new("/tmp"), 5000)
            .await
            .unwrap();
        assert!(
            result.is_empty(),
            "non-zero exit should return empty string, got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_run_script_nonzero_exit_with_stderr() {
        // Regression for CRIT-B: a script that fails AND writes to stderr
        // (e.g. `gh auth status` without credentials) must still return an
        // empty String to the caller. The stderr contents are logged at
        // warn! inside run_script — we don't assert on logs here, but the
        // return contract is what callers rely on.
        let result = run_script(
            &["sh", "-c", "echo 'fake auth error' >&2; exit 42"],
            Path::new("/tmp"),
            5000,
        )
        .await
        .expect("non-zero exit with stderr must not surface as an error");
        assert!(
            result.is_empty(),
            "non-zero exit with stderr should return empty string, got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_run_script_ghost_complete_active_stripped() {
        // Verify run_script's env_remove("GHOST_COMPLETE_ACTIVE") works.
        // We must set the var in the parent process so run_script's child
        // would inherit it WITHOUT the env_remove. This requires set_var,
        // which mutates global state — acceptable at MSRV 1.75 where it's
        // safe, and the mutation window is brief.
        //
        // NOTE: This test must go through run_script (not raw Command) to
        // catch regressions if the env_remove line is accidentally deleted.
        std::env::set_var("GHOST_COMPLETE_ACTIVE", "1");
        let result = run_script(
            &["sh", "-c", "echo ${GHOST_COMPLETE_ACTIVE:-stripped}"],
            Path::new("/tmp"),
            5000,
        )
        .await
        .unwrap();
        std::env::remove_var("GHOST_COMPLETE_ACTIVE");
        assert_eq!(
            result.trim(),
            "stripped",
            "run_script must strip GHOST_COMPLETE_ACTIVE from generator env"
        );
    }

    #[tokio::test]
    async fn test_run_script_stdout_capped_at_1mb() {
        // Generator producing ~2MB of output: stdout must be truncated to
        // exactly MAX_GENERATOR_STDOUT_BYTES and the call must still return
        // Ok with the truncated bytes (not an error).
        let result = run_script(
            &["sh", "-c", "yes a | head -c 2000000"],
            Path::new("/tmp"),
            10_000,
        )
        .await
        .expect("truncation must not surface as an error");

        assert_eq!(
            result.len(),
            MAX_GENERATOR_STDOUT_BYTES,
            "stdout must be truncated to the 1MB cap"
        );
    }

    #[tokio::test]
    async fn test_run_script_stdout_under_cap_unchanged() {
        // Sanity: an output well under the cap is returned in full.
        let result = run_script(
            &["sh", "-c", "yes a | head -c 1024"],
            Path::new("/tmp"),
            10_000,
        )
        .await
        .unwrap();

        assert_eq!(result.len(), 1024);
    }

    #[tokio::test]
    async fn test_run_script_auth_vars_inherited() {
        // Auth tokens MUST be inherited — generators like `gh` need GITHUB_TOKEN,
        // `aws` needs AWS_SECRET_ACCESS_KEY, etc. Stripping these silently breaks
        // authenticated completions. See audit HIGH-13 discussion.
        // We use HOME which always exists and matches secret-like patterns (no it
        // doesn't — but PATH is guaranteed and proves full env inheritance).
        // More importantly: verify no env_clear() is in effect by checking the
        // child sees the same env var count as the parent (within tolerance).
        let result = run_script(&["sh", "-c", "env | wc -l"], Path::new("/tmp"), 5000)
            .await
            .unwrap();
        let child_vars: usize = result.trim().parse().unwrap_or(0);
        // Parent env count minus 1 (GHOST_COMPLETE_ACTIVE removed).
        // Child should have nearly the same number — if env_clear() were
        // in effect, child_vars would be ~7 instead of ~50+.
        let parent_vars = std::env::vars().count();
        assert!(
            child_vars >= parent_vars.saturating_sub(5),
            "child should inherit full env (got {child_vars} vars, parent has {parent_vars})"
        );
    }
}
