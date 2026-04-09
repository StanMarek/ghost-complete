use std::path::Path;

use anyhow::Result;
use tokio::process::Command;

use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitQueryKind {
    Branches,
    Tags,
    Remotes,
}

pub fn generator_to_query_kind(type_str: &str) -> Option<GitQueryKind> {
    match type_str {
        "git_branches" => Some(GitQueryKind::Branches),
        "git_tags" => Some(GitQueryKind::Tags),
        "git_remotes" => Some(GitQueryKind::Remotes),
        _ => None,
    }
}

pub async fn git_suggestions(cwd: &Path, kind: GitQueryKind) -> Result<Vec<Suggestion>> {
    let lines = match kind {
        GitQueryKind::Branches => git_branches(cwd).await,
        GitQueryKind::Tags => git_tags(cwd).await,
        GitQueryKind::Remotes => git_remotes(cwd).await,
    };

    let (suggestion_kind, description) = match kind {
        GitQueryKind::Branches => (SuggestionKind::GitBranch, "branch"),
        GitQueryKind::Tags => (SuggestionKind::GitTag, "tag"),
        GitQueryKind::Remotes => (SuggestionKind::GitRemote, "remote"),
    };

    Ok(lines
        .into_iter()
        .map(|name| Suggestion {
            text: name,
            description: Some(description.to_string()),
            kind: suggestion_kind,
            source: SuggestionSource::Git,
            ..Default::default()
        })
        .collect())
}

async fn git_branches(cwd: &Path) -> Vec<String> {
    run_git(cwd, &["branch", "--format=%(refname:short)"]).await
}

async fn git_tags(cwd: &Path) -> Vec<String> {
    run_git(cwd, &["tag", "--list"]).await
}

async fn git_remotes(cwd: &Path) -> Vec<String> {
    run_git(cwd, &["remote"]).await
}

async fn run_git(cwd: &Path, args: &[&str]) -> Vec<String> {
    let output = match Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => {
            tracing::debug!("git command failed: {e}");
            return Vec::new();
        }
    };

    if !output.status.success() {
        let exit_code = output.status.code();
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Exit code 128 is git's "not a repo" code — expected noise outside
        // repos. Everything else (corrupt index, locked refs, dubious-ownership,
        // missing HEAD, broken hooks) is a real error worth surfacing so users
        // debugging empty completions can see the actual cause.
        if exit_code == Some(128) {
            tracing::debug!(
                args = ?args,
                stderr = %stderr.trim(),
                "git command failed (not a repo)"
            );
        } else {
            tracing::warn!(
                args = ?args,
                exit = ?exit_code,
                stderr = %stderr.trim(),
                "git command failed"
            );
        }
        return Vec::new();
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.trim().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generator_to_query_kind() {
        assert_eq!(
            generator_to_query_kind("git_branches"),
            Some(GitQueryKind::Branches)
        );
        assert_eq!(
            generator_to_query_kind("git_tags"),
            Some(GitQueryKind::Tags)
        );
        assert_eq!(
            generator_to_query_kind("git_remotes"),
            Some(GitQueryKind::Remotes)
        );
        assert_eq!(generator_to_query_kind("unknown"), None);
    }

    #[tokio::test]
    async fn test_git_branches_in_non_git_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let branches = git_branches(tmp.path()).await;
        assert!(branches.is_empty());
    }

    #[tokio::test]
    async fn test_run_git_non_repo_returns_empty_and_does_not_panic() {
        // Exercises the non-zero-exit branch of `run_git` directly. `git
        // branch` in a non-repo directory exits 128 ("not a repository"),
        // which must be handled gracefully — empty Vec, no panic, and the
        // stderr-logging branch must not blow up on non-UTF8 or empty stderr.
        let tmp = tempfile::TempDir::new().unwrap();
        let result = run_git(tmp.path(), &["branch", "--format=%(refname:short)"]).await;
        assert!(result.is_empty(), "expected empty Vec outside a git repo");

        // Also exercise a non-128 failure: invalid subcommand exits 1 (usage
        // error), which should route through the `warn!` branch rather than
        // the `debug!("not a repo")` branch.
        let result = run_git(tmp.path(), &["this-is-not-a-real-subcommand"]).await;
        assert!(
            result.is_empty(),
            "expected empty Vec for invalid git subcommand"
        );
    }

    #[tokio::test]
    async fn test_git_suggestions_returns_correct_kind() {
        // Run in workspace root — this is a real git repo
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        if workspace_root.join(".git").exists() {
            let suggestions = git_suggestions(&workspace_root, GitQueryKind::Branches)
                .await
                .unwrap();
            for s in &suggestions {
                assert_eq!(s.kind, SuggestionKind::GitBranch);
                assert_eq!(s.source, SuggestionSource::Git);
            }
            // We should have at least one branch (main/master)
            assert!(
                !suggestions.is_empty(),
                "expected at least one branch in the workspace git repo"
            );
        }
    }
}
