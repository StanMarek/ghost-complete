//! Kind-derived base priorities and per-suggestion effective priority.
//!
//! Spec authors override per-item via the Fig `priority` JSON field
//! (range 0..=100, higher = better). When unset, the kind's base value
//! is used so the default ordering still surfaces domain content above
//! flags above filesystem.

use crate::types::{Suggestion, SuggestionKind};

/// Base priority for a `SuggestionKind` when the suggestion does not
/// declare its own. Numbers chosen so that branches > generator output
/// > flags > filesystem, with comfortable headroom for spec overrides.
pub fn base_for_kind(kind: SuggestionKind) -> u8 {
    match kind {
        SuggestionKind::GitBranch => 80,
        SuggestionKind::GitTag => 75,
        SuggestionKind::GitRemote => 70,
        SuggestionKind::Subcommand => 70,
        SuggestionKind::ProviderValue => 70,
        SuggestionKind::EnvVar => 50,
        SuggestionKind::Command => 40,
        SuggestionKind::Flag => 30,
        SuggestionKind::Directory => 25,
        SuggestionKind::FilePath => 20,
        SuggestionKind::History => 10,
    }
}

/// Effective priority for a suggestion: spec override if present, else
/// the kind base.
pub fn effective(s: &Suggestion) -> u8 {
    s.priority.unwrap_or_else(|| base_for_kind(s.kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_priorities_are_in_documented_order() {
        // Full chain top-to-bottom plus the documented three-way tie.
        assert!(base_for_kind(SuggestionKind::GitBranch) > base_for_kind(SuggestionKind::GitTag));
        assert!(base_for_kind(SuggestionKind::GitTag) > base_for_kind(SuggestionKind::GitRemote));
        assert_eq!(
            base_for_kind(SuggestionKind::GitRemote),
            base_for_kind(SuggestionKind::Subcommand)
        );
        assert_eq!(
            base_for_kind(SuggestionKind::Subcommand),
            base_for_kind(SuggestionKind::ProviderValue)
        );
        assert!(
            base_for_kind(SuggestionKind::ProviderValue) > base_for_kind(SuggestionKind::EnvVar)
        );
        assert!(base_for_kind(SuggestionKind::EnvVar) > base_for_kind(SuggestionKind::Command));
        assert!(base_for_kind(SuggestionKind::Command) > base_for_kind(SuggestionKind::Flag));
        assert!(base_for_kind(SuggestionKind::Flag) > base_for_kind(SuggestionKind::Directory));
        assert!(base_for_kind(SuggestionKind::Directory) > base_for_kind(SuggestionKind::FilePath));
        assert!(base_for_kind(SuggestionKind::FilePath) > base_for_kind(SuggestionKind::History));
    }

    #[test]
    fn effective_uses_override_when_present() {
        let s = Suggestion {
            kind: SuggestionKind::Flag,
            priority: Some(99),
            ..Default::default()
        };
        assert_eq!(effective(&s), 99);
    }

    #[test]
    fn effective_falls_back_to_base() {
        let s = Suggestion {
            kind: SuggestionKind::GitBranch,
            priority: None,
            ..Default::default()
        };
        assert_eq!(effective(&s), 80);
    }

    #[test]
    fn base_priorities_are_within_fig_range() {
        for k in [
            SuggestionKind::GitBranch,
            SuggestionKind::GitTag,
            SuggestionKind::GitRemote,
            SuggestionKind::Subcommand,
            SuggestionKind::ProviderValue,
            SuggestionKind::EnvVar,
            SuggestionKind::Command,
            SuggestionKind::Flag,
            SuggestionKind::Directory,
            SuggestionKind::FilePath,
            SuggestionKind::History,
        ] {
            let p = base_for_kind(k);
            assert!(p <= 100, "{k:?} base priority {p} out of range");
        }
    }

    /// Pin the documented kind-base values for `Subcommand` and `Flag`.
    /// `tools/spec-priority-audit/apply.mjs` hard-codes these as
    /// `SUBCOMMAND_KIND_BASE = 70` and `FLAG_KIND_BASE = 30` so that it can
    /// skip writing values equal to the kind base (a no-op for ranking).
    /// If anyone changes either constant in this crate without updating the
    /// Node script, the audit tool would silently emit redundant
    /// `priority: 70`/`priority: 30` entries on every spec it touches —
    /// noisy diffs and a corpus that no longer round-trips through
    /// `apply.mjs`. This is a runtime `assert_eq!` inside `#[test]`, so it
    /// forces a deliberate cross-language update by failing the test suite
    /// when the bases drift.
    #[test]
    fn subcommand_and_flag_bases_match_audit_tool_constants() {
        assert_eq!(
            base_for_kind(SuggestionKind::Subcommand),
            70,
            "if you change this, update SUBCOMMAND_KIND_BASE in \
             tools/spec-priority-audit/apply.mjs"
        );
        assert_eq!(
            base_for_kind(SuggestionKind::Flag),
            30,
            "if you change this, update FLAG_KIND_BASE in \
             tools/spec-priority-audit/apply.mjs"
        );
    }
}
