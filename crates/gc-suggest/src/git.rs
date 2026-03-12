use std::path::Path;
use std::process::Command;

use anyhow::Result;

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

pub fn git_suggestions(cwd: &Path, kind: GitQueryKind) -> Result<Vec<Suggestion>> {
    let lines = match kind {
        GitQueryKind::Branches => git_branches(cwd),
        GitQueryKind::Tags => git_tags(cwd),
        GitQueryKind::Remotes => git_remotes(cwd),
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

fn git_branches(cwd: &Path) -> Vec<String> {
    run_git(cwd, &["branch", "--format=%(refname:short)"])
}

fn git_tags(cwd: &Path) -> Vec<String> {
    run_git(cwd, &["tag", "--list"])
}

fn git_remotes(cwd: &Path) -> Vec<String> {
    run_git(cwd, &["remote"])
}

fn run_git(cwd: &Path, args: &[&str]) -> Vec<String> {
    let output = match Command::new("git").args(args).current_dir(cwd).output() {
        Ok(o) => o,
        Err(e) => {
            tracing::debug!("git command failed: {e}");
            return Vec::new();
        }
    };

    if !output.status.success() {
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

    #[test]
    fn test_git_branches_in_non_git_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let branches = git_branches(tmp.path());
        assert!(branches.is_empty());
    }

    #[test]
    fn test_git_suggestions_returns_correct_kind() {
        // Run in workspace root — this is a real git repo
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        if workspace_root.join(".git").exists() {
            let suggestions = git_suggestions(&workspace_root, GitQueryKind::Branches).unwrap();
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
