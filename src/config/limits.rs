//! Agent and query budget limits.
//!
//! Sourced from `MAX_*` / `SCHEMA_*` / `QUERY_*` environment variables by
//! [`crate::config::loader`]. Defaults match the pre-split `config.rs`.
//! `MAX_RESULT_CHARS` is accepted-but-ignored (deprecated in favor of
//! `MAX_TOOL_RESULT_CHARS`) and `MAX_AGENT_STEPS` aliases `MAX_STEPS`.

/// Budget limits for the agent loop, SQL surface, and schema tools.
#[derive(Clone, Debug)]
pub struct LimitsConfig {
    /// Maximum agent steps per question.
    ///
    /// Env: `MAX_STEPS` (default `8`); deprecated alias `MAX_AGENT_STEPS`
    /// (warns, `MAX_STEPS` wins when both are set).
    pub max_steps: usize,

    /// Maximum SQL text length accepted by the validator.
    ///
    /// Env: `MAX_SQL_LENGTH` (default `10000`).
    pub max_sql_length: usize,

    /// Maximum rows returned per query.
    ///
    /// Env: `MAX_ROWS` (default `100`).
    pub max_rows: usize,

    /// Schema-cache TTL in seconds.
    ///
    /// Env: `SCHEMA_CACHE_SECONDS` (default `300`).
    pub schema_cache_s: u64,

    /// Per-query timeout in seconds (also the TLS-handshake deadline).
    ///
    /// Env: `QUERY_TIMEOUT_SECONDS` (default `30`).
    pub query_timeout_s: u64,

    /// Maximum concurrent pooled SQL connections.
    ///
    /// Env: `MAX_CONCURRENT_QUERIES` (default `1`).
    pub max_concurrent_queries: usize,

    /// Maximum JOINs accepted per statement.
    ///
    /// Env: `MAX_JOINS` (default `5`).
    pub max_joins: usize,

    /// Maximum subqueries accepted per statement.
    ///
    /// Env: `MAX_SUBQUERIES` (default `5`).
    pub max_subqueries: usize,

    /// Maximum tables returned by schema-ranking tools.
    ///
    /// Env: `MAX_SCHEMA_RESULTS` (default `20`).
    pub max_schema_results: usize,

    /// Maximum characters kept per tool result (output truncation cap).
    ///
    /// Env: `MAX_TOOL_RESULT_CHARS` (default `20000`).
    pub max_tool_result_chars: usize,

    /// Maximum tool calls accepted (and executed) per agent step.
    ///
    /// Env: `MAX_TOOL_CALLS_PER_STEP` (default `10`).
    pub max_tool_calls_per_step: usize,
}
