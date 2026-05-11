use std::path::Path;
use std::time::Duration;

use anyhow::Result;

use super::util::spawn_with_timeout;
use super::{Provider, ProviderCtx};
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

const TMUX_TIMEOUT_MS: u64 = 2_000;

#[derive(Debug, Clone, Copy)]
enum TmuxQuery {
    Sessions,
    Windows,
    Panes,
    Clients,
}

impl TmuxQuery {
    fn args(self) -> &'static [&'static str] {
        match self {
            Self::Sessions => &[
                "list-sessions",
                "-F",
                "#{session_name}\t#{session_windows} windows",
            ],
            Self::Windows => &[
                "list-windows",
                "-a",
                "-F",
                "#{session_name}:#{window_index}:#{window_name}\t#{window_panes} panes",
            ],
            Self::Panes => &[
                "list-panes",
                "-a",
                "-F",
                "#{session_name}:#{window_index}.#{pane_index}\t#{pane_current_command}",
            ],
            Self::Clients => &["list-clients", "-F", "#{client_name}\t#{session_name}"],
        }
    }
}

pub(crate) fn parse_tmux_state_output(stdout: &str) -> Vec<Suggestion> {
    stdout
        .lines()
        .filter_map(|line| {
            let line = line.trim_end();
            if line.trim().is_empty() {
                return None;
            }

            let (target, description) = match line.split_once('\t') {
                Some((target, description)) => (target.trim(), description.trim()),
                None => (line.trim(), ""),
            };

            if target.is_empty() {
                return None;
            }

            Some(Suggestion {
                text: target.to_string(),
                description: (!description.is_empty()).then(|| description.to_string()),
                kind: SuggestionKind::ProviderValue,
                source: SuggestionSource::Provider,
                ..Default::default()
            })
        })
        .collect()
}

async fn run_tmux_with_binary(cwd: &Path, binary: &str, query: TmuxQuery) -> Option<String> {
    match spawn_with_timeout(
        cwd,
        binary,
        query.args().iter().copied(),
        None,
        Duration::from_millis(TMUX_TIMEOUT_MS),
    )
    .await
    {
        Ok(stdout) => Some(stdout),
        Err(error) => {
            tracing::warn!(binary, error = %error, "tmux provider command failed");
            None
        }
    }
}

async fn generate_tmux(
    ctx: &ProviderCtx,
    binary: &str,
    query: TmuxQuery,
) -> Result<Vec<Suggestion>> {
    if !ctx.env.contains_key("TMUX") {
        return Ok(Vec::new());
    }

    let Some(stdout) = run_tmux_with_binary(&ctx.cwd, binary, query).await else {
        return Ok(Vec::new());
    };

    Ok(parse_tmux_state_output(&stdout))
}

pub struct TmuxSessions;

impl Provider for TmuxSessions {
    fn name(&self) -> &'static str {
        "tmux_sessions"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_binary(ctx, "tmux").await
    }
}

impl TmuxSessions {
    pub(crate) async fn generate_with_binary(
        &self,
        ctx: &ProviderCtx,
        binary: &str,
    ) -> Result<Vec<Suggestion>> {
        generate_tmux(ctx, binary, TmuxQuery::Sessions).await
    }
}

pub struct TmuxWindows;

impl Provider for TmuxWindows {
    fn name(&self) -> &'static str {
        "tmux_windows"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_binary(ctx, "tmux").await
    }
}

impl TmuxWindows {
    pub(crate) async fn generate_with_binary(
        &self,
        ctx: &ProviderCtx,
        binary: &str,
    ) -> Result<Vec<Suggestion>> {
        generate_tmux(ctx, binary, TmuxQuery::Windows).await
    }
}

pub struct TmuxPanes;

impl Provider for TmuxPanes {
    fn name(&self) -> &'static str {
        "tmux_panes"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_binary(ctx, "tmux").await
    }
}

impl TmuxPanes {
    pub(crate) async fn generate_with_binary(
        &self,
        ctx: &ProviderCtx,
        binary: &str,
    ) -> Result<Vec<Suggestion>> {
        generate_tmux(ctx, binary, TmuxQuery::Panes).await
    }
}

pub struct TmuxClients;

impl Provider for TmuxClients {
    fn name(&self) -> &'static str {
        "tmux_clients"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_binary(ctx, "tmux").await
    }
}

impl TmuxClients {
    pub(crate) async fn generate_with_binary(
        &self,
        ctx: &ProviderCtx,
        binary: &str,
    ) -> Result<Vec<Suggestion>> {
        generate_tmux(ctx, binary, TmuxQuery::Clients).await
    }
}
