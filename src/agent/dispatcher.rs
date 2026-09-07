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
use serde_json::Value;

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
        match call.function.name.as_str() {
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
        }
    }
}
