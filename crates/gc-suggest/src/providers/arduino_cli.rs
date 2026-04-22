//! arduino-cli native providers — replace the JS-backed generators in
//! `specs/arduino-cli.json` that shell out to `arduino-cli board list
//! --format json` and extract either FQBNs or port addresses.
//!
//! At T2 only the FQBN-extracting `arduino_cli_boards` provider is
//! implemented. The port-address provider (T3) will share the
//! `run_board_list` subprocess helper and the `ArduinoBoardListOutput`
//! types defined here — this file is the one-stop home for both.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use serde::Deserialize;
use tokio::process::Command;

use super::{Provider, ProviderCtx};
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

/// Arduino's USB enumeration can be slow on the first scan after a cold
/// boot (the CLI walks every serial port and probes them in series).
/// 2 seconds is the Phase 3A plan's default for external-tool providers
/// and is tight enough to keep completions responsive while tolerating
/// the one-shot enumeration cost on a fresh shell.
const ARDUINO_CLI_TIMEOUT_MS: u64 = 2_000;

/// Top-level shape returned by `arduino-cli board list --format json`.
///
/// Newer arduino-cli versions (>= 1.0) wrap the array in an object under
/// `detected_ports`, while older versions emit the array directly. The
/// `#[serde(untagged)]` enum accepts both without an extra round-trip
/// through `serde_json::Value`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum ArduinoBoardListOutput {
    Wrapped { detected_ports: Vec<DetectedPort> },
    Flat(Vec<DetectedPort>),
}

impl ArduinoBoardListOutput {
    fn into_ports(self) -> Vec<DetectedPort> {
        match self {
            Self::Wrapped { detected_ports } => detected_ports,
            Self::Flat(ports) => ports,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct DetectedPort {
    #[serde(default)]
    pub(crate) port: Option<PortInfo>,
    #[serde(default)]
    pub(crate) matching_boards: Option<Vec<MatchingBoard>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PortInfo {
    #[serde(default)]
    pub(crate) address: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MatchingBoard {
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) fqbn: Option<String>,
}

/// Run `arduino-cli board list --format json` and parse the result.
/// Returns `None` on any failure (IO error, timeout, non-zero exit,
/// malformed JSON), always logged via `tracing::warn!` with structured
/// context. T3's port provider will reuse this helper unchanged.
pub(crate) async fn run_board_list(cwd: &Path) -> Option<ArduinoBoardListOutput> {
    run_board_list_with_binary(cwd, "arduino-cli").await
}

// Parametric binary name lets T3–T9 subprocess-failure tests inject a
// nonexistent path without mutating $PATH. Production callers go through
// `run_board_list` which hardcodes the real binary.
pub(crate) async fn run_board_list_with_binary(
    cwd: &Path,
    binary: &str,
) -> Option<ArduinoBoardListOutput> {
    let output = match tokio::time::timeout(
        Duration::from_millis(ARDUINO_CLI_TIMEOUT_MS),
        Command::new(binary)
            .args(["board", "list", "--format", "json"])
            .current_dir(cwd)
            .kill_on_drop(true)
            .output(),
    )
    .await
    {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            tracing::warn!("arduino-cli command failed: {e}");
            return None;
        }
        Err(_) => {
            tracing::warn!("arduino-cli board list timed out after {ARDUINO_CLI_TIMEOUT_MS}ms");
            return None;
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            exit = ?output.status.code(),
            stderr = %stderr.trim(),
            "arduino-cli board list failed"
        );
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_board_list_raw(&stdout)
}

/// Pure parsing step — split out so tests can exercise both wire shapes
/// without spawning a subprocess.
fn parse_board_list_raw(json: &str) -> Option<ArduinoBoardListOutput> {
    match serde_json::from_str::<ArduinoBoardListOutput>(json) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!("arduino-cli board list JSON parse failed: {e}");
            None
        }
    }
}

/// Extract FQBN suggestions from a parsed `arduino-cli board list`
/// payload. Mirrors the spec's JS filter semantics: drop entries that
/// have no matching board, then take the first matching board's fqbn
/// paired with `"{board_name} on port {port_address}"` as the
/// description.
fn suggestions_from_output(output: ArduinoBoardListOutput) -> Vec<Suggestion> {
    output
        .into_ports()
        .into_iter()
        .filter_map(|entry| {
            let board = entry.matching_boards.as_ref()?.first()?;
            let fqbn = board.fqbn.as_deref()?.to_string();
            let board_name = board.name.as_deref().unwrap_or("");
            let port_address = entry
                .port
                .as_ref()
                .and_then(|p| p.address.as_deref())
                .unwrap_or("");
            Some(Suggestion {
                text: fqbn,
                description: Some(format!("{board_name} on port {port_address}")),
                kind: SuggestionKind::Command,
                source: SuggestionSource::Provider,
                ..Default::default()
            })
        })
        .collect()
}

/// Test-visible parse-then-extract shim. Returns an empty `Vec` on any
/// failure (malformed JSON, empty input) — never panics, never errors.
#[cfg(test)]
fn parse_board_list(json: &str) -> Vec<Suggestion> {
    parse_board_list_raw(json)
        .map(suggestions_from_output)
        .unwrap_or_default()
}

/// FQBN-extracting provider — replaces `requires_js: true` generators
/// that call `arduino-cli board list --format json` and run a JS
/// function to project `matching_boards[0].fqbn` out of each entry.
pub struct ArduinoCliBoards;

impl Provider for ArduinoCliBoards {
    fn name(&self) -> &'static str {
        "arduino_cli_boards"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        let Some(output) = run_board_list(&ctx.cwd).await else {
            return Ok(Vec::new());
        };
        Ok(suggestions_from_output(output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wrapped_shape() {
        let json = r#"{
            "detected_ports": [
                {
                    "port": {"address": "/dev/ttyACM0"},
                    "matching_boards": [
                        {"name": "Arduino Uno", "fqbn": "arduino:avr:uno"}
                    ]
                }
            ]
        }"#;
        let suggestions = parse_board_list(json);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].text, "arduino:avr:uno");
        assert_eq!(
            suggestions[0].description.as_deref(),
            Some("Arduino Uno on port /dev/ttyACM0")
        );
        assert_eq!(suggestions[0].kind, SuggestionKind::Command);
        assert_eq!(suggestions[0].source, SuggestionSource::Provider);
    }

    #[test]
    fn parses_flat_array_shape() {
        let json = r#"[
            {
                "port": {"address": "/dev/ttyUSB0"},
                "matching_boards": [
                    {"name": "Arduino Mega", "fqbn": "arduino:avr:mega"}
                ]
            }
        ]"#;
        let suggestions = parse_board_list(json);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].text, "arduino:avr:mega");
        assert_eq!(
            suggestions[0].description.as_deref(),
            Some("Arduino Mega on port /dev/ttyUSB0")
        );
    }

    #[test]
    fn empty_detected_ports_yields_empty_vec() {
        assert!(parse_board_list(r#"{"detected_ports": []}"#).is_empty());
        assert!(parse_board_list("[]").is_empty());
    }

    #[test]
    fn entries_without_matching_boards_are_skipped() {
        // Matches the JS `filter(i => i.matching_boards)` semantics. Three
        // failure modes that must all be skipped without panicking: the
        // field is explicitly null, absent entirely, or present-but-empty.
        let json = r#"{
            "detected_ports": [
                {"port": {"address": "/dev/ttyS0"}, "matching_boards": null},
                {"port": {"address": "/dev/ttyS1"}},
                {"port": {"address": "/dev/ttyS2"}, "matching_boards": []},
                {
                    "port": {"address": "/dev/ttyACM0"},
                    "matching_boards": [
                        {"name": "Arduino Uno", "fqbn": "arduino:avr:uno"}
                    ]
                }
            ]
        }"#;
        let suggestions = parse_board_list(json);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].text, "arduino:avr:uno");
    }

    #[test]
    fn malformed_json_yields_empty_vec() {
        assert!(parse_board_list("not json").is_empty());
        assert!(parse_board_list("").is_empty());
        assert!(parse_board_list("{").is_empty());
    }

    #[test]
    fn suggestion_shape_for_mixed_fixture() {
        let json = r#"{
            "detected_ports": [
                {"port": {"address": "/dev/ttyS0"}, "matching_boards": null},
                {
                    "port": {"address": "/dev/ttyACM0"},
                    "matching_boards": [
                        {"name": "Arduino Uno", "fqbn": "arduino:avr:uno"}
                    ]
                }
            ]
        }"#;
        let suggestions = parse_board_list(json);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].text, "arduino:avr:uno");
        assert_eq!(
            suggestions[0].description,
            Some("Arduino Uno on port /dev/ttyACM0".to_string())
        );
        assert_eq!(suggestions[0].kind, SuggestionKind::Command);
        assert_eq!(suggestions[0].source, SuggestionSource::Provider);
    }

    #[tokio::test]
    async fn subprocess_failure_returns_empty_vec() {
        // Exercises the spawn-time "file not found" path by injecting a
        // binary name that cannot resolve anywhere on disk. No global
        // state is mutated, so this test is safe to run in parallel
        // alongside any other test in the workspace — and the same
        // pattern will serve T3–T9 without duplication.
        let tmp = tempfile::TempDir::new().unwrap();
        let result =
            run_board_list_with_binary(tmp.path(), "/nonexistent/arduino-cli-definitely-not-real")
                .await;
        assert!(
            result.is_none(),
            "expected None when the arduino-cli binary does not exist"
        );
    }
}
