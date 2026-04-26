//! Per-suggestion effective priority and the `Priority` newtype.
//!
//! Spec authors override per-item via the Fig `priority` JSON field
//! (range 0..=100, higher = better). When unset, the kind's base value
//! (`SuggestionKind::base_priority`) is used so the default ordering
//! still surfaces domain content above flags above filesystem.

use serde::{Deserialize, Deserializer, Serialize};

use crate::types::Suggestion;

/// Validated rank value in the documented range 0..=100. Constructed via
/// the clamping `Priority::new`; values above 100 are clamped down so the
/// type cannot represent an out-of-range priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Priority(u8);

impl Priority {
    pub const fn new(v: u8) -> Self {
        Self(if v > 100 { 100 } else { v })
    }

    pub const fn get(self) -> u8 {
        self.0
    }
}

impl<'de> Deserialize<'de> for Priority {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = u8::deserialize(deserializer)?;
        Ok(Priority::new(raw))
    }
}

/// Effective priority for a suggestion: spec override if present, else
/// the kind base.
pub fn effective(s: &Suggestion) -> Priority {
    s.priority.unwrap_or_else(|| s.kind.base_priority())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SuggestionKind;

    #[test]
    fn base_priorities_are_in_documented_order() {
        // Full chain top-to-bottom plus the documented three-way tie.
        assert!(SuggestionKind::GitBranch.base_priority() > SuggestionKind::GitTag.base_priority());
        assert!(SuggestionKind::GitTag.base_priority() > SuggestionKind::GitRemote.base_priority());
        assert_eq!(
            SuggestionKind::GitRemote.base_priority(),
            SuggestionKind::Subcommand.base_priority()
        );
        assert_eq!(
            SuggestionKind::Subcommand.base_priority(),
            SuggestionKind::ProviderValue.base_priority()
        );
        assert!(
            SuggestionKind::ProviderValue.base_priority() > SuggestionKind::EnvVar.base_priority()
        );
        assert!(SuggestionKind::EnvVar.base_priority() > SuggestionKind::Command.base_priority());
        assert!(SuggestionKind::Command.base_priority() > SuggestionKind::Flag.base_priority());
        assert!(SuggestionKind::Flag.base_priority() > SuggestionKind::Directory.base_priority());
        assert!(
            SuggestionKind::Directory.base_priority() > SuggestionKind::FilePath.base_priority()
        );
        assert!(SuggestionKind::FilePath.base_priority() > SuggestionKind::History.base_priority());
    }

    #[test]
    fn effective_uses_override_when_present() {
        let s = Suggestion {
            kind: SuggestionKind::Flag,
            priority: Some(Priority::new(99)),
            ..Default::default()
        };
        assert_eq!(effective(&s).get(), 99);
    }

    #[test]
    fn effective_falls_back_to_base() {
        let s = Suggestion {
            kind: SuggestionKind::GitBranch,
            priority: None,
            ..Default::default()
        };
        assert_eq!(effective(&s).get(), 80);
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
            let p = k.base_priority().get();
            assert!(p <= 100, "{k:?} base priority {p} out of range");
        }
    }

    #[test]
    fn priority_new_clamps_values_above_100() {
        assert_eq!(Priority::new(101).get(), 100);
        assert_eq!(Priority::new(255).get(), 100);
        assert_eq!(Priority::new(100).get(), 100);
        assert_eq!(Priority::new(50).get(), 50);
        assert_eq!(Priority::new(0).get(), 0);
    }

    #[test]
    fn priority_deserialize_clamps_out_of_range() {
        let p: Priority = serde_json::from_str("200").unwrap();
        assert_eq!(p.get(), 100);
        let p: Priority = serde_json::from_str("75").unwrap();
        assert_eq!(p.get(), 75);
    }
}
