use anyhow::{Context, Result};

#[derive(Clone, Debug)]
pub struct Config {
    pub database_host: String,
    pub database_port: u16,
    pub database_name: String,
    pub database_user: String,
    pub database_password: String,
    pub database_trust_cert: bool,

    pub ollama_url: String,
    pub ollama_model: String,
    pub ollama_timeout_seconds: u64,
    pub ollama_connect_timeout_seconds: u64,
    pub ollama_temperature: f32,

    pub max_steps: usize,
    pub max_sql_length: usize,
    pub max_rows: usize,
    pub max_result_chars: usize,
    pub schema_cache_seconds: u64,
    pub query_timeout_seconds: u64,
    pub max_concurrent_queries: usize,
    pub max_joins: usize,
    pub max_subqueries: usize,
    pub max_schema_results: usize,
    pub max_tool_result_chars: usize,
    pub max_tool_calls_per_step: usize,

    pub allowed_tables: Vec<String>,
    pub block_sensitive_columns: bool,
    pub block_comments: bool,
    pub allow_cte: bool,
    pub allow_system_tables: bool,

    pub audit_enabled: bool,
    pub audit_path: String,
    pub audit_sql: bool,
    pub verbose: bool,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let allowed_tables = env_default("ALLOWED_TABLES", "")
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| normalize_table(s))
            .collect();

        Ok(Self {
            database_host: env("DATABASE_HOST")?,
            database_port: parse_u16("DATABASE_PORT", 1433)?,
            database_name: env("DATABASE_NAME")?,
            database_user: env("DATABASE_USER")?,
            database_password: env("DATABASE_PASSWORD")?,
            database_trust_cert: parse_bool("DATABASE_TRUST_CERT", true)?,

            ollama_url: env_default("OLLAMA_URL", "http://127.0.0.1:11434"),
            ollama_model: env_default("OLLAMA_MODEL", "qwen3:4b"),
            ollama_timeout_seconds: parse_u64("OLLAMA_TIMEOUT_SECONDS", 120)?,
            ollama_connect_timeout_seconds: parse_u64("OLLAMA_CONNECT_TIMEOUT_SECONDS", 5)?,
            ollama_temperature: env_default("OLLAMA_TEMPERATURE", "0.0")
                .parse()
                .context("OLLAMA_TEMPERATURE inválido")?,

            max_steps: parse_usize("MAX_STEPS", 8)?,
            max_sql_length: parse_usize("MAX_SQL_LENGTH", 10000)?,
            max_rows: parse_usize("MAX_ROWS", 100)?,
            max_result_chars: parse_usize("MAX_RESULT_CHARS", 30000)?,
            schema_cache_seconds: parse_u64("SCHEMA_CACHE_SECONDS", 300)?,
            query_timeout_seconds: parse_u64("QUERY_TIMEOUT_SECONDS", 30)?,
            max_concurrent_queries: parse_usize("MAX_CONCURRENT_QUERIES", 1)?,
            max_joins: parse_usize("MAX_JOINS", 5)?,
            max_subqueries: parse_usize("MAX_SUBQUERIES", 5)?,
            max_schema_results: parse_usize("MAX_SCHEMA_RESULTS", 20)?,
            max_tool_result_chars: parse_usize("MAX_TOOL_RESULT_CHARS", 20000)?,
            max_tool_calls_per_step: parse_usize("MAX_TOOL_CALLS_PER_STEP", 10)?,

            allowed_tables,
            block_sensitive_columns: parse_bool("BLOCK_SENSITIVE_COLUMNS", true)?,
            block_comments: parse_bool("BLOCK_COMMENTS", true)?,
            allow_cte: parse_bool("ALLOW_CTE", true)?,
            allow_system_tables: parse_bool("ALLOW_SYSTEM_TABLES", false)?,

            audit_enabled: parse_bool("AUDIT_ENABLED", true)?,
            audit_path: env_default("AUDIT_PATH", "logs/agent-audit.jsonl"),
            audit_sql: parse_bool("AUDIT_SQL", true)?,
            verbose: false,
        })
    }
}

fn env(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("Falta variable {key}"))
}
fn env_default(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}
fn parse_bool(key: &str, default: bool) -> Result<bool> {
    match std::env::var(key) {
        Ok(v) => match v.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Ok(true),
            "false" | "0" | "no" => Ok(false),
            _ => anyhow::bail!("{key} debe ser true/false"),
        },
        Err(_) => Ok(default),
    }
}
fn parse_u64(key: &str, default: u64) -> Result<u64> {
    Ok(env_default(key, &default.to_string()).parse()?)
}
fn parse_usize(key: &str, default: usize) -> Result<usize> {
    Ok(env_default(key, &default.to_string()).parse()?)
}
fn parse_u16(key: &str, default: u16) -> Result<u16> {
    Ok(env_default(key, &default.to_string()).parse()?)
}
fn normalize_table(s: &str) -> String {
    s.replace('[', "").replace(']', "").to_ascii_lowercase()
}
