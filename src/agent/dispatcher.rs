//! Unified tool dispatcher (single match over tool names).
//!
//! Slice D, step 2: `dispatch_tool` + `dispatch_tool_with_history` fused into
//! one `dispatch_tool(call, request_id, session_opt)`. The search/describe
//! arms ground `schema_memory` only when a session is present
//! (`MEMORY_GROUNDING_LIMIT` preserved); every other arm is byte-identical on
//! both paths. `run` (one-shot) and `run_with_history` (session/caps/persist)
//! keep their distinct loop responsibilities and both call this dispatcher.
//! `execute_read_tool` stays the single validation→audit→execute→format path.

use anyhow::Result;
use serde_json::{Value, json};
use std::time::Instant;

use crate::{
    database::schema::upsert_schema_memory, database::TableInfo, llm::ToolCall,
};

use super::{
    core::Agent,
    format::{format_column_matches, limit_text, split_table},
    memory::{
        ground_memory_from_column_matches, normalize_tool_arguments, MEMORY_GROUNDING_LIMIT,
    },
    session::Session,
};

impl Agent {
    /// Single tool dispatcher for both agent loops.
    ///
    /// `session_opt` is `None` on the one-shot path (`run`) and `Some` on the
    /// session path (`run_with_history`). Invalid-object re-inject (17
    /// candidates), hints, dedup, `limit_text` and redaction all flow through
    /// the same callees as before — no duplicated match arms remain.
    pub(crate) async fn dispatch_tool(
        &self,
        call: &ToolCall,
        request_id: &str,
        session_opt: Option<&mut Session>,
    ) -> Result<String> {
        let args = normalize_tool_arguments(&call.function.arguments)?;
        let tool_name = call.function.name.clone();
        let started = Instant::now();
        let result = match call.function.name.as_str() {
            "search_schema" => {
                let query_raw = args
                    .get("query")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                // Single ranking inside; reuse `matched` for memory (no
                // second search_with_fallback, no per-table describe calls).
                let (result, matched) = self.search_schema_ranked(&query_raw).await?;
                if let Some(session) = session_opt {
                    for tbl in matched.iter().take(MEMORY_GROUNDING_LIMIT) {
                        let key = format!("{}.{}", tbl.schema, tbl.table);
                        // No DB fetch: ground with table identity + synonym only.
                        // Empty columns preserve any previously stored full list.
                        upsert_schema_memory(
                            &mut session.schema_memory,
                            key,
                            tbl.clone(),
                            Vec::new(),
                            Some(query_raw.clone()),
                            self.config.limits.schema_cache_s,
                        );
                    }
                }
                Ok(result)
            }
            "describe_table" => {
                let table = args
                    .get("table")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                // Single describe_table_full fetch; reuse its columns for
                // memory instead of a second describe_table DB call.
                let (result, cols) = self.describe_table_tool_with_detail(&args).await?;
                if let Some(session) = session_opt {
                    let (schema, name) = split_table(&table);
                    let key = format!("{}.{}", schema, name);
                    let tbl = TableInfo {
                        schema: schema.clone(),
                        table: name.clone(),
                        // Real type travels on discovery lists; unknown on this path.
                        table_type: String::new(),
                    };
                    upsert_schema_memory(
                        &mut session.schema_memory,
                        key,
                        tbl,
                        cols,
                        Some(table.clone()),
                        self.config.limits.schema_cache_s,
                    );
                }
                Ok(result)
            }
            "list_tables" => self.list_tables_tool().await,
            "search_columns" => {
                let query = args
                    .get("query")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let matches = self.search_columns_data(&query).await?;
                // Reuse already-fetched match rows for memory (deduped by
                // table, max MEMORY_GROUNDING_LIMIT tables, zero DB calls).
                if let Some(session) = session_opt {
                    ground_memory_from_column_matches(
                        &mut session.schema_memory,
                        &matches,
                        &query,
                        self.config.limits.schema_cache_s,
                    );
                }
                Ok(limit_text(
                    &format_column_matches(&matches, &query),
                    self.config.limits.max_tool_result_chars,
                ))
            }
            "execute_read_query" => self.execute_read_tool(&args, request_id).await,
            other => anyhow::bail!("Tool no permitida: {other}"),
        };
        let latency_ms = started.elapsed().as_millis() as u64;
        let _ = self
            .audit(
                "tool_latency",
                json!({
                    "request_id": request_id,
                    "tool": tool_name,
                    "latency_ms": latency_ms,
                    "success": result.is_ok()
                }),
            )
            .await;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dispatcher_test_config(audit_path: String) -> crate::config::Config {
        crate::config::Config {
            db: crate::config::DbConfig {
                host: "localhost".into(),
                port: 1433,
                name: "TestDB".into(),
                user: "user".into(),
                password: "pass".into(),
                trust_cert: true,
            },
            llm: crate::config::LlmConfig {
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
            policy: crate::config::PolicyConfig {
                allowed_tables: vec![],
                block_sensitive_columns: true,
                block_comments: true,
                allow_cte: true,
                allow_system_tables: false,
            },
            limits: crate::config::LimitsConfig {
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
            audit: crate::config::AuditConfig {
                enabled: true,
                path: audit_path,
                capture_sql: false,
            },
            verbose: false,
        }
    }

    #[tokio::test]
    async fn dispatch_records_tool_latency_audit() {
        let dir =
            std::env::temp_dir().join(format!("sql-agent-latency-{}", uuid::Uuid::new_v4()));
        let path = dir.join("audit.jsonl");
        let path_str = path.to_string_lossy().to_string();
        let agent = Agent::new(dispatcher_test_config(path_str.clone()));
        // Policy-blocked SQL returns Ok without touching the DB.
        let call = crate::llm::ToolCall {
            id: None,
            function: crate::llm::ToolFunction {
                name: "execute_read_query".into(),
                arguments: serde_json::json!({"sql": "DELETE FROM dbo.Usuario"}),
            },
        };
        let out = agent
            .dispatch_tool(&call, "req-latency", None)
            .await
            .expect("blocked SQL should return Ok message");
        assert!(out.contains("bloqueada") || out.contains("seguridad"));
        // Second dispatch on the session path must also record latency.
        let mut session = crate::agent::session::Session::new();
        let out2 = agent
            .dispatch_tool(&call, "req-latency-session", Some(&mut session))
            .await
            .expect("session dispatch should also succeed");
        assert!(out2.contains("bloqueada") || out2.contains("seguridad"));
        let content = tokio::fs::read_to_string(&path)
            .await
            .expect("audit file should exist after dispatch");
        let mut found_none = false;
        let mut found_session = false;
        for line in content.lines() {
            let v: serde_json::Value = serde_json::from_str(line).expect("valid JSONL");
            if v.get("event").and_then(|e| e.as_str()) == Some("tool_latency") {
                let payload = &v["payload"];
                assert_eq!(payload.get("tool").and_then(|t| t.as_str()), Some("execute_read_query"));
                let req = payload
                    .get("request_id")
                    .and_then(|r| r.as_str())
                    .expect("request_id present");
                assert!(
                    req == "req-latency" || req == "req-latency-session",
                    "unexpected request_id {req}"
                );
                let latency = payload
                    .get("latency_ms")
                    .and_then(|l| l.as_u64())
                    .expect("latency_ms must be present as u64");
                // Non-negative is inherent to u64; assert presence plus success flag.
                assert!(latency < 60_000, "latency should be sane, got {latency}");
                assert_eq!(
                    payload.get("success").and_then(|s| s.as_bool()),
                    Some(true),
                    "blocked SQL returns Ok message so success must be true"
                );
                if req == "req-latency" {
                    found_none = true;
                } else {
                    found_session = true;
                }
            }
        }
        assert!(
            found_none && found_session,
            "tool_latency must be recorded on both None and session paths, got: {content}"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
