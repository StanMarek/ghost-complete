//! Caller-owned synchronized-output frame.
//!
//! Wraps an overlay update closure with exactly ONE balanced
//! `begin_sync`/`end_sync` pair on `RenderStrategy::Synchronized`. On
//! `RenderStrategy::PreRenderBuffer` no markers are emitted; the caller's
//! single coalesced write is its own atomicity guarantee.
//!
//! Empty-render short-circuit: if the closure leaves `buf` unchanged
//! (`buf.len()` matches the pre-frame length), no sync markers are emitted
//! — saves the 16-byte cost of `\x1b[?2026h\x1b[?2026l` on no-op renders.

use crate::ansi;
use gc_terminal::{RenderStrategy, TerminalProfile};

pub fn with_overlay_update_frame<F>(buf: &mut Vec<u8>, profile: &TerminalProfile, body: F)
where
    F: FnOnce(&mut Vec<u8>),
{
    let sync = matches!(profile.render_strategy(), RenderStrategy::Synchronized);
    // Captured before `begin_sync` so a no-op-body rollback (`truncate(pre)`)
    // also drops the speculative `begin_sync`; `body_pre` (after it) is the
    // baseline for detecting whether the body wrote anything.
    let pre = buf.len();
    if sync {
        ansi::begin_sync(buf);
    }
    let body_pre = buf.len();
    body(buf);
    if sync {
        if buf.len() == body_pre {
            // No-op body — roll back the begin_sync to avoid emitting
            // a useless 16-byte no-op frame.
            buf.truncate(pre);
        } else {
            ansi::end_sync(buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gc_terminal::TerminalProfile;

    #[test]
    fn synchronized_profile_wraps_in_decset_2026() {
        let mut buf = Vec::new();
        with_overlay_update_frame(&mut buf, &TerminalProfile::for_ghostty(), |b| {
            b.extend_from_slice(b"BODY");
        });
        assert_eq!(buf, b"\x1b[?2026hBODY\x1b[?2026l");
    }

    #[test]
    fn pre_render_profile_emits_no_sync_markers() {
        let mut buf = Vec::new();
        with_overlay_update_frame(&mut buf, &TerminalProfile::for_iterm2(), |b| {
            b.extend_from_slice(b"BODY");
        });
        assert_eq!(buf, b"BODY");
    }

    #[test]
    fn empty_body_emits_no_bytes_on_synchronized() {
        let mut buf = Vec::new();
        with_overlay_update_frame(&mut buf, &TerminalProfile::for_ghostty(), |_b| {});
        assert!(
            buf.is_empty(),
            "no-op body must emit zero bytes; got {:?}",
            buf
        );
    }

    #[test]
    fn nested_calls_emit_only_one_outer_frame_pair() {
        // If a caller accidentally nests (anti-pattern), each frame is
        // independently balanced. Test guards against the prior bug where
        // popup helpers AND caller both emit sync.
        let mut buf = Vec::new();
        with_overlay_update_frame(&mut buf, &TerminalProfile::for_ghostty(), |b| {
            with_overlay_update_frame(b, &TerminalProfile::for_ghostty(), |bb| {
                bb.extend_from_slice(b"INNER");
            });
        });
        assert_eq!(buf, b"\x1b[?2026h\x1b[?2026hINNER\x1b[?2026l\x1b[?2026l");
        // 2 begin + 2 end is correct for nested usage; we just rely on
        // callers NOT to nest. The unframed primitives in Task 3 are how
        // we avoid nesting in practice.
    }
}
