//! Backwards-compatible re-exports for the pre-split `ui.rs` paths.
//!
//! Slice F split the former 589-line `ui.rs` into focused modules:
//! [`super::commands`] (Command + parse_command + HELP_TEXT) and
//! [`super::draw`] (needs_redraw + draw + render_content_lines + handle_key).
//! This module keeps every existing `tui::ui::` path resolving.

pub use super::commands::{Command, HELP_TEXT, parse_command};
pub use super::draw::{draw, handle_key, needs_redraw};
