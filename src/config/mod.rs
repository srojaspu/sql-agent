//! Application configuration facade.
//!
//! Slice A of the clean-architecture refactor: the pre-split flat `Config`
//! is now an [`AppConfig`] of focused sub-structs, one per concern:
//!
//! * [`DbConfig`] (`db`) — SQL Server connection (`DATABASE_*`).
//! * [`LlmConfig`] (`llm`) — provider selection and shared knobs (`LLM_*`, `OLLAMA_*`).
//! * [`PolicyConfig`] (`policy`) — allowlist and validator switches.
//! * [`LimitsConfig`] (`limits`) — agent/query/schema budgets.
//! * [`AuditConfig`] (`audit`) — audit-log knobs (`AUDIT_*`).
//!
//! Loading semantics live in [`loader`] and are unchanged from the pre-split
//! `config.rs`: same variable names, defaults, deprecated aliases,
//! validation messages, and drift detection.
//!
//! Backward compatibility: [`Config`] remains the canonical name used by
//! every caller (`crate::config::Config`); it is an alias for [`AppConfig`].

pub mod audit;
pub mod db;
pub mod limits;
pub mod llm;
pub mod loader;
pub mod policy;

pub use audit::AuditConfig;
pub use db::DbConfig;
pub use limits::LimitsConfig;
pub use llm::LlmConfig;
pub use policy::PolicyConfig;

/// Application configuration: nested sub-structs per concern plus the
/// runtime-only `verbose` flag (set from CLI args, never from the env).
#[derive(Clone, Debug)]
pub struct AppConfig {
    /// SQL Server connection settings (`DATABASE_*`).
    pub db: DbConfig,
    /// LLM provider settings (`LLM_*`, `OLLAMA_*`).
    pub llm: LlmConfig,
    /// SQL security-policy knobs (allowlist + validator switches).
    pub policy: PolicyConfig,
    /// Agent and query budget limits.
    pub limits: LimitsConfig,
    /// Audit-log settings (`AUDIT_*`).
    pub audit: AuditConfig,
    /// Verbose output. Runtime-only: set from `--verbose`, never from env.
    pub verbose: bool,
}

/// Canonical config name used crate-wide (pre-split `Config`).
///
/// Alias for [`AppConfig`]: `use crate::config::Config` keeps working, and
/// `Config::from_env` / `Config::from_map` / `Config::find_drifted` resolve
/// to the [`loader`] inherent impls on [`AppConfig`].
pub type Config = AppConfig;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_alias_resolves_to_app_config() {
        fn assert_is_config(_: &Config) {}
        let cfg = AppConfig {
            db: DbConfig {
                host: "localhost".into(),
                port: 1433,
                name: "TestDB".into(),
                user: "user".into(),
                password: "pass".into(),
                trust_cert: false,
            },
            llm: LlmConfig {
                provider: "ollama".into(),
                model: String::new(),
                api_key: String::new(),
                base_url: String::new(),
                ollama_url: "http://127.0.0.1:11434".into(),
                ollama_model: "qwen3:4b".into(),
                timeout_s: 120,
                connect_timeout_s: 5,
                temperature: 0.0,
                max_retries: 3,
            },
            policy: PolicyConfig {
                allowed_tables: vec![],
                block_sensitive_columns: true,
                block_comments: true,
                allow_cte: true,
                allow_system_tables: false,
            },
            limits: LimitsConfig {
                max_steps: 8,
                max_sql_length: 10_000,
                max_rows: 100,
                schema_cache_s: 300,
                query_timeout_s: 30,
                max_concurrent_queries: 1,
                max_joins: 5,
                max_subqueries: 5,
                max_schema_results: 20,
                max_tool_result_chars: 20_000,
                max_tool_calls_per_step: 10,
            },
            audit: AuditConfig {
                enabled: true,
                path: "logs/agent-audit.jsonl".into(),
                capture_sql: false,
            },
            verbose: false,
        };
        assert_is_config(&cfg);
        assert!(!cfg.verbose);
    }
}
