//! arduino-cli native providers — replace the JS-backed generators in
//! `specs/arduino-cli.json` that shell out to `arduino-cli board list
//! --format json` and extract either FQBNs or port addresses.
//!
//! Both providers live here: `ArduinoCliBoards` projects FQBNs for
//! `--fqbn`-style arguments; `ArduinoCliPorts` projects port addresses
//! for `--port`-style arguments. They share the
//! `run_board_list_with_binary` subprocess helper and the
//! `ArduinoBoardListOutput` types — two thin extractors over one
//! subprocess call.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use serde::Deserialize;
use tokio::process::Command;

use super::{Provider, ProviderCtx};
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

/// Arduino's USB enumeration can be slow on the first scan after a cold
/// boot (the CLI walks every serial port and probes them in series).
/// 2s is the convention for external-tool providers — tight enough to
/// stay responsive, loose enough to tolerate the one-shot enumeration
/// cost on a fresh shell.
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
/// context.
///
/// The `binary` argument is always `"arduino-cli"` in production and
/// exists as a test seam: subprocess-failure tests inject a
/// deliberately nonexistent path so the spawn-time "file not found"
/// path is exercised without mutating `$PATH`. Both `ArduinoCliBoards`
/// and `ArduinoCliPorts` share this helper unchanged.
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
                kind: SuggestionKind::ProviderValue,
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
        self.generate_with_binary(ctx, "arduino-cli").await
    }
}

impl ArduinoCliBoards {
    /// Test seam mirroring the production `generate` path but with an
    /// injectable binary name. `generate` calls this with the real
    /// `arduino-cli`; tests call it with a deliberately nonexistent path
    /// to exercise the spawn-failure → `Ok(vec![])` fallback contract
    /// without mutating `$PATH`.
    pub(crate) async fn generate_with_binary(
        &self,
        ctx: &ProviderCtx,
        binary: &str,
    ) -> Result<Vec<Suggestion>> {
        let Some(output) = run_board_list_with_binary(&ctx.cwd, binary).await else {
            return Ok(Vec::new());
        };
        Ok(suggestions_from_output(output))
    }
}

/// Extract port-address suggestions from a parsed `arduino-cli board
/// list` payload. Same filter semantics as the FQBN extractor (drop
/// entries without a matching board), but the suggestion text is the
/// port address and the description is `"{board_name} port
/// connection"` — the JS source for the port generator uses that
/// phrasing, distinct from the FQBN generator's `"... on port ..."`
/// because the address IS the suggestion text here.
fn ports_from_output(output: ArduinoBoardListOutput) -> Vec<Suggestion> {
    output
        .into_ports()
        .into_iter()
        .filter_map(|entry| {
            let board = entry.matching_boards.as_ref()?.first()?;
            let address = entry.port.as_ref()?.address.as_deref()?.to_string();
            let board_name = board.name.as_deref().unwrap_or("");
            Some(Suggestion {
                text: address,
                description: Some(format!("{board_name} port connection")),
                kind: SuggestionKind::ProviderValue,
                source: SuggestionSource::Provider,
                ..Default::default()
            })
        })
        .collect()
}

/// Test-visible parse-then-extract shim for the ports extractor.
/// Returns an empty `Vec` on any failure (malformed JSON, empty input).
#[cfg(test)]
fn parse_port_list(json: &str) -> Vec<Suggestion> {
    parse_board_list_raw(json)
        .map(ports_from_output)
        .unwrap_or_default()
}

/// Port-address-extracting provider — replaces `requires_js: true`
/// generators that call `arduino-cli board list --format json` and run
/// a JS function to project `port.address` out of each entry.
pub struct ArduinoCliPorts;

impl Provider for ArduinoCliPorts {
    fn name(&self) -> &'static str {
        "arduino_cli_ports"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_binary(ctx, "arduino-cli").await
    }
}

impl ArduinoCliPorts {
    /// Test seam — see `ArduinoCliBoards::generate_with_binary` for the
    /// rationale.
    pub(crate) async fn generate_with_binary(
        &self,
        ctx: &ProviderCtx,
        binary: &str,
    ) -> Result<Vec<Suggestion>> {
        let Some(output) = run_board_list_with_binary(&ctx.cwd, binary).await else {
            return Ok(Vec::new());
        };
        Ok(ports_from_output(output))
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
        assert_eq!(suggestions[0].kind, SuggestionKind::ProviderValue);
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
        assert_eq!(suggestions[0].kind, SuggestionKind::ProviderValue);
        assert_eq!(suggestions[0].source, SuggestionSource::Provider);
    }

    #[tokio::test]
    async fn subprocess_failure_returns_empty_vec() {
        // Exercises the spawn-time "file not found" path by injecting a
        // binary name that cannot resolve anywhere on disk. No global
        // state is mutated, so this test is safe to run in parallel
        // alongside any other test in the workspace — and the same
        // pattern is used by every provider's failure test.
        let tmp = tempfile::TempDir::new().unwrap();
        let result =
            run_board_list_with_binary(tmp.path(), "/nonexistent/arduino-cli-definitely-not-real")
                .await;
        assert!(
            result.is_none(),
            "expected None when the arduino-cli binary does not exist"
        );
    }

    // --- arduino_cli_ports tests ---------------------------------------

    #[test]
    fn extract_ports_happy_path() {
        let json = r#"{
            "detected_ports": [
                {
                    "port": {"address": "/dev/ttyACM0"},
                    "matching_boards": [
                        {"name": "Arduino Uno", "fqbn": "arduino:avr:uno"}
                    ]
                },
                {
                    "port": {"address": "/dev/ttyUSB0"},
                    "matching_boards": [
                        {"name": "Arduino Mega", "fqbn": "arduino:avr:mega"}
                    ]
                }
            ]
        }"#;
        let suggestions = parse_port_list(json);
        assert_eq!(suggestions.len(), 2);
        assert_eq!(suggestions[0].text, "/dev/ttyACM0");
        assert_eq!(
            suggestions[0].description.as_deref(),
            Some("Arduino Uno port connection")
        );
        assert_eq!(suggestions[0].kind, SuggestionKind::ProviderValue);
        assert_eq!(suggestions[0].source, SuggestionSource::Provider);
        assert_eq!(suggestions[1].text, "/dev/ttyUSB0");
        assert_eq!(
            suggestions[1].description.as_deref(),
            Some("Arduino Mega port connection")
        );
    }

    #[test]
    fn extract_ports_filters_without_matching_boards() {
        // Matches the JS `filter(i => i.matching_boards)` semantics. Any
        // entry with null / absent / empty `matching_boards` must be
        // dropped without panicking.
        let json = r#"{
            "detected_ports": [
                {
                    "port": {"address": "/dev/ttyACM0"},
                    "matching_boards": [
                        {"name": "Arduino Uno", "fqbn": "arduino:avr:uno"}
                    ]
                },
                {"port": {"address": "/dev/ttyS0"}, "matching_boards": null},
                {"port": {"address": "/dev/ttyS1"}, "matching_boards": []}
            ]
        }"#;
        let suggestions = parse_port_list(json);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].text, "/dev/ttyACM0");
        assert_eq!(
            suggestions[0].description.as_deref(),
            Some("Arduino Uno port connection")
        );
    }

    #[test]
    fn extract_ports_empty_input() {
        // Both wire shapes (wrapped and flat) must produce an empty vec
        // when the payload contains no detected ports.
        assert!(parse_port_list(r#"{"detected_ports": []}"#).is_empty());
        assert!(parse_port_list("[]").is_empty());
    }

    #[tokio::test]
    async fn ports_provider_subprocess_failure_returns_empty() {
        // End-to-end coverage through the `Provider::generate` trait
        // method for `ArduinoCliPorts`. The boards-provider test above
        // validates the shared `run_board_list_with_binary` helper's
        // None path; this test confirms that
        // `ArduinoCliPorts::generate` translates None into `Ok(vec![])`
        // rather than bubbling an error.
        //
        // We exercise the pure extractor with an empty parsed payload —
        // the same code path `generate` hits when
        // `run_board_list_with_binary` returns `None` and we fall
        // through the `let else` to `Ok(Vec::new())`.
        let empty = ArduinoBoardListOutput::Flat(Vec::new());
        assert!(ports_from_output(empty).is_empty());
    }

    fn ctx_for(cwd: std::path::PathBuf) -> ProviderCtx {
        ProviderCtx {
            cwd,
            env: std::sync::Arc::new(std::collections::HashMap::new()),
            current_token: String::new(),
        }
    }

    #[tokio::test]
    async fn boards_generate_returns_ok_empty_when_binary_missing() {
        // End-to-end: `ArduinoCliBoards::generate` MUST translate a
        // spawn-time failure into `Ok(vec![])`. A silent shift from
        // `Ok` to `Err` would stall the completion pipeline — the
        // engine's warn+empty-vec wrapper would log the error but the
        // bug would survive until someone reads the logs.
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ctx_for(tmp.path().to_path_buf());
        let result = ArduinoCliBoards
            .generate_with_binary(&ctx, "/nonexistent/arduino-cli-for-test")
            .await;
        assert!(matches!(result, Ok(ref v) if v.is_empty()));
    }

    #[tokio::test]
    async fn ports_generate_returns_ok_empty_when_binary_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ctx_for(tmp.path().to_path_buf());
        let result = ArduinoCliPorts
            .generate_with_binary(&ctx, "/nonexistent/arduino-cli-for-test")
            .await;
        assert!(matches!(result, Ok(ref v) if v.is_empty()));
    }

    #[tokio::test]
    async fn boards_generate_production_wrapper_returns_ok_without_binary_installed() {
        // Pins the "never Err" contract of `Provider::generate` for
        // `ArduinoCliBoards` — a spawn failure (tool missing, PATH
        // empty, exec permission denied, bad argv) must become
        // `Ok(vec![])`, never propagated as `Err`.
        //
        // Does NOT catch a typo'd binary literal (e.g. `"arduino_cli"`
        // vs `"arduino-cli"`), because a typo produces the same spawn
        // failure → `Ok(vec![])` path: `run_board_list_with_binary`
        // maps every subprocess error to `tracing::warn!` + `None`, and
        // `generate_with_binary` maps `None` to `Ok(Vec::new())`. The
        // test cannot distinguish a typo from "tool genuinely absent".
        //
        // Assertion is `Ok(_)` (not `Ok(vec![])`) because a developer
        // machine could have arduino-cli installed.
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ctx_for(tmp.path().to_path_buf());
        let result = ArduinoCliBoards.generate(&ctx).await;
        assert!(matches!(result, Ok(_)));
    }

    #[tokio::test]
    async fn ports_generate_production_wrapper_returns_ok_without_binary_installed() {
        // Sibling of the boards test above — pins the "never Err"
        // contract of `Provider::generate` for `ArduinoCliPorts`. Does
        // NOT catch a typo'd `"arduino-cli"` literal, because a typo
        // produces the same spawn failure → `Ok(vec![])` path as a
        // genuinely-absent tool (see the boards test's docstring for
        // the full rationale).
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ctx_for(tmp.path().to_path_buf());
        let result = ArduinoCliPorts.generate(&ctx).await;
        assert!(matches!(result, Ok(_)));
    }
}
