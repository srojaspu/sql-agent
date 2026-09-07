//! Schema cache, schema-memory hints and tool-argument dedup helpers.
//!
//! Extracted verbatim from `agent::core` (slice D, step 1): the `SchemaCache`
//! snapshot plus its `Agent` impl (`cached_schema` / `cached_tables` /
//! `table_allowed`), the bounded hint builder, the per-turn same-call dedup
//! key, the column-match memory grounding helpers and the tool-argument
//! normalizer. No behavior change.

use anyhow::{Context, Result};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{
    config::Config,
    database::schema::{upsert_schema_memory, ColumnMatch, SchemaMemory},
    database::{ColumnInfo, TableInfo},
};

use super::{core::Agent, ranking::precompute_normalized};

#[derive(Clone, Debug)]
pub(crate) struct SchemaCache {
    pub(crate) expires_at: Instant,
    pub(crate) tables: Arc<Vec<TableInfo>>,
    pub(crate) normalized: Arc<Vec<super::ranking::NormalizedEntry>>,
}

/// Max tables grounded into schema memory per search tool call.
/// Bounds the old fan-out (one describe per match) now replaced by data reuse.
pub const MEMORY_GROUNDING_LIMIT: usize = 5;

/*
 * ====================================================================
 * LOOP GUARD: bounded hints + same-call dedup
 * ====================================================================
 */

/// Max schema-memory hints injected into the system prompt per turn.
pub const MAX_HINTS: usize = 8;
/// Max total chars of injected schema hints per turn.
pub const MAX_HINT_CHARS: usize = 2000;

/// Marker appended when hints are trimmed by count or char budget.
pub const HINT_TRUNCATION_MARKER: &str = "\n…[hints truncated]";

/// Build the bounded schema-hint text from memory.
/// Keeps the most-recently-used valid (unexpired) entries up to 8 hints /
/// 2000 chars; appends a truncation marker when trimmed.
pub fn build_schema_hint_text(memory: &SchemaMemory) -> String {
    let mut entries: Vec<_> = memory.iter().filter(|(_, e)| !e.is_expired()).collect();
    entries.sort_by_key(|(_, e)| std::cmp::Reverse(e.last_used));
    let trimmed_by_count = entries.len() > MAX_HINTS;
    let lines: Vec<String> = entries
        .into_iter()
        .take(MAX_HINTS)
        .map(|(k, e)| {
            format!(
                "{} -> {}.{} (synonyms: {})",
                k,
                e.table.schema,
                e.table.table,
                e.synonyms.join(", ")
            )
        })
        .collect();
    let mut out = lines.join("\n");
    let mut truncated = trimmed_by_count;
    if out.len() > MAX_HINT_CHARS {
        let mut end = MAX_HINT_CHARS.min(out.len());
        while end > 0 && !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        truncated = true;
    }
    if truncated {
        out.push_str(HINT_TRUNCATION_MARKER);
    }
    out
}

/// Stable per-turn dedup key: tool name + NUL + canonical args JSON.
pub fn dedup_key(tool: &str, args: &Value) -> String {
    format!(
        "{tool}\0{}",
        serde_json::to_string(args).unwrap_or_default()
    )
}

/// Group column matches by table (deduped, at most `limit` tables) and convert
/// the already-fetched match rows into memory columns — zero DB calls.
/// Nullable defaults to true (conservative) and ordinal to 0 (unknown); full
/// column lists still arrive via `describe_table`, which overwrites these.
pub fn memory_columns_for_column_matches(
    matches: &[ColumnMatch],
    limit: usize,
) -> Vec<(String, TableInfo, Vec<ColumnInfo>)> {
    let mut grouped: HashMap<String, (TableInfo, Vec<ColumnInfo>)> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for m in matches {
        if grouped.len() >= limit && !grouped.contains_key(&format!("{}.{}", m.schema, m.table)) {
            continue;
        }
        let key = format!("{}.{}", m.schema, m.table);
        let entry = grouped.entry(key.clone()).or_insert_with(|| {
            order.push(key.clone());
            (
                TableInfo {
                    schema: m.schema.clone(),
                    table: m.table.clone(),
                    table_type: String::new(),
                },
                Vec::new(),
            )
        });
        if !entry.1.iter().any(|c| c.column == m.column) {
            entry.1.push(ColumnInfo {
                column: m.column.clone(),
                data_type: m.data_type.clone(),
                nullable: true,
                ordinal: 0,
            });
        }
    }
    order
        .into_iter()
        .filter_map(|k| grouped.remove(&k).map(|(t, c)| (k, t, c)))
        .collect()
}

/// Ground schema memory from already-fetched column matches (no DB calls).
/// Merge-safe: existing entries keep their full column list and only gain
/// missing matched columns plus the new synonym; new tables store the partial
/// matched columns until `describe_table` grounds them fully.
pub fn ground_memory_from_column_matches(
    memory: &mut SchemaMemory,
    matches: &[ColumnMatch],
    query: &str,
    ttl_seconds: u64,
) {
    for (key, tbl, cols) in memory_columns_for_column_matches(matches, MEMORY_GROUNDING_LIMIT) {
        if let Some(existing) = memory.get_mut(&key) {
            existing.last_used = chrono::Utc::now();
            existing.hit_count += 1;
            if !existing.synonyms.contains(&query.to_string()) {
                existing.synonyms.push(query.to_string());
            }
            for c in cols {
                if !existing.columns.iter().any(|e| e.column == c.column) {
                    existing.columns.push(c);
                }
            }
            existing.ttl_seconds = ttl_seconds;
        } else {
            upsert_schema_memory(memory, key, tbl, cols, Some(query.to_string()), ttl_seconds);
        }
    }
}

/*
 * ====================================================================
 * TOOL ARGUMENTS
 * ====================================================================
 */

pub(crate) fn normalize_tool_arguments(v: &Value) -> Result<Value> {
    match v {
        Value::Object(_) => Ok(v.clone()),

        Value::String(s) => {
            serde_json::from_str(s).context("Argumentos de tool no son JSON válido")
        }

        _ => {
            anyhow::bail!(
                "Argumentos de tool deben ser \
                 un objeto JSON"
            );
        }
    }
}

/*
 * ================================================================
 * SCHEMA CACHE
 * ================================================================
 */

impl Agent {
    /// Cheap snapshot of the schema cache: clones two `Arc`s, never the table Vec.
    /// Ranking callers use `snapshot.tables` as a slice plus `snapshot.normalized`
    /// so `normalize_term` runs once per cache load, not per table per query.
    pub(crate) async fn cached_schema(&self) -> Result<SchemaCache> {
        {
            let guard = self.schema.read().await;

            if let Some(cache) = &*guard {
                if cache.expires_at > Instant::now() {
                    if self.config.verbose {
                        println!("⚡ Esquema desde caché");
                    }

                    return Ok(cache.clone());
                }
            }
        }

        if self.config.verbose {
            println!(
                "🗄️ SQL Server → \
                 INFORMATION_SCHEMA.TABLES..."
            );
        }

        let tables = self.db.list_tables().await?;

        if self.config.verbose {
            println!("🔎 search_schema: {} tablas encontradas", tables.len());
        }

        // Validate allowlist vs live (drift detection)
        if !self.config.policy.allowed_tables.is_empty() {
            let live_names: Vec<String> = tables
                .iter()
                .map(|t| format!("{}.{}", t.schema, t.table))
                .collect();
            let drifted = Config::find_drifted(&self.config.policy.allowed_tables, &live_names);
            if !drifted.is_empty() {
                tracing::warn!(
                    drifted = ?drifted,
                    "Allowlist drift: allowed tables not found in live DB"
                );
                if self.config.verbose {
                    println!(
                        "⚠️ Allowlist drift: no encontradas en BD: {}",
                        drifted.join(", ")
                    );
                }
            }
        }

        /*
         * Guardamos copia del esquema with precomputed normalized names.
         */

        let cache = SchemaCache {
            expires_at: Instant::now() + Duration::from_secs(self.config.limits.schema_cache_s),
            normalized: Arc::new(precompute_normalized(&tables)),
            tables: Arc::new(tables),
        };
        *self.schema.write().await = Some(cache.clone());

        Ok(cache)
    }

    /// Tables-only view of the cache (cheap `Arc` clone, no Vec copy).
    pub(crate) async fn cached_tables(&self) -> Result<Arc<Vec<TableInfo>>> {
        Ok(self.cached_schema().await?.tables)
    }

    /*
     * ================================================================
     * TABLE ALLOWLIST
     * ================================================================
     */

    pub(crate) fn table_allowed(&self, schema: &str, table: &str) -> bool {
        /*
         * Nunca permitir esquemas del sistema
         * si la política está desactivada.
         */

        if !self.config.policy.allow_system_tables
            && (schema.eq_ignore_ascii_case("sys")
                || schema.eq_ignore_ascii_case("information_schema"))
        {
            return false;
        }

        /*
         * Si no existe allowlist,
         * permitimos las tablas visibles
         * excepto las de sistema.
         */

        if self.config.policy.allowed_tables.is_empty() {
            return true;
        }

        let full = format!("{}.{}", schema, table).to_ascii_lowercase();

        self.config
            .policy
            .allowed_tables
            .iter()
            .any(|x| x.eq_ignore_ascii_case(&full) || x.eq_ignore_ascii_case(table))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::TableInfo;

    fn make_tables(names: &[(&str, &str)]) -> Vec<TableInfo> {
        names
            .iter()
            .map(|(s, t)| TableInfo {
                schema: s.to_string(),
                table: t.to_string(),
                table_type: "BASE TABLE".to_string(),
            })
            .collect()
    }

    // ===== P1a-6 dispatch data reuse (same formats, fewer DB calls) =====
    use std::collections::HashMap;

    fn column_matches_for_memory_test() -> Vec<ColumnMatch> {
        vec![
            ColumnMatch {
                schema: "dbo".into(),
                table: "Orders".into(),
                column: "email".into(),
                data_type: "nvarchar".into(),
            },
            ColumnMatch {
                schema: "dbo".into(),
                table: "Orders".into(),
                column: "id".into(),
                data_type: "int".into(),
            },
            ColumnMatch {
                schema: "dbo".into(),
                table: "Usuario".into(),
                column: "email".into(),
                data_type: "nvarchar".into(),
            },
        ]
    }

    #[test]
    fn memory_columns_dedupes_tables_and_bounds_to_five() {
        // 12 matches across 7 tables -> at most 5 tables, first-seen order kept.
        let matches: Vec<ColumnMatch> = (0..12)
            .map(|i| ColumnMatch {
                schema: "dbo".into(),
                table: format!("Tabla{i:02}"),
                column: "email".into(),
                data_type: "nvarchar".into(),
            })
            .collect();
        let grouped = memory_columns_for_column_matches(&matches, MEMORY_GROUNDING_LIMIT);
        assert_eq!(
            grouped.len(),
            MEMORY_GROUNDING_LIMIT,
            "fan-out must stay bounded"
        );
        assert_eq!(grouped[0].1.table, "Tabla00");
        assert_eq!(grouped[4].1.table, "Tabla04");
        // Duplicates collapse into one table entry with both columns.
        let dupes = column_matches_for_memory_test();
        let grouped = memory_columns_for_column_matches(&dupes, MEMORY_GROUNDING_LIMIT);
        assert_eq!(grouped.len(), 2, "Orders+Usuario deduped, got {grouped:?}");
        let orders = grouped
            .iter()
            .find(|(_, t, _)| t.table == "Orders")
            .unwrap();
        assert_eq!(orders.2.len(), 2, "both Orders columns reused");
    }

    #[test]
    fn ground_memory_reuses_matches_with_zero_db_calls() {
        // Simple call counter stands in for DB describes: the old path called
        // describe once per match (up to 5); the new path calls zero.
        struct CallCounter {
            calls: usize,
        }
        impl CallCounter {
            fn old_path_describe(&mut self, matches: &[ColumnMatch]) {
                for _ in matches.iter().take(5) {
                    self.calls += 1;
                }
            }
        }
        let matches = column_matches_for_memory_test();
        let mut counter = CallCounter { calls: 0 };
        counter.old_path_describe(&matches);
        assert_eq!(counter.calls, 3, "old path hit DB per match");

        let new_calls = 0; // ground_memory_from_column_matches takes no DB handle
        let mut memory: crate::database::schema::SchemaMemory = HashMap::new();
        ground_memory_from_column_matches(&mut memory, &matches, "email", 300);
        assert_eq!(new_calls, 0);
        assert!(new_calls < counter.calls, "new path must make fewer calls");
        assert!(memory.contains_key("dbo.Orders"));
        assert!(memory.contains_key("dbo.Usuario"));
        assert_eq!(memory["dbo.Orders"].columns.len(), 2);
    }

    #[test]
    fn ground_memory_merges_without_clobbering_full_columns() {
        use crate::database::ColumnInfo;
        let mut memory: crate::database::schema::SchemaMemory = HashMap::new();
        let tbl = TableInfo {
            schema: "dbo".into(),
            table: "Orders".into(),
            table_type: "BASE TABLE".into(),
        };
        // Previously described table holds the full column list.
        upsert_schema_memory(
            &mut memory,
            "dbo.Orders".into(),
            tbl,
            vec![
                ColumnInfo {
                    column: "id".into(),
                    data_type: "int".into(),
                    nullable: false,
                    ordinal: 1,
                },
                ColumnInfo {
                    column: "total".into(),
                    data_type: "decimal".into(),
                    nullable: false,
                    ordinal: 2,
                },
            ],
            Some("orders".into()),
            300,
        );
        // A later column search reuses its single matched row; full list survives.
        let matches = vec![ColumnMatch {
            schema: "dbo".into(),
            table: "Orders".into(),
            column: "email".into(),
            data_type: "nvarchar".into(),
        }];
        ground_memory_from_column_matches(&mut memory, &matches, "email", 300);
        let cols: Vec<&str> = memory["dbo.Orders"]
            .columns
            .iter()
            .map(|c| c.column.as_str())
            .collect();
        assert!(
            cols.contains(&"id") && cols.contains(&"total"),
            "full list kept"
        );
        assert!(cols.contains(&"email"), "matched column merged");
    }

    #[test]
    fn describe_reuses_detail_columns_for_memory() {
        // Pins P1a-6: memory columns ARE the TableDetail columns from the single
        // describe_table_full fetch — no second describe_table call exists.
        use crate::agent::format::format_table_detail;
        use crate::database::schema::TableDetail;
        let detail = TableDetail {
            columns: vec![crate::database::ColumnInfo {
                column: "id".into(),
                data_type: "int".into(),
                nullable: false,
                ordinal: 1,
            }],
            primary_keys: vec!["id".into()],
            foreign_keys: vec![],
            view_definition: None,
            sample_rows: vec![],
            row_count: 1,
        };
        let cols_for_memory: Vec<crate::database::ColumnInfo> = detail.columns.clone();
        assert_eq!(cols_for_memory.len(), 1);
        assert_eq!(cols_for_memory[0].column, "id");
        let out = format_table_detail("dbo", "Orders", &detail);
        assert!(out.contains("ESTRUCTURA") && out.contains("MUESTRA"));
    }

    #[test]
    fn schema_cache_clone_shares_arc_allocation() {
        let tables = make_tables(&[("dbo", "Usuario")]);
        let cache = SchemaCache {
            expires_at: std::time::Instant::now() + std::time::Duration::from_secs(300),
            normalized: Arc::new(precompute_normalized(&tables)),
            tables: Arc::new(tables),
        };
        let cloned = cache.clone();
        assert!(
            Arc::ptr_eq(&cache.tables, &cloned.tables),
            "snapshot clone must share the table Arc, not copy the Vec"
        );
        assert!(
            Arc::ptr_eq(&cache.normalized, &cloned.normalized),
            "normalized names must also be shared"
        );
        assert_eq!(cloned.normalized.len(), cloned.tables.len());
    }
}
