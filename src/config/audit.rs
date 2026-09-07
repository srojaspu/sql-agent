//! Audit-log settings.
//!
//! Sourced from `AUDIT_*` environment variables by
//! [`crate::config::loader`]. Defaults match the pre-split `config.rs`.

/// Audit-log knobs (`AUDIT_*`).
#[derive(Clone, Debug)]
pub struct AuditConfig {
    /// Whether agent events are appended to the audit log.
    ///
    /// Env: `AUDIT_ENABLED` (default `true`).
    pub enabled: bool,

    /// JSONL audit-log path (parent directories are created on write).
    ///
    /// Env: `AUDIT_PATH` (default `"logs/agent-audit.jsonl"`).
    pub path: String,

    /// Whether SQL text is included in audit payloads.
    ///
    /// Env: `AUDIT_SQL` (default `false`).
    ///
    /// Privacy note: defaults to `false` so query text that may contain
    /// PII is never persisted unless explicitly opted in. (Pre-split field
    /// name: `audit_sql`.)
    pub capture_sql: bool,
}
