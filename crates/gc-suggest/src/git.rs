use std::path::Path;

use anyhow::Result;
use tokio::process::Command;

use crate::priority::Priority;
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
    let current_branch = match kind {
        GitQueryKind::Branches => current_git_branch(cwd).await,
        GitQueryKind::Tags | GitQueryKind::Remotes => None,
    };
    let mut lines = match kind {
        GitQueryKind::Branches => git_branches(cwd).await,
        GitQueryKind::Tags => git_tags(cwd).await,
        GitQueryKind::Remotes => git_remotes(cwd).await,
    };
    move_current_branch_first(&mut lines, current_branch.as_deref());

    let (suggestion_kind, description) = match kind {
        GitQueryKind::Branches => (SuggestionKind::GitBranch, "branch"),
        GitQueryKind::Tags => (SuggestionKind::GitTag, "tag"),
        GitQueryKind::Remotes => (SuggestionKind::GitRemote, "remote"),
    };

    Ok(lines
        .into_iter()
        .map(|name| {
            let is_current_branch = current_branch.as_deref() == Some(name.as_str());
            Suggestion {
                text: name,
                description: Some(
                    if is_current_branch {
                        "current branch"
                    } else {
                        description
                    }
                    .to_string(),
                ),
                kind: suggestion_kind,
                source: SuggestionSource::Git,
                priority: is_current_branch.then(|| Priority::new(100)),
                ..Default::default()
            }
        })
        .collect())
}

async fn git_branches(cwd: &Path) -> Vec<String> {
    run_git(cwd, &["branch", "--format=%(refname:short)"]).await
}

async fn current_git_branch(cwd: &Path) -> Option<String> {
    run_git(cwd, &["branch", "--show-current"])
        .await
        .into_iter()
        .next()
}

fn move_current_branch_first(branches: &mut Vec<String>, current_branch: Option<&str>) {
    let Some(current_branch) = current_branch else {
        return;
    };
    let Some(index) = branches.iter().position(|branch| branch == current_branch) else {
        return;
    };
    if index > 0 {
        let branch = branches.remove(index);
        branches.insert(0, branch);
    }
}

async fn git_tags(cwd: &Path) -> Vec<String> {
    run_git(cwd, &["tag", "--list"]).await
}

async fn git_remotes(cwd: &Path) -> Vec<String> {
    run_git(cwd, &["remote"]).await
}

/// Git operations should not block completions indefinitely.
const GIT_TIMEOUT_MS: u64 = 5_000;

async fn run_git(cwd: &Path, args: &[&str]) -> Vec<String> {
    let output = match tokio::time::timeout(
        std::time::Duration::from_millis(GIT_TIMEOUT_MS),
        Command::new("git")
            .args(args)
            .current_dir(cwd)
            .kill_on_drop(true)
            .output(),
    )
    .await
    {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            tracing::warn!("git command failed: {e}");
            return Vec::new();
        }
        Err(_) => {
            tracing::warn!(args = ?args, "git command timed out after {GIT_TIMEOUT_MS}ms");
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
    use std::process::Command as StdCommand;

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

    #[tokio::test]
    async fn test_git_branch_suggestions_prioritize_current_branch() {
        let tmp = tempfile::TempDir::new().unwrap();
        git_fixture(tmp.path(), &["init"]);
        git_fixture(tmp.path(), &["config", "user.email", "ghost@example.com"]);
        git_fixture(tmp.path(), &["config", "user.name", "Ghost Complete"]);
        git_fixture(tmp.path(), &["branch", "-M", "main"]);
        std::fs::write(tmp.path().join("README.md"), "test\n").unwrap();
        git_fixture(tmp.path(), &["add", "README.md"]);
        git_fixture(tmp.path(), &["commit", "-m", "initial"]);
        git_fixture(tmp.path(), &["branch", "z-current"]);
        git_fixture(tmp.path(), &["checkout", "z-current"]);

        let suggestions = git_suggestions(tmp.path(), GitQueryKind::Branches)
            .await
            .unwrap();

        let texts: Vec<_> = suggestions.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(
            texts.first(),
            Some(&"z-current"),
            "current branch must be first, got {texts:?}"
        );
        let current = suggestions
            .iter()
            .find(|s| s.text == "z-current")
            .expect("current branch should be listed");
        assert_eq!(current.description.as_deref(), Some("current branch"));
        assert_eq!(current.priority.map(|p| p.get()), Some(100));
    }

    fn git_fixture(cwd: &Path, args: &[&str]) {
        let output = StdCommand::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap_or_else(|e| panic!("failed to run git {args:?}: {e}"));
        assert!(
            output.status.success(),
            "git {args:?} failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
