//! Session module: persistence store plus history-context helpers.
//!
//! Split from the former 533-line `session.rs`:
//!
//! * [`store`] — [`Session`] struct, caps, JSONL persistence.
//! * [`context`] — [`build_history_context`], [`is_anaphoric`], cap re-exports.
//!
//! This module only declares submodules and re-exports their public API,
//! plus the single-source audit helpers it shares (`redact_content` and
//! the JSONL caps live in `audit::redaction` / `audit::rotation`).

pub mod context;
pub mod store;

#[allow(unused_imports)]
pub use context::{build_history_context, is_anaphoric, MAX_CHARS, MAX_MESSAGES};
pub use store::Session;

// Single-source audit helpers, re-exported so existing
// `agent::session::{redact_content, JSONL_*}` paths keep working.
#[allow(unused_imports)]
pub use crate::audit::redaction::redact_content;
#[allow(unused_imports)]
pub use crate::audit::rotation::{JSONL_MAX_BYTES, JSONL_MAX_LINES};
