//! Backwards-compatible re-exports for the pre-split `app.rs` paths.
//!
//! Slice F split the former 669-line `app.rs` into focused modules:
//! [`super::state`] (AppState struct + pure state mutators),
//! [`super::input`] (input/cursor editing) and [`super::events`]
//! (AppEvent + handle_event/handle_command/channel).
//! This module keeps every existing `tui::app::` path resolving.

pub use super::events::AppEvent;
pub use super::state::{AppState, ChatLine};
