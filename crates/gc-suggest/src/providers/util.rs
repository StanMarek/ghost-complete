use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::de::DeserializeOwned;
use tokio::process::Command;

pub(crate) async fn spawn_with_timeout<I, S>(
    cwd: &Path,
    binary: &str,
    args: I,
    env: Option<&HashMap<String, String>>,
    timeout: Duration,
) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new(binary);
    command.args(args).current_dir(cwd).kill_on_drop(true);
    if let Some(env) = env {
        command.envs(env);
    }

    let output = match tokio::time::timeout(timeout, command.output()).await {
        Err(_) => {
            return Err(anyhow!(
                "{binary} timed out after {}ms",
                timeout.as_millis()
            ))
        }
        Ok(Ok(output)) => output,
        Ok(Err(io_err)) => {
            let context = if io_err.kind() == std::io::ErrorKind::NotFound {
                format!("{binary}: binary not installed")
            } else {
                format!("{binary} command failed to spawn")
            };
            return Err(anyhow::Error::new(io_err).context(context));
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "{binary} exited with status {:?}: {}",
            output.status.code(),
            stderr.trim()
        ));
    }

    String::from_utf8(output.stdout).with_context(|| format!("{binary} emitted non-UTF8 stdout"))
}

/// Returns true if `error` represents a `binary not installed` spawn
/// failure (`io::ErrorKind::NotFound`). Callers use this to demote the
/// log level from `warn` to `trace` so a user without optional tools
/// (`kubectl`, `multipass`, `docker`, ...) does not see a warning every
/// time they trigger a related completion.
pub(crate) fn is_binary_missing(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|io_err| io_err.kind() == std::io::ErrorKind::NotFound)
}

pub(crate) fn parse_json_lines<T: DeserializeOwned>(stdout: &str) -> Result<Vec<T>> {
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).context("line-delimited JSON parse failed"))
        .collect()
}

pub(crate) fn parse_json_root<T: DeserializeOwned>(stdout: &str) -> Result<T> {
    serde_json::from_str(stdout).context("JSON document parse failed")
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use std::time::Duration;

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct Row {
        name: String,
    }

    #[test]
    fn parse_json_lines_ignores_blank_lines() {
        let rows: Vec<Row> =
            super::parse_json_lines("{\"name\":\"one\"}\n\n{\"name\":\"two\"}\n").unwrap();

        assert_eq!(
            rows,
            vec![
                Row {
                    name: "one".to_string(),
                },
                Row {
                    name: "two".to_string(),
                },
            ]
        );
    }

    #[test]
    fn parse_json_root_deserializes_whole_document() {
        let row: Row = super::parse_json_root("{\"name\":\"root\"}").unwrap();

        assert_eq!(
            row,
            Row {
                name: "root".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn spawn_with_timeout_returns_stdout_for_success() {
        let stdout = super::spawn_with_timeout(
            std::path::Path::new("/tmp"),
            "/bin/echo",
            ["ghost"],
            None,
            Duration::from_secs(1),
        )
        .await
        .unwrap();

        assert_eq!(stdout, "ghost\n");
    }
}
