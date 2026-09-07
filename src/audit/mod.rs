//! Audit logging: PII redaction, bounded JSONL rotation, sink abstraction.
//!
//! Layout (all leaves are `agent::*`-free, breaking the old
//! `agent::core → audit → agent::session` cycle):
//!
//! * [`redaction`] — pure PII redaction (`redact_content`, `redact_value`).
//! * [`rotation`] — bounded-JSONL caps and truncation (`rotate_file`).
//! * [`file_sink`] — bounded JSONL append used by the file sink.
//! * [`sink`] — [`AuditSink`] trait plus [`FileAuditSink`]/[`NoopAuditSink`].
//!
//! This module only declares submodules and re-exports their public API.

pub mod file_sink;
pub mod redaction;
pub mod rotation;
pub mod sink;

pub use file_sink::{write, AuditEvent};
pub use redaction::{redact_content, redact_value};
pub use rotation::{JSONL_KEEP_LINES, JSONL_MAX_BYTES, JSONL_MAX_LINES};
pub use sink::{AuditSink, FileAuditSink, NoopAuditSink};
