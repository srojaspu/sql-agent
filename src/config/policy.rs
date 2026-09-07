//! SQL security-policy settings.
//!
//! Sourced from `ALLOWED_*` / `BLOCK_*` / `ALLOW_*` environment variables by
//! [`crate::config::loader`]. These fields feed `SecurityPolicy` verbatim
//! (see `Agent::with_llm`); no validation rule lives here, only the knobs.

/// SQL security-policy knobs (allowlist + validator switches).
#[derive(Clone, Debug)]
pub struct PolicyConfig {
    /// Allowlisted tables as normalized `schema.table` names; empty means
    /// "no allowlist configured".
    ///
    /// Env: `ALLOWED_TABLES` (default `""`).
    ///
    /// Deprecated aliases (warn + map, `ALLOWED_TABLES` wins when set):
    /// `BLOCKED_TABLES`, `BLOCKED_COLUMNS`.
    pub allowed_tables: Vec<String>,

    /// Reject statements referencing sensitive columns (password, token…).
    ///
    /// Env: `BLOCK_SENSITIVE_COLUMNS` (default `true`).
    pub block_sensitive_columns: bool,

    /// Reject statements containing SQL comments.
    ///
    /// Env: `BLOCK_COMMENTS` (default `true`).
    pub block_comments: bool,

    /// Accept Common Table Expressions (`WITH …`).
    ///
    /// Env: `ALLOW_CTE` (default `true`).
    pub allow_cte: bool,

    /// Accept system-table references (`sys.*`).
    ///
    /// Env: `ALLOW_SYSTEM_TABLES` (default `false`).
    pub allow_system_tables: bool,
}
