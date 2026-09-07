//! Typed error hierarchy for the sql-agent clean-architecture refactor.
//!
//! Slice A introduces the error types only: no function signatures change
//! yet, so every layer keeps returning `anyhow::Result` for now. Later
//! slices will migrate fallible APIs to these types one layer at a time.
//!
//! Layout mirrors the layering:
//!
//! * [`ConfigError`] — environment / map loading (`config::loader`).
//! * [`ValidationBlocked`] — security validator rejections (`security`).
//! * [`DbError`] — SQL Server connectivity and permission gates (`database`).
//! * [`LlmError`] — provider HTTP failures and malformed replies (`llm`).
//! * [`AuditError`] — audit-log persistence (`audit`).
//! * [`AgentError`] — agent-facing umbrella; every layer converts into it
//!   via `#[from]`, so `?` keeps working once signatures migrate.
//!
//! Display strings are new API (no existing error text was altered to add
//! them); they stay short because callers wrap them with context.

use thiserror::Error;

/// Failures while loading configuration from the environment or a map.
///
/// Covers the loader's three failure modes plus provider-level config
/// misuse (vendor keys without a matching `LLM_PROVIDER`).
#[derive(Debug, Error)]
pub enum ConfigError {
    /// A required variable is absent.
    ///
    /// Carries the variable name (e.g. `"DATABASE_HOST"`).
    #[error("missing required config: {0}")]
    Missing(String),

    /// A variable is present but cannot be parsed or is out of range.
    ///
    /// Carries `(variable, reason)`; the reason preserves today's loader
    /// messages verbatim (e.g. `"OLLAMA_TEMPERATURE inválido"`).
    #[error("invalid config {0}: {1}")]
    Invalid(String, String),

    /// A variable looks like config (`PREFIX_`-style) but is not known.
    ///
    /// Carries the offending key (e.g. `"ALLOWED_TYPO_XYZ"`).
    #[error("unknown config var: {0}")]
    Unknown(String),

    /// Provider-level misuse, such as a vendor-specific key (`OPENAI_API_KEY`)
    /// without `LLM_PROVIDER`.
    ///
    /// Carries a human-readable explanation; today's text
    /// (`"{key} ya no es compatible; use LLM_API_KEY con LLM_PROVIDER"`)
    /// is preserved by the loader and flows through here on migration.
    #[error("provider config error: {0}")]
    Provider(String),
}

/// The security validator refused a statement.
///
/// The payload is the validator's existing rejection message, unchanged:
/// allowlist misses, sensitive-column hits, comment/CTE/system-table policy
/// violations, join/subquery budget overflows, and read-only gate denials.
#[derive(Debug, Error)]
pub enum ValidationBlocked {
    /// Carries the validator's rejection message verbatim.
    #[error("validation blocked: {0}")]
    Blocked(String),
}

/// SQL Server connectivity, timeout, and permission-gate failures.
#[derive(Debug, Error)]
pub enum DbError {
    /// A connect, TLS-handshake, or query deadline expired.
    ///
    /// Carries what timed out (e.g. `"TCP SQL Server"`, `"query"`).
    #[error("database timeout: {0}")]
    Timeout(String),

    /// The login failed the read-only gate: write/admin grants, privileged
    /// roles (`sysadmin`, `db_owner`, `CONTROL SERVER`), or an
    /// incomplete permission probe (fail-closed).
    ///
    /// Carries the gate's denial message verbatim.
    #[error("database is not read-only: {0}")]
    ReadOnly(String),

    /// Transport or pool failure: TCP refused, TLS error, pool exhaustion.
    ///
    /// Carries the underlying failure message.
    #[error("database transport error: {0}")]
    Transport(String),
}

/// LLM provider failures: HTTP errors, timeouts, and malformed replies.
#[derive(Debug, Error)]
pub enum LlmError {
    /// The provider returned an HTTP error or an unusable body.
    ///
    /// Carries `(provider, detail)` so multi-provider logs stay greppable
    /// (e.g. `("OpenAI", "devolvió HTTP 401")`).
    #[error("llm provider {0} error: {1}")]
    Provider(String, String),

    /// The provider did not answer within its configured deadline.
    ///
    /// Carries the timeout in seconds (`llm.timeout_s`).
    #[error("llm request timed out after {0}s")]
    Timeout(u64),

    /// The provider answered 200 OK with a body we cannot use: invalid
    /// JSON, missing `choices[0].message` / `content` / `parts`, or an
    /// unfinished Ollama response.
    ///
    /// Carries the parse failure detail.
    #[error("llm bad response: {0}")]
    BadResponse(String),
}

/// Audit-log persistence failures.
///
/// Audit writes are best-effort bounded JSONL appends; when they fail the
/// agent surfaces the I/O detail instead of silently dropping the event.
#[derive(Debug, Error)]
pub enum AuditError {
    /// Carries the underlying I/O failure message.
    #[error("audit write failed: {0}")]
    Io(String),
}

/// Agent-facing umbrella error.
///
/// Each layer converts into its variant via `#[from]`, so migrating a
/// signature to `Result<_, AgentError>` keeps every existing `?` working.
/// [`AgentError::Budget`] is the only agent-born variant: step/tool-call
/// budget overflows detected by the agent loop itself.
#[derive(Debug, Error)]
pub enum AgentError {
    /// Configuration loading failed.
    #[error(transparent)]
    Config(#[from] ConfigError),

    /// The security validator rejected a statement.
    #[error(transparent)]
    Validation(#[from] ValidationBlocked),

    /// The database layer failed.
    #[error(transparent)]
    Db(#[from] DbError),

    /// The LLM provider failed.
    #[error(transparent)]
    Llm(#[from] LlmError),

    /// The audit log could not be persisted.
    #[error(transparent)]
    Audit(#[from] AuditError),

    /// The agent exceeded a budget (steps, tool calls per step).
    ///
    /// Carries a human-readable explanation (e.g.
    /// `"Demasiadas herramientas en un mismo paso"`).
    #[error("budget exceeded: {0}")]
    Budget(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_missing_displays_key() {
        let err = ConfigError::Missing("DATABASE_HOST".into());
        assert_eq!(err.to_string(), "missing required config: DATABASE_HOST");
    }

    #[test]
    fn config_invalid_carries_key_and_reason() {
        let err = ConfigError::Invalid("MAX_STEPS".into(), "out of range".into());
        assert_eq!(err.to_string(), "invalid config MAX_STEPS: out of range");
    }

    #[test]
    fn validation_blocked_preserves_message() {
        let err = ValidationBlocked::Blocked("tabla no autorizada".into());
        assert_eq!(err.to_string(), "validation blocked: tabla no autorizada");
    }

    #[test]
    fn llm_variants_display() {
        assert_eq!(
            LlmError::Provider("OpenAI".into(), "devolvió HTTP 401".into()).to_string(),
            "llm provider OpenAI error: devolvió HTTP 401"
        );
        assert_eq!(
            LlmError::Timeout(120).to_string(),
            "llm request timed out after 120s"
        );
        assert_eq!(
            LlmError::BadResponse("JSON inválido".into()).to_string(),
            "llm bad response: JSON inválido"
        );
    }

    #[test]
    fn db_and_audit_variants_display() {
        assert_eq!(
            DbError::Timeout("query".into()).to_string(),
            "database timeout: query"
        );
        assert_eq!(
            DbError::ReadOnly("sysadmin".into()).to_string(),
            "database is not read-only: sysadmin"
        );
        assert_eq!(
            DbError::Transport("refused".into()).to_string(),
            "database transport error: refused"
        );
        assert_eq!(
            AuditError::Io("disk full".into()).to_string(),
            "audit write failed: disk full"
        );
    }

    #[test]
    fn agent_error_from_conversions_are_transparent() {
        let err = AgentError::from(ConfigError::Unknown("ALLOWED_TYPO_XYZ".into()));
        assert_eq!(err.to_string(), "unknown config var: ALLOWED_TYPO_XYZ");
        let err = AgentError::from(ValidationBlocked::Blocked("nope".into()));
        assert_eq!(err.to_string(), "validation blocked: nope");
        let err = AgentError::from(DbError::Timeout("TCP".into()));
        assert_eq!(err.to_string(), "database timeout: TCP");
        let err = AgentError::from(LlmError::Timeout(5));
        assert_eq!(err.to_string(), "llm request timed out after 5s");
        let err = AgentError::from(AuditError::Io("e".into()));
        assert_eq!(err.to_string(), "audit write failed: e");
        let err = AgentError::Budget("too many steps".into());
        assert_eq!(err.to_string(), "budget exceeded: too many steps");
    }
}
