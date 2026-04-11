//! Intermediate popup frame representation.
//!
//! `PopupFrame` captures the visual content of a popup (rows, styled spans,
//! scrollbar indicators) without any ANSI escape sequences. This allows the
//! same popup content logic to be rendered via:
//! - ANSI escape sequences (real proxy popup — existing render.rs path)
//! - ratatui widgets (TUI config editor preview)
//!
//! **Design decision:** This module exists *alongside* render.rs, not as a
//! replacement. render_popup() is 2325 lines with 115 tests and subtle ANSI
//! byte-level state transitions. Rewiring it through a frame model is
//! high-risk for zero user-visible benefit. Instead, frame.rs reuses the
//! same pure helpers (kind_icon, sanitize_display_text,
//! translate_match_indices) and implements parallel content construction.

#[allow(unused_imports)]
use gc_suggest::Suggestion;

/// Abstract style role applied to a text span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanStyle {
    /// Default item text.
    Plain,
    /// Fuzzy-match highlighted character.
    MatchHighlight,
    /// Description text (dim).
    Description,
    /// Gutter icon + padding.
    Gutter,
    /// Border characters.
    Border,
    /// Scrollbar characters.
    Scrollbar,
    /// Loading indicator.
    Loading,
}

/// A run of text sharing the same style within a popup row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledSpan {
    pub text: String,
    pub style: SpanStyle,
}

/// Scrollbar indicator for a content row's right edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollbarCell {
    /// No scrollbar needed (all items fit).
    None,
    /// Scrollbar thumb (filled block).
    Thumb,
    /// Scrollbar track (dotted line).
    Track,
}

/// A content row representing one suggestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentRow {
    pub is_selected: bool,
    pub spans: Vec<StyledSpan>,
    pub scrollbar: ScrollbarCell,
}

/// A single row in the popup frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PopupRow {
    /// Top or bottom border (e.g., "╭──╮" or "╰──╯").
    Border { text: String },
    /// A suggestion content row.
    Content(ContentRow),
    /// Loading indicator ("  ...").
    Loading { spans: Vec<StyledSpan> },
}

/// Complete popup frame ready for rendering. Pure data — no ANSI escapes.
#[derive(Debug, Clone)]
pub struct PopupFrame {
    pub rows: Vec<PopupRow>,
    pub borders: bool,
    pub content_width: u16,
    pub total_width: u16,
}

