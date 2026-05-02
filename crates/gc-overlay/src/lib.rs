//! ANSI-based popup rendering for terminal autocomplete.
//!
//! Renders suggestion popups using cursor save/restore, synchronized output
//! (DECSET 2026), and viewport scrolling to ensure popups always render below
//! the cursor without destroying scrollback content.

pub mod ansi;
pub mod frame;
pub(crate) mod layout;
mod render;
pub mod types;
pub(crate) mod util;

pub use frame::{ContentRow, PopupFrame, PopupRow, ScrollbarCell, SpanStyle, StyledSpan};
pub use render::{
    clear_popup, parse_style, render_indicator_row, render_popup, FeedbackKind, PopupTheme,
};
pub use types::{
    OverlayState, PopupLayout, DEFAULT_MAX_POPUP_WIDTH, DEFAULT_MAX_VISIBLE,
    DEFAULT_MIN_POPUP_WIDTH,
};
