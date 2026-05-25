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

/// Wrap an overlay update closure in exactly one balanced DECSET 2026 frame.
///
/// Use this to own the outer synchronized-output frame for one overlay tick —
/// the whole popup + detail box update lands inside a single
/// `begin_sync`/`end_sync` pair on `RenderStrategy::Synchronized` terminals,
/// preventing the user from seeing partial paints. On
/// `RenderStrategy::PreRenderBuffer` no markers are emitted; the caller's
/// single coalesced `write()` is the atomicity guarantee.
///
/// # Invocation contract
///
/// `body` is called exactly once. `FnOnce` guarantees AT MOST one call; this
/// function additionally guarantees AT LEAST one — callers (e.g.
/// `render_popup`) rely on this to populate `Option<PopupLayout>` captures
/// inside the closure. Any future refactor that conditionally skips `body`
/// must update those callers.
///
/// # No-op short-circuit
///
/// If `body` leaves `buf` unchanged (its post-call length equals the
/// pre-`begin_sync` length), the speculatively-written `begin_sync` is rolled
/// back so the call emits zero bytes — saves the 16-byte cost of
/// `\x1b[?2026h\x1b[?2026l` on no-op renders.
///
/// # Do NOT nest
///
/// Calling `with_overlay_update_frame` from inside another
/// `with_overlay_update_frame` body is an anti-pattern: each level emits its
/// own balanced pair, producing nested markers (see
/// `nested_calls_emit_independent_frame_pairs` for the exact bytes). To fold
/// multiple overlay operations into ONE frame, call the `*_unframed`
/// primitives — `render_popup_unframed`, `clear_popup_unframed`,
/// `render_detail_box`, `clear_detail_box` — inside the body instead of their
/// framed wrappers (`render_popup`, `clear_popup`).
pub fn with_overlay_update_frame<F>(buf: &mut Vec<u8>, profile: &TerminalProfile, body: F)
where
    F: FnOnce(&mut Vec<u8>),
{
    let sync = matches!(profile.render_strategy(), RenderStrategy::Synchronized);
    // On Synchronized profiles, `pre` snapshots the buffer length BEFORE the
    // speculative `begin_sync` so a no-op-body rollback (`truncate(pre)`)
    // drops both the body bytes and the speculative marker. `body_pre` (after
    // the marker) is the baseline for detecting whether the body wrote
    // anything. On PreRenderBuffer profiles no marker is emitted, so
    // `pre == body_pre` and the rollback branch never runs.
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
    fn nested_calls_emit_independent_frame_pairs() {
        // Nesting is an anti-pattern: each `with_overlay_update_frame` level
        // emits its own balanced `begin_sync`/`end_sync` pair without any
        // deduplication. Callers must avoid nesting by using the `*_unframed`
        // primitives inside the outer frame body. This test pins the current
        // behavior so a regression that breaks the balance — or, conversely,
        // a refactor that secretly starts deduping — is caught.
        let mut buf = Vec::new();
        with_overlay_update_frame(&mut buf, &TerminalProfile::for_ghostty(), |b| {
            with_overlay_update_frame(b, &TerminalProfile::for_ghostty(), |bb| {
                bb.extend_from_slice(b"INNER");
            });
        });
        assert_eq!(buf, b"\x1b[?2026h\x1b[?2026hINNER\x1b[?2026l\x1b[?2026l");
    }
}
