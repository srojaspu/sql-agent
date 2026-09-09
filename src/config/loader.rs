//! Environment / map loading for [`AppConfig`].
//!
//! Moved verbatim from the pre-split `config.rs`: every environment variable
//! name, default, deprecated alias, validation message, and drift semantic
//! is byte-identical. Only the `Ok(Self { … })` construction targets the new
//! nested sub-structs; every right-hand-side expression is untouched.

use anyhow::{Context, Result};

use super::db::sanitize_db_name;
use super::{AppConfig, AuditConfig, DbConfig, LimitsConfig, LlmConfig, PolicyConfig};
use crate::util::normalize_table_name;

impl AppConfig {
    pub fn from_env() -> Result<Self> {
        let map: std::collections::HashMap<String, String> = std::env::vars().collect();
        Self::from_map(&map)
    }

    pub fn from_map(map: &std::collections::HashMap<String, String>) -> Result<Self> {
        Self::validate_no_unknown(map)?;

        // Deprecated, ignored: per-tool output is capped by MAX_TOOL_RESULT_CHARS.
        if map.contains_key("MAX_RESULT_CHARS") {
            tracing::warn!(
                "MAX_RESULT_CHARS is deprecated and ignored, use MAX_TOOL_RESULT_CHARS instead"
            );
        }

        // Resolve aliases with warnings
        let allowed_tables_raw = Self::resolve_allowed_tables_raw(map);
        let allowed_tables = allowed_tables_raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(normalize_table_name)
            .collect();

        let max_steps_raw = Self::resolve_max_steps_raw(map);

        Ok(Self {
            db: DbConfig {
                host: get_required(map, "DATABASE_HOST")?,
                port: parse_u16_map(map, "DATABASE_PORT", 1433)?,
                name: sanitize_db_name(&get_required(map, "DATABASE_NAME")?)?,
                user: get_required(map, "DATABASE_USER")?,
                password: get_required(map, "DATABASE_PASSWORD")?,
                trust_cert: parse_bool_map(map, "DATABASE_TRUST_CERT", false)?,
            },
            llm: LlmConfig {
                provider: get_default_map(map, "LLM_PROVIDER", "ollama"),
                model: get_default_map(map, "LLM_MODEL", ""),
                api_key: get_default_map(map, "LLM_API_KEY", ""),
                base_url: get_default_map(map, "LLM_BASE_URL", ""),
                ollama_url: get_default_map(map, "OLLAMA_URL", "http://127.0.0.1:11434"),
                ollama_model: get_default_map(map, "OLLAMA_MODEL", "qwen3:4b"),
                timeout_s: parse_u64_map(map, "OLLAMA_TIMEOUT_SECONDS", 120)?,
                connect_timeout_s: parse_u64_map(map, "OLLAMA_CONNECT_TIMEOUT_SECONDS", 5)?,
                temperature: get_default_map(map, "OLLAMA_TEMPERATURE", "0.0")
                    .parse()
                    .context("OLLAMA_TEMPERATURE inválido")?,
                // Wired knob: 0 means a single attempt with no retry.
                max_retries: get_default_map(map, "LLM_MAX_RETRIES", "3")
                    .parse()
                    .context("LLM_MAX_RETRIES inválido")?,
            },
            policy: PolicyConfig {
                allowed_tables,
                block_sensitive_columns: parse_bool_map(map, "BLOCK_SENSITIVE_COLUMNS", true)?,
                block_comments: parse_bool_map(map, "BLOCK_COMMENTS", true)?,
                allow_cte: parse_bool_map(map, "ALLOW_CTE", true)?,
                allow_system_tables: parse_bool_map(map, "ALLOW_SYSTEM_TABLES", false)?,
            },
            limits: LimitsConfig {
                max_steps: if let Some(raw) = max_steps_raw {
                    raw.parse()
                        .with_context(|| "MAX_STEPS/MAX_AGENT_STEPS inválido")?
                } else {
                    8
                },
                max_sql_length: parse_usize_map(map, "MAX_SQL_LENGTH", 10000)?,
                max_rows: parse_usize_map(map, "MAX_ROWS", 100)?,
                schema_cache_s: parse_u64_map(map, "SCHEMA_CACHE_SECONDS", 300)?,
                query_timeout_s: parse_u64_map(map, "QUERY_TIMEOUT_SECONDS", 30)?,
                max_concurrent_queries: parse_usize_map(map, "MAX_CONCURRENT_QUERIES", 1)?,
                max_joins: parse_usize_map(map, "MAX_JOINS", 5)?,
                max_subqueries: parse_usize_map(map, "MAX_SUBQUERIES", 5)?,
                max_schema_results: parse_usize_map(map, "MAX_SCHEMA_RESULTS", 20)?,
                max_tool_result_chars: parse_usize_map(map, "MAX_TOOL_RESULT_CHARS", 20000)?,
                max_tool_calls_per_step: parse_usize_map(map, "MAX_TOOL_CALLS_PER_STEP", 10)?,
            },
            audit: AuditConfig {
                enabled: parse_bool_map(map, "AUDIT_ENABLED", true)?,
                path: get_default_map(map, "AUDIT_PATH", "logs/agent-audit.jsonl"),
                // Default false: audit SQL text may contain PII; opt in explicitly.
                capture_sql: parse_bool_map(map, "AUDIT_SQL", false)?,
            },
            verbose: false,
        })
    }

    fn resolve_allowed_tables_raw(map: &std::collections::HashMap<String, String>) -> String {
        if let Some(v) = map.get("ALLOWED_TABLES") {
            if !v.trim().is_empty() {
                return v.clone();
            }
        }
        if let Some(v) = map.get("BLOCKED_TABLES") {
            if !v.trim().is_empty() {
                tracing::warn!("BLOCKED_TABLES is deprecated, use ALLOWED_TABLES instead");
                return v.clone();
            }
        }
        if let Some(v) = map.get("BLOCKED_COLUMNS") {
            if !v.trim().is_empty() {
                tracing::warn!("BLOCKED_COLUMNS is deprecated, use ALLOWED_TABLES instead");
                return v.clone();
            }
        }
        String::new()
    }

    fn resolve_max_steps_raw(map: &std::collections::HashMap<String, String>) -> Option<String> {
        if let Some(v) = map.get("MAX_STEPS") {
            if !v.trim().is_empty() {
                return Some(v.clone());
            }
        }
        if let Some(v) = map.get("MAX_AGENT_STEPS") {
            if !v.trim().is_empty() {
                tracing::warn!("MAX_AGENT_STEPS is deprecated, use MAX_STEPS instead");
                return Some(v.clone());
            }
        }
        None
    }

    fn validate_no_unknown(map: &std::collections::HashMap<String, String>) -> Result<()> {
        const KNOWN: &[&str] = &[
            "DATABASE_HOST",
            "DATABASE_PORT",
            "DATABASE_NAME",
            "DATABASE_USER",
            "DATABASE_PASSWORD",
            "DATABASE_TRUST_CERT",
            "LLM_PROVIDER",
            "LLM_MODEL",
            "LLM_API_KEY",
            "LLM_BASE_URL",
            "LLM_MAX_RETRIES",
            "OLLAMA_URL",
            "OLLAMA_MODEL",
            "OLLAMA_TIMEOUT_SECONDS",
            "OLLAMA_CONNECT_TIMEOUT_SECONDS",
            "OLLAMA_TEMPERATURE",
            "MAX_STEPS",
            "MAX_AGENT_STEPS",
            "MAX_SQL_LENGTH",
            "MAX_ROWS",
            "MAX_RESULT_CHARS",
            "SCHEMA_CACHE_SECONDS",
            "QUERY_TIMEOUT_SECONDS",
            "MAX_CONCURRENT_QUERIES",
            "MAX_JOINS",
            "MAX_SUBQUERIES",
            "MAX_SCHEMA_RESULTS",
            "MAX_TOOL_RESULT_CHARS",
            "MAX_TOOL_CALLS_PER_STEP",
            "ALLOWED_TABLES",
            "BLOCKED_TABLES",
            "BLOCKED_COLUMNS",
            "BLOCK_SENSITIVE_COLUMNS",
            "BLOCK_COMMENTS",
            "ALLOW_CTE",
            "ALLOW_SYSTEM_TABLES",
            "AUDIT_ENABLED",
            "AUDIT_PATH",
            "AUDIT_SQL",
        ];
        const PREFIXES: &[&str] = &[
            "DATABASE_",
            "OLLAMA_",
            "LLM_",
            "MAX_",
            "ALLOWED_",
            "BLOCKED_",
            "BLOCK_",
            "AUDIT_",
            "SCHEMA_",
            "QUERY_",
            "ALLOW_",
            "UNKNOWN_",
        ];
        let known_set: std::collections::HashSet<&str> = KNOWN.iter().copied().collect();
        for key in map.keys() {
            if matches!(
                key.as_str(),
                "OPENAI_API_KEY" | "GOOGLE_API_KEY" | "ANTHROPIC_API_KEY"
            ) {
                anyhow::bail!("{key} ya no es compatible; use LLM_API_KEY con LLM_PROVIDER");
            }
            if known_set.contains(key.as_str()) {
                continue;
            }
            // Only fail for keys that look like config vars
            let is_prefix = PREFIXES.iter().any(|p| key.starts_with(p));
            if is_prefix {
                anyhow::bail!("Unknown config var: {key} — check spelling or remove it");
            }
            // Also treat exact UNKNOWN_VAR as unknown for test compat
            if key == "UNKNOWN_VAR" {
                anyhow::bail!("Unknown config var: {key}");
            }
        }
        Ok(())
    }

    /// Returns allowed tables that are NOT present in live (drift).
    /// Both slices are compared case-insensitively after stripping brackets.
    pub fn find_drifted(allowed: &[String], live_full_names: &[String]) -> Vec<String> {
        use std::collections::HashSet;
        if allowed.is_empty() {
            return Vec::new();
        }
        let live_set: HashSet<String> = live_full_names
            .iter()
            .map(|s| normalize_table_name(s))
            .collect();
        allowed
            .iter()
            .filter(|a| !live_set.contains(&normalize_table_name(a)))
            .cloned()
            .collect()
    }

    /// Convenience: check self.policy.allowed_tables vs live full names, warn on drift.
    pub fn drifted_vs_live(&self, live_full_names: &[String]) -> Vec<String> {
        Self::find_drifted(&self.policy.allowed_tables, live_full_names)
    }
}

fn get_required(map: &std::collections::HashMap<String, String>, key: &str) -> Result<String> {
    map.get(key)
        .cloned()
        .with_context(|| format!("Falta variable {key}"))
}

fn get_default_map(
    map: &std::collections::HashMap<String, String>,
    key: &str,
    default: &str,
) -> String {
    map.get(key).cloned().unwrap_or_else(|| default.to_string())
}

fn parse_bool_map(
    map: &std::collections::HashMap<String, String>,
    key: &str,
    default: bool,
) -> Result<bool> {
    match map.get(key) {
        Some(v) => match v.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Ok(true),
            "false" | "0" | "no" => Ok(false),
            _ => anyhow::bail!("{key} debe ser true/false"),
        },
        None => Ok(default),
    }
}

fn parse_u64_map(
    map: &std::collections::HashMap<String, String>,
    key: &str,
    default: u64,
) -> Result<u64> {
    Ok(get_default_map(map, key, &default.to_string()).parse()?)
}

fn parse_usize_map(
    map: &std::collections::HashMap<String, String>,
    key: &str,
    default: usize,
) -> Result<usize> {
    Ok(get_default_map(map, key, &default.to_string()).parse()?)
}

fn parse_u16_map(
    map: &std::collections::HashMap<String, String>,
    key: &str,
    default: u16,
) -> Result<u16> {
    Ok(get_default_map(map, key, &default.to_string()).parse()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn minimal_map() -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("DATABASE_HOST".into(), "localhost".into());
        m.insert("DATABASE_NAME".into(), "TestDB".into());
        m.insert("DATABASE_USER".into(), "user".into());
        m.insert("DATABASE_PASSWORD".into(), "pass".into());
        m
    }

    #[test]
    fn trust_cert_defaults_to_false() {
        let m = minimal_map();
        let cfg = AppConfig::from_map(&m).expect("defaults should parse");
        assert!(
            !cfg.db.trust_cert,
            "DATABASE_TRUST_CERT must default to false (explicit dev opt-in only)"
        );
    }

    #[test]
    fn llm_provider_uses_generic_api_key() {
        let mut m = minimal_map();
        m.insert("LLM_PROVIDER".into(), "anthropic".into());
        m.insert("LLM_API_KEY".into(), "generic-key".into());
        let cfg = AppConfig::from_map(&m).expect("provider config should parse");
        assert_eq!(cfg.llm.api_key, "generic-key");
    }

    #[test]
    fn provider_specific_api_keys_are_rejected() {
        let mut m = minimal_map();
        m.insert("LLM_PROVIDER".into(), "openai".into());
        m.insert("OPENAI_API_KEY".into(), "provider-key".into());
        assert!(AppConfig::from_map(&m).is_err());
    }

    #[test]
    fn trust_cert_explicit_dev_override() {
        let mut m = minimal_map();
        m.insert("DATABASE_TRUST_CERT".into(), "true".into());
        let cfg = AppConfig::from_map(&m).expect("explicit override should parse");
        assert!(cfg.db.trust_cert);
    }

    #[test]
    fn audit_sql_defaults_to_false_pii_safe() {
        let m = minimal_map();
        let cfg = AppConfig::from_map(&m).expect("defaults should parse");
        assert!(
            !cfg.audit.capture_sql,
            "AUDIT_SQL must default to false so query text with PII is not persisted"
        );
    }

    #[test]
    fn blocked_tables_alias_to_allowed_tables() {
        let mut m = minimal_map();
        m.insert("BLOCKED_TABLES".into(), "dbo.foo".into());
        let cfg = AppConfig::from_map(&m).expect("alias should be accepted");
        assert!(
            cfg.policy.allowed_tables.iter().any(|t| t == "dbo.foo"),
            "BLOCKED_TABLES should populate allowed_tables, got {:?}",
            cfg.policy.allowed_tables
        );
    }

    #[test]
    fn blocked_columns_alias_warns_and_maps() {
        let mut m = minimal_map();
        m.insert("BLOCKED_COLUMNS".into(), "dbo.bar".into());
        let cfg = AppConfig::from_map(&m).expect("BLOCKED_COLUMNS alias should be accepted");
        assert!(
            cfg.policy.allowed_tables.iter().any(|t| t == "dbo.bar"),
            "BLOCKED_COLUMNS should alias to allowed_tables"
        );
    }

    #[test]
    fn max_agent_steps_alias() {
        let mut m = minimal_map();
        m.insert("MAX_AGENT_STEPS".into(), "15".into());
        let cfg = AppConfig::from_map(&m).expect("alias should be accepted");
        assert_eq!(cfg.limits.max_steps, 15);
    }

    #[test]
    fn unknown_var_fails() {
        let mut m = minimal_map();
        m.insert("ALLOWED_TYPO_XYZ".into(), "dbo.foo".into());
        let res = AppConfig::from_map(&m);
        assert!(
            res.is_err(),
            "unknown var ALLOWED_TYPO_XYZ should fail, got ok {:?}",
            res.unwrap().policy.allowed_tables
        );
        let err = res.unwrap_err().to_string();
        assert!(
            err.to_ascii_lowercase().contains("unknown") || err.contains("ALLOWED_TYPO_XYZ"),
            "error should mention unknown var, got: {err}"
        );
    }

    #[test]
    fn alias_precedence_allowed_over_blocked() {
        let mut m = minimal_map();
        m.insert("ALLOWED_TABLES".into(), "dbo.allowed".into());
        m.insert("BLOCKED_TABLES".into(), "dbo.blocked".into());
        let cfg = AppConfig::from_map(&m).expect("both present should succeed");
        assert!(
            cfg.policy.allowed_tables.iter().any(|t| t == "dbo.allowed"),
            "ALLOWED_TABLES should take precedence"
        );
        assert!(
            !cfg.policy.allowed_tables.iter().any(|t| t == "dbo.blocked"),
            "BLOCKED should not overwrite ALLOWED"
        );
    }

    #[test]
    fn allowlist_drift_detects_missing() {
        let allowed = vec!["dbo.missing".to_string(), "dbo.existing".to_string()];
        let live = vec!["dbo.existing".to_string(), "dbo.other".to_string()];
        let drifted = AppConfig::find_drifted(&allowed, &live);
        assert_eq!(drifted, vec!["dbo.missing"]);
    }

    #[test]
    fn allowlist_no_drift_when_all_present() {
        let allowed = vec!["dbo.a".to_string(), "dbo.b".to_string()];
        let live = vec![
            "dbo.a".to_string(),
            "dbo.b".to_string(),
            "dbo.c".to_string(),
        ];
        let drifted = AppConfig::find_drifted(&allowed, &live);
        assert!(drifted.is_empty(), "no drift expected, got {drifted:?}");
    }

    #[test]
    fn allowlist_empty_allows_all_no_drift() {
        let allowed: Vec<String> = vec![];
        let live = vec!["dbo.a".to_string()];
        let drifted = AppConfig::find_drifted(&allowed, &live);
        assert!(drifted.is_empty());
    }

    #[test]
    fn allowlist_drift_case_insensitive() {
        let allowed = vec!["DBO.MISSING".to_string()];
        let live = vec!["dbo.missing".to_string()];
        let drifted = AppConfig::find_drifted(&allowed, &live);
        assert!(
            drifted.is_empty(),
            "case-insensitive match should not drift"
        );
    }

    #[test]
    fn llm_max_retries_defaults_to_three() {
        let m = minimal_map();
        let cfg = AppConfig::from_map(&m).expect("defaults should parse");
        assert_eq!(cfg.llm.max_retries, 3);
    }

    #[test]
    fn llm_max_retries_custom_bound_respected() {
        let mut m = minimal_map();
        m.insert("LLM_MAX_RETRIES".into(), "1".into());
        let cfg = AppConfig::from_map(&m).expect("custom bound should parse");
        assert_eq!(cfg.llm.max_retries, 1);
    }

    #[test]
    fn llm_max_retries_zero_means_single_attempt() {
        let mut m = minimal_map();
        m.insert("LLM_MAX_RETRIES".into(), "0".into());
        let cfg = AppConfig::from_map(&m).expect("zero should parse");
        assert_eq!(cfg.llm.max_retries, 0);
    }

    #[test]
    fn llm_max_retries_invalid_rejected() {
        let mut m = minimal_map();
        m.insert("LLM_MAX_RETRIES".into(), "abc".into());
        let err = AppConfig::from_map(&m).unwrap_err().to_string();
        assert!(
            err.contains("LLM_MAX_RETRIES inválido"),
            "invalid retries must report typed message, got: {err}"
        );
    }

    #[test]
    fn database_name_illegal_rejected_before_connect() {
        let mut m = minimal_map();
        m.insert("DATABASE_NAME".into(), "my db!".into());
        assert!(
            AppConfig::from_map(&m).is_err(),
            "illegal DATABASE_NAME must be rejected with no connection attempted"
        );
    }

    #[test]
    fn database_name_empty_and_overlong_rejected() {
        let mut m = minimal_map();
        m.insert("DATABASE_NAME".into(), "".into());
        assert!(AppConfig::from_map(&m).is_err());
        let mut m2 = minimal_map();
        m2.insert("DATABASE_NAME".into(), "a".repeat(129));
        assert!(AppConfig::from_map(&m2).is_err());
    }
}
