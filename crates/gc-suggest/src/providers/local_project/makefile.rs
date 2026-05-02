//! `MakefileTargets` provider — extracts target names from the nearest
//! ancestor `GNUmakefile`/`makefile`/`Makefile`. Hand-rolled parser, no
//! shellout to `make -qp`. The 95% case (a flat target list with
//! optional dependencies) is fully covered. Things we deliberately
//! drop: targets gated by variable expansion (`$(BUILD_DIR)`), pattern
//! rules (`%.o: %.c`), and meta targets (`.PHONY:`, `.SUFFIXES:`, …).
//! Anything we miss falls through to the spec's filesystem fallback at
//! suggestion-merge time.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::Result;

use super::{MtimeCache, MAX_ANCESTOR_WALK};
use crate::providers::{Provider, ProviderCtx};
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

/// GNU make's documented filename precedence — see the GNU make manual
/// §"What Name to Give Your Makefile". The first existing file wins;
/// later files at the same level are ignored even if present.
const MAKEFILE_NAMES: &[&str] = &["GNUmakefile", "makefile", "Makefile"];

static MAKEFILE_CACHE: LazyLock<MtimeCache<Vec<String>>> = LazyLock::new(MtimeCache::new);

/// Extract target names from raw `Makefile` bytes. Returns suggestion
/// strings in source order, deduped on insertion.
///
/// Errors are absorbed: invalid UTF-8 is decoded lossily after a
/// one-time warn; lines we can't classify are silently skipped.
/// Returning an empty `Vec` is the only failure mode the cache layer
/// needs to handle.
pub(crate) fn parse_makefile_targets(bytes: &[u8]) -> Vec<String> {
    let text = match std::str::from_utf8(bytes) {
        Ok(s) => std::borrow::Cow::Borrowed(s),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "makefile: invalid UTF-8 in input; falling back to lossy decode (target names with replacement chars may surface)"
            );
            String::from_utf8_lossy(bytes)
        }
    };
    let mut out: Vec<String> = Vec::new();
    let mut filtered: usize = 0;

    for line in logical_lines(&text) {
        if line.starts_with('\t') || line.starts_with('#') {
            filtered += 1;
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('.') {
            filtered += 1;
            continue;
        }

        // Skip GNU/POSIX assignment forms by peeking past the first
        // colon. The relevant operators are `=`, `:=`, `::=`, `?=`,
        // `+=`, `!=`. The first colon's RHS char tells us:
        //   `=`        → `:=` (simple assignment) — skip.
        //   `:` + `=`  → `::=` (POSIX simple) — skip.
        // `?=`/`+=`/`!=` have no leading colon and never reach this
        // arm. `=` alone (recursive `VAR = value`) bypasses this whole
        // branch since there's no colon.
        let Some(colon_idx) = trimmed.find(':') else {
            continue;
        };
        let mut rest = trimmed[colon_idx + 1..].chars();
        match rest.next() {
            Some('=') => {
                filtered += 1;
                continue;
            }
            Some(':') if rest.next() == Some('=') => {
                filtered += 1;
                continue;
            }
            _ => {}
        }

        let lhs = trimmed[..colon_idx].trim();
        if lhs.is_empty() {
            filtered += 1;
            continue;
        }

        for name in lhs.split_whitespace() {
            if name.is_empty()
                || name.starts_with('.')
                || name.contains("$(")
                || name.contains("${")
                || name.contains('%')
            {
                continue;
            }
            if !out.iter().any(|t| t == name) {
                out.push(name.to_string());
            }
        }
    }

    tracing::debug!(targets = out.len(), filtered, "makefile parse complete");
    out
}

/// Yield logical lines, joining `\<newline>` continuations into a
/// single line. The trailing backslash itself is dropped along with
/// the newline; surrounding whitespace on either side of the join is
/// preserved verbatim — the parser doesn't care.
fn logical_lines(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut continued = false;

    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if continued {
            current.push(' ');
        }
        if let Some(stripped) = line.strip_suffix('\\') {
            current.push_str(stripped);
            continued = true;
        } else {
            current.push_str(line);
            out.push(std::mem::take(&mut current));
            continued = false;
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Walk up to [`MAX_ANCESTOR_WALK`] ancestors of `start` looking for
/// the first directory containing a Makefile under any of the three
/// recognized names. Returns the absolute path on the first match.
pub(crate) fn find_makefile(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);
    for _ in 0..MAX_ANCESTOR_WALK {
        let dir = current?;
        for name in MAKEFILE_NAMES {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        current = dir.parent();
    }
    None
}

/// Provider implementation — the spec routes `{"type":
/// "makefile_targets"}` here via `ProviderKind::MakefileTargets`.
pub struct MakefileTargets;

impl Provider for MakefileTargets {
    fn name(&self) -> &'static str {
        "makefile_targets"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        Self::generate_with_root(&ctx.cwd).await
    }
}

impl MakefileTargets {
    /// Test seam — production calls this with `&ctx.cwd`; tests inject
    /// a tempdir path so the ancestor walk is bounded by the test
    /// fixture rather than escaping into the developer's filesystem.
    pub(crate) async fn generate_with_root(root: &Path) -> Result<Vec<Suggestion>> {
        let Some(path) = find_makefile(root) else {
            return Ok(Vec::new());
        };
        let Some(targets) = MAKEFILE_CACHE.get_or_insert_with(&path, parse_makefile_targets) else {
            return Ok(Vec::new());
        };
        Ok(targets
            .into_iter()
            .map(|name| Suggestion {
                text: name,
                description: Some("Make target".to_string()),
                kind: SuggestionKind::ProviderValue,
                source: SuggestionSource::Provider,
                ..Default::default()
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn happy_path_three_targets() {
        let src = b"build:\n\tcc -o build\n\ntest:\n\tcargo test\n\nlint:\n\tclippy\n";
        let targets = parse_makefile_targets(src);
        assert_eq!(targets, vec!["build", "test", "lint"]);
    }

    #[test]
    fn backslash_continuation_joins() {
        // The split prerequisite list spans two lines; we don't care
        // about the deps, only that the LHS still resolves to `all`.
        let src = b"all: \\\n\tdep_one dep_two\n\tcc -o all\n";
        assert_eq!(parse_makefile_targets(src), vec!["all"]);
    }

    #[test]
    fn multi_target_rule_emits_each_target() {
        let src = b"build test bench: deps\n\techo go\n";
        assert_eq!(parse_makefile_targets(src), vec!["build", "test", "bench"]);
    }

    #[test]
    fn phony_meta_targets_filtered_out() {
        let src = b".PHONY: install clean\ninstall:\n\tcp\n";
        assert_eq!(parse_makefile_targets(src), vec!["install"]);
    }

    #[test]
    fn pattern_rules_filtered_out() {
        let src = b"%.o: %.c\n\tcc -c $<\nbuild:\n\tcc\n";
        assert_eq!(parse_makefile_targets(src), vec!["build"]);
    }

    #[test]
    fn variable_expanded_target_filtered_out() {
        let src = b"clean: $(BUILD_DIR)\n\trm -rf\nbuild:\n\tcc\n";
        assert_eq!(parse_makefile_targets(src), vec!["clean", "build"]);
    }

    #[test]
    fn computed_lhs_target_filtered_out() {
        let src = b"$(BUILD_DIR):\n\tmkdir -p $(BUILD_DIR)\nbuild:\n\tcc\n";
        assert_eq!(parse_makefile_targets(src), vec!["build"]);
    }

    #[test]
    fn recipe_lines_ignored() {
        // Tab-prefixed lines are recipe bodies and never targets, even
        // if they contain a colon (e.g. `\t echo a:b`).
        let src = b"build:\n\techo build:bytes\n";
        assert_eq!(parse_makefile_targets(src), vec!["build"]);
    }

    #[test]
    fn comment_lines_ignored() {
        let src = b"# A header comment\n# build: not_a_target\nbuild:\n\tcc\n";
        assert_eq!(parse_makefile_targets(src), vec!["build"]);
    }

    #[test]
    fn empty_input_returns_empty() {
        assert!(parse_makefile_targets(b"").is_empty());
        assert!(parse_makefile_targets(b"\n\n\n").is_empty());
    }

    #[test]
    fn variable_assignment_skipped() {
        // `:=` and `?=` are assignments, not target rules.
        let src = b"CC := gcc\nFLAGS ?= -O2\nbuild:\n\t$(CC)\n";
        assert_eq!(parse_makefile_targets(src), vec!["build"]);
    }

    #[test]
    fn posix_double_colon_assignment_skipped() {
        // POSIX `::=` (simple assignment) must not be misclassified as
        // a target named `CC`.
        let src = b"CC ::= gcc\nbuild:\n\t$(CC)\n";
        assert_eq!(parse_makefile_targets(src), vec!["build"]);
    }

    #[test]
    fn crlf_line_endings_are_handled() {
        let src = b"build:\r\n\tcc\r\ntest:\r\n\tcargo test\r\n";
        assert_eq!(parse_makefile_targets(src), vec!["build", "test"]);
    }

    #[test]
    fn crlf_with_backslash_continuation_joins() {
        let src = b"all: \\\r\n\tdep\r\n\tcc -o all\r\n";
        assert_eq!(parse_makefile_targets(src), vec!["all"]);
    }

    #[test]
    fn dedup_preserves_first_occurrence_order() {
        let src = b"build:\n\techo\nbuild:\n\techo again\ntest:\n\techo\n";
        assert_eq!(parse_makefile_targets(src), vec!["build", "test"]);
    }

    #[test]
    fn invalid_utf8_handled_via_lossy_decode() {
        // 0xFF is invalid UTF-8 anywhere; lossy decode replaces it with
        // U+FFFD. The parser must still extract `build`.
        let src: Vec<u8> = b"build:\n\techo \xFF\n".to_vec();
        assert_eq!(parse_makefile_targets(&src), vec!["build"]);
    }

    #[tokio::test]
    async fn generate_with_root_against_empty_dir_returns_ok_empty() {
        let tmp = TempDir::new().unwrap();
        let result = MakefileTargets::generate_with_root(tmp.path()).await;
        assert!(matches!(result, Ok(ref v) if v.is_empty()));
    }

    #[tokio::test]
    async fn generate_with_root_finds_makefile_and_returns_suggestions() {
        let tmp = TempDir::new().unwrap();
        let mf = tmp.path().join("Makefile");
        std::fs::File::create(&mf)
            .unwrap()
            .write_all(b"build:\n\tcc\ntest:\n\tcargo test\n")
            .unwrap();
        let suggestions = MakefileTargets::generate_with_root(tmp.path())
            .await
            .unwrap();
        let texts: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["build", "test"]);
        assert_eq!(suggestions[0].kind, SuggestionKind::ProviderValue);
        assert_eq!(suggestions[0].source, SuggestionSource::Provider);
        assert_eq!(suggestions[0].description.as_deref(), Some("Make target"));
    }

    #[tokio::test]
    async fn gnu_makefile_takes_precedence_over_makefile() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("GNUmakefile"), b"gnu_only:\n\tcc\n").unwrap();
        std::fs::write(tmp.path().join("Makefile"), b"plain_only:\n\tcc\n").unwrap();
        let suggestions = MakefileTargets::generate_with_root(tmp.path())
            .await
            .unwrap();
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].text, "gnu_only");
    }

    #[tokio::test]
    async fn ancestor_walk_finds_makefile_in_parent() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("Makefile"), b"top:\n\tcc\n").unwrap();
        let nested = tmp.path().join("a").join("b").join("c");
        std::fs::create_dir_all(&nested).unwrap();
        let suggestions = MakefileTargets::generate_with_root(&nested).await.unwrap();
        assert_eq!(suggestions[0].text, "top");
    }
}
