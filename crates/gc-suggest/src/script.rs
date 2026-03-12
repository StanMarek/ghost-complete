use std::path::Path;

use anyhow::{bail, Result};
use tokio::process::Command;

const MAX_SUBSTITUTION_LEN: usize = 1024;
const SHELL_METACHARACTERS: &[char] = &['|', ';', '&', '`', '$'];

/// Execute a shell command as an array (no shell interpolation), return stdout.
///
/// Stderr is discarded but logged at debug level. The `GHOST_COMPLETE_ACTIVE`
/// env var is stripped so child processes don't think they're inside the proxy.
/// On timeout the child is killed and an error is returned.
pub async fn run_script(argv: &[&str], cwd: &Path, timeout_ms: u64) -> Result<String> {
    if argv.is_empty() {
        bail!("empty script command");
    }

    let mut cmd = Command::new(argv[0]);
    if argv.len() > 1 {
        cmd.args(&argv[1..]);
    }
    cmd.current_dir(cwd);
    cmd.env_remove("GHOST_COMPLETE_ACTIVE");
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    cmd.kill_on_drop(true);

    let child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("script execution failed for {:?}: {e}", argv))?;

    // `kill_on_drop(true)` ensures the child is killed if the future is dropped on timeout.
    let output = match tokio::time::timeout(
        std::time::Duration::from_millis(timeout_ms),
        child.wait_with_output(),
    )
    .await
    {
        Ok(result) => {
            result.map_err(|e| anyhow::anyhow!("script I/O error for {:?}: {e}", argv))?
        }
        Err(_) => {
            bail!("script timed out after {timeout_ms}ms: {:?}", argv);
        }
    };

    if !output.stderr.is_empty() {
        tracing::debug!(
            "script stderr for {:?}: {}",
            argv,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Substitute `{prev_token}` and `{current_token}` placeholders in a command template.
///
/// Each substitution value is truncated to [`MAX_SUBSTITUTION_LEN`] bytes.
/// A warning is logged if the substituted result contains shell metacharacters.
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
                let truncated = &prev[..prev.len().min(MAX_SUBSTITUTION_LEN)];
                result = result.replace("{prev_token}", truncated);
            } else {
                result = result.replace("{prev_token}", "");
            }
            if let Some(current) = current_token {
                let truncated = &current[..current.len().min(MAX_SUBSTITUTION_LEN)];
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
}
