//! Core LLM-facing text formatters (pure domain logic).
//!
//! Extracted from `agent::core` (slice D, step 1): table/column/result
//! formatters, the invalid-object re-inject builder, `limit_text`,
//! `looks_like_sql` and `split_table` (+ `split_table_pub`).
//!
//! `split_table` intentionally stays a thin wrapper over the shared
//! `util::split_table_name` helper instead of being unified with it: behavior
//! is identical on all tested inputs, but this wrapper keeps the missing-schema
//! `tracing::warn` the sqlserver path relies on staying silent about.

use serde_json::Value;

use crate::{
    database::schema::{ColumnMatch, TableDetail},
    database::TableInfo,
    security::is_sensitive_column,
    util::split_table_name as shared_split_table,
};

/*
 * ====================================================================
 * TABLE NAME
 * ====================================================================
 */

pub fn split_table(s: &str) -> (String, String) {
    // Shared core handles trim/strip/split/dbo-default; keep the warn here
    // so the sqlserver path stays silent as before.
    let out = shared_split_table(s);
    if !s.contains('.') {
        tracing::warn!(
            "split_table: no schema supplied for '{}', defaulting to dbo (explicit schema recommended)",
            s
        );
    }
    out
}

pub fn split_table_pub(s: &str) -> (String, String) {
    split_table(s)
}

/*
 * ====================================================================
 * LIMIT TEXT
 * ====================================================================
 */

pub fn limit_text(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }

    let mut end = max.min(text.len());

    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }

    format!("{}...[resultado recortado]", &text[..end])
}

/// Render a tool-dispatch failure as a recoverable tool result so one bad call
/// (empty/malformed args, disallowed table, unknown tool) never aborts the whole
/// turn. The model reads the message and retries with corrected arguments
/// within MAX_STEPS.
pub fn tool_error_result(name: &str, err: &anyhow::Error) -> String {
    format!(
        "❌ Error en herramienta '{name}': {err}\n\
         Corregí los argumentos (o el nombre calificado de tabla/columna) y reintentá."
    )
}

/*
 * ====================================================================
 * SQL DETECTION
 * ====================================================================
 */

pub fn looks_like_sql(s: &str) -> bool {
    extract_sql(s).is_some()
}

/// Detect a bare read query in plain text OR inside a Markdown code fence
/// (` ```sql ` / ` ``` `), returning the inner SQL text when present.
/// Case-insensitive fence detection; the result has surrounding whitespace
/// and fences stripped so it can be passed straight to the validator.
pub fn extract_sql(s: &str) -> Option<String> {
    let mut t = s.trim().to_string();

    // Strip an opening fence (```sql or ```), case-insensitive by length.
    let lower = t.to_ascii_lowercase();
    if lower.starts_with("```sql") {
        t = t[7..].trim_start().to_string();
    } else if lower.starts_with("```") {
        t = t[3..].trim_start().to_string();
    }

    // Strip a trailing fence.
    if t.trim_end().ends_with("```") {
        let trimmed = t.trim_end();
        let end = trimmed.len() - 3;
        t = trimmed[..end].trim_end().to_string();
    }

    let upper = t.to_ascii_uppercase();
    let is_sql = upper.starts_with("SELECT ")
        || upper == "SELECT"
        || upper.starts_with("WITH ")
        || upper == "WITH";

    if is_sql {
        Some(t)
    } else {
        None
    }
}

/// Format the full table/view listing for the LLM (SG-1 discovery).
/// Pure helper: every entry shows schema, name and type (BASE TABLE / VIEW).
pub fn format_table_list(tables: &[TableInfo]) -> String {
    let mut out = format!("✓ TABLAS Y VISTAS: {}\n", tables.len());
    if tables.is_empty() {
        out.push_str(
            "No hay tablas visibles para tu filtro.\n\
             ⚠️ No inventes nombres de tabla; pide aclaración o ajusta el filtro.\n",
        );
    } else {
        for t in tables {
            out.push_str(&format!(
                "  • {}.{} [{}]\n",
                t.schema, t.table, t.table_type
            ));
        }
        out.push_str(
            "\n→ SIGUIENTE PASO: usa describe_table con el nombre calificado \
             EXACTO de la lista, o search_columns para buscar por columna.\n",
        );
    }
    out
}

/// Format column search results for the LLM (SG-1 discovery).
/// Pure helper: every match shows its qualified table.column pair.
pub fn format_column_matches(matches: &[ColumnMatch], query: &str) -> String {
    let mut out = format!("✓ COLUMNAS para '{query}': {}\n", matches.len());
    if matches.is_empty() {
        out.push_str(&format!(
            "No se encontraron columnas para '{query}'.\n\
             → Usa list_tables para explorar el esquema completo; \
             no inventes nombres de columna.\n"
        ));
    } else {
        for m in matches {
            out.push_str(&format!(
                "  • {}.{}.{} ({})\n",
                m.schema, m.table, m.column, m.data_type
            ));
        }
        out.push_str(
            "\n→ SIGUIENTE PASO: usa describe_table con la tabla EXACTA \
             de la lista y luego execute_read_query.\n",
        );
    }
    out
}

/// Format a single JSON cell for LLM display (shared by the execute_read
/// row path and the describe sample path so both render identically).
/// `Null` renders as `[NULL]`; anything without a scalar mapping renders
/// as `[complex]` instead of leaking debug output.
pub fn format_cell_value(v: &Value) -> String {
    match v {
        Value::Null => "[NULL]".to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        _ => "[complex]".to_string(),
    }
}

/// Format query rows for the LLM, redacting sensitive values by header.
/// Defense-in-depth: even if a sensitive value reaches this layer (e.g.
/// legacy `SELECT *`), the header check renders `[REDACTED]` instead.
pub fn format_query_result(rows: &[Value], truncated: bool) -> String {
    let mut formatted = format!("✓ RESULTADOS ({} filas)\n\n", rows.len());
    if rows.is_empty() {
        formatted.push_str("No se encontraron datos.\n");
    } else {
        for (idx, row) in rows.iter().enumerate() {
            if let Some(obj) = row.as_object() {
                let cells: Vec<String> = obj
                    .iter()
                    .map(|(key, val)| {
                        let display_val = if is_sensitive_column(key) {
                            "[REDACTED]".to_string()
                        } else {
                            format_cell_value(val)
                        };
                        format!("{key}={display_val}")
                    })
                    .collect();
                formatted.push_str(&format!("Fila {}: {}\n", idx + 1, cells.join(", ")));
            } else {
                formatted.push_str(&format!("Fila {}: {row}\n", idx + 1));
            }
        }
    }
    if truncated {
        formatted.push_str("\n[Nota: Resultado limitado al máximo configurado]\n");
    }
    formatted
}

/// Format distinct column values as a compact list for the LLM. Each row
/// carries a single key/value pair; the first value is extracted directly.
/// Sensitive values are already blocked by the validator before this runs, so
/// this renders the raw value (defense-in-depth redaction still applies if a
/// sensitive header somehow reaches here).
pub fn format_distinct_values(rows: &[Value], column: &str) -> String {
    let mut out = format!("✓ VALORES DISTINTOS de '{column}': {}\n", rows.len());
    if rows.is_empty() {
        out.push_str("  (sin valores)\n");
    } else {
        for row in rows {
            let val = row
                .as_object()
                .and_then(|o| o.values().next())
                .map(format_cell_value)
                .unwrap_or_else(|| "[complex]".to_string());
            out.push_str(&format!("  • {val}\n"));
        }
    }
    out.push_str("→ Usá SOLO estos valores para filtrar en el WHERE.\n");
    out
}

/// Format the enriched describe output for the LLM (SG-2).
/// Pure helper: ESTRUCTURA + PK + FK + [VIEW definition] + MUESTRA + COUNT(*).
pub fn format_table_detail(schema: &str, name: &str, detail: &TableDetail) -> String {
    let mut out = format!("\n✓ ESTRUCTURA DE {schema}.{name}\n\nCOLUMNAS:\n");
    for col in &detail.columns {
        out.push_str(&format!(
            "  • {} ({}){}\n",
            col.column,
            col.data_type,
            if col.nullable {
                " [NULLABLE]"
            } else {
                " [NO NULO]"
            }
        ));
    }

    if detail.primary_keys.is_empty() {
        out.push_str("\nPRIMARY KEY: (ninguna)\n");
    } else {
        out.push_str(&format!(
            "\nPRIMARY KEY: {}\n",
            detail.primary_keys.join(", ")
        ));
    }

    if detail.foreign_keys.is_empty() {
        out.push_str("FOREIGN KEYS: (ninguna)\n");
    } else {
        out.push_str("FOREIGN KEYS:\n");
        for fk in &detail.foreign_keys {
            out.push_str(&format!(
                "  • {} → {}.{}({})\n",
                fk.column, fk.ref_schema, fk.ref_table, fk.ref_column
            ));
        }
    }

    if let Some(def) = &detail.view_definition {
        out.push_str(&format!("\n[VIEW] Definición:\n{def}\n"));
    } else {
        // None is ambiguous: base table OR view hidden by a missing VIEW
        // DEFINITION grant. Never stay silent so the LLM does not misread
        // absence as "not a view".
        out.push_str("\n[VIEW] Definición: definition unavailable (permissions) — base table or missing VIEW DEFINITION grant.\n");
    }

    out.push_str(&format!(
        "\nMUESTRA (TOP 5, {} filas):\n",
        detail.sample_rows.len()
    ));
    if detail.sample_rows.is_empty() {
        out.push_str("  (sin filas)\n");
    } else {
        for (idx, row) in detail.sample_rows.iter().enumerate() {
            if let Some(obj) = row.as_object() {
                let cells: Vec<String> = obj
                    .iter()
                    .map(|(k, v)| {
                        let display = if is_sensitive_column(k) {
                            "[REDACTED]".to_string()
                        } else {
                            format_cell_value(v)
                        };
                        format!("{k} = {display}")
                    })
                    .collect();
                out.push_str(&format!("  Fila {}: {}\n", idx + 1, cells.join(", ")));
            } else {
                out.push_str(&format!("  Fila {}: {row}\n", idx + 1));
            }
        }
    }

    // Row count -1 means unknown (bounded COUNT timed out or failed);
    // structure/sample above are still complete, so report unknown explicitly.
    if detail.row_count < 0 {
        out.push_str(
            "\nCOUNT(*): unknown (COUNT capped/timed out — structure above is complete)\n",
        );
    } else {
        out.push_str(&format!("\nCOUNT(*): {}\n", detail.row_count));
    }
    out.push_str(
        "\n→ IMPORTANTE: Esto incluye ESTRUCTURA y MUESTRA. \
         Para más DATOS usa execute_read_query con un SELECT.",
    );
    out
}

pub fn format_invalid_reinject(candidates: &[TableInfo], error_msg: &str) -> String {
    let mut out = format!("❌ Invalid object name: {}\n", error_msg);
    out.push_str("→ La tabla no existe. Usa solo nombres calificados de search_schema.\n");
    out.push_str("Candidatos disponibles (17 máx):\n");
    for t in candidates.iter().take(17) {
        out.push_str(&format!("  • {}.{}\n", t.schema, t.table));
    }
    if candidates.is_empty() {
        out.push_str("  (no hay candidatos visibles)\n");
    }
    out.push_str(
        "→ Corrige el SQL usando un nombre de la lista y reintenta dentro de MAX_STEPS.\n",
    );
    out
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

    #[test]
    fn split_table_defaults_to_dbo() {
        let (schema, table) = split_table_pub("usuarios");
        assert_eq!(schema, "dbo");
        assert_eq!(table, "usuarios");
    }

    #[test]
    fn split_table_with_schema_keeps_schema() {
        let (s, t) = split_table_pub("dbo.Usuario");
        assert_eq!(s, "dbo");
        assert_eq!(t, "Usuario");
        let (s2, t2) = split_table_pub("[dbo].[Usuario]");
        assert_eq!(s2, "dbo");
        assert_eq!(t2, "Usuario");
    }

    #[test]
    fn split_table_matches_shared_helper() {
        // P2: agent split_table must stay equivalent to the shared helper
        // (warn-only difference on missing schema).
        for input in [
            "dbo.Usuario",
            "[dbo].[Usuario]",
            "\"dbo\".\"Usuario\"",
            "usuarios",
            "db.dbo.Usuario",
        ] {
            assert_eq!(
                split_table_pub(input),
                crate::util::split_table_name(input),
                "mismatch for {input}"
            );
        }
    }

    #[test]
    fn format_cell_value_matches_both_row_paths() {
        // P2: the extracted helper must render exactly what both inline
        // matches rendered before the fusion.
        assert_eq!(format_cell_value(&Value::Null), "[NULL]");
        assert_eq!(format_cell_value(&serde_json::json!(42)), "42");
        assert_eq!(format_cell_value(&serde_json::json!("hola")), "hola");
        assert_eq!(format_cell_value(&serde_json::json!(true)), "true");
        assert_eq!(format_cell_value(&serde_json::json!({"a": 1})), "[complex]");
        assert_eq!(format_cell_value(&serde_json::json!([1, 2])), "[complex]");
    }

    #[test]
    fn tool_error_result_reports_name_message_and_retry_hint() {
        let err = anyhow::anyhow!("Falta table");
        let out = tool_error_result("describe_table", &err);
        assert!(
            out.contains("describe_table"),
            "tool name must appear, got: {out}"
        );
        assert!(
            out.contains("Falta table"),
            "error message must appear, got: {out}"
        );
        assert!(
            out.contains("Corregí"),
            "must ask the model to retry, got: {out}"
        );
    }

    #[test]
    fn format_invalid_reinject_contains_candidates() {
        let tables = make_tables(&[("dbo", "Usuario"), ("dbo", "Producto")]);
        let msg = format_invalid_reinject(&tables, "Invalid object name 'dbo.usuarios'");
        assert!(msg.contains("Invalid object name"));
        assert!(msg.contains("dbo.Usuario"));
        assert!(msg.contains("dbo.Producto"));
        assert!(msg.contains("17 máx") || msg.contains("Candidatos"));
    }

    #[test]
    fn format_invalid_reinject_limits_to_17() {
        let tables: Vec<TableInfo> = (0..30)
            .map(|i| TableInfo {
                schema: "dbo".into(),
                table: format!("Tabla{i:02}"),
                table_type: "BASE TABLE".into(),
            })
            .collect();
        let msg = format_invalid_reinject(&tables, "Invalid object name 'dbo.foo'");
        // Should contain only first 17
        assert!(msg.contains("Tabla00"));
        assert!(msg.contains("Tabla16"));
        assert!(!msg.contains("Tabla17"), "should limit to 17 candidates");
    }

    // ===== Task 2.4 discovery formatters (SG-1/SG-2) =====
    use crate::database::schema::{ColumnMatch, ForeignKeyInfo, TableDetail};

    fn sample_tables_with_types() -> Vec<TableInfo> {
        vec![
            TableInfo {
                schema: "dbo".into(),
                table: "Orders".into(),
                table_type: "BASE TABLE".into(),
            },
            TableInfo {
                schema: "dbo".into(),
                table: "VwActive".into(),
                table_type: "VIEW".into(),
            },
        ]
    }

    #[test]
    fn format_table_list_shows_schema_and_type() {
        let out = format_table_list(&sample_tables_with_types());
        assert!(out.contains("dbo.Orders"), "got: {out}");
        assert!(out.contains("BASE TABLE"), "got: {out}");
        assert!(out.contains("dbo.VwActive"), "got: {out}");
        assert!(out.contains("VIEW"), "got: {out}");
    }

    #[test]
    fn format_table_list_empty_warns_without_guessing() {
        let out = format_table_list(&[]);
        assert!(out.contains('0'), "got: {out}");
        assert!(
            out.to_lowercase().contains("no inventes") || out.to_lowercase().contains("no hay"),
            "empty list must warn against inventing names, got: {out}"
        );
    }

    #[test]
    fn format_column_matches_returns_table_column_pairs() {
        let matches = vec![ColumnMatch {
            schema: "dbo".into(),
            table: "Orders".into(),
            column: "email".into(),
            data_type: "nvarchar".into(),
        }];
        let out = format_column_matches(&matches, "email");
        assert!(out.contains("dbo.Orders"), "got: {out}");
        assert!(out.contains("email"), "got: {out}");
    }

    #[test]
    fn format_column_matches_lists_every_deduped_table_column_pair() {
        // Same format input the memory-grouping path reuses for the LLM:
        // every deduped table.column pair must render qualified.
        let dupes = vec![
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
        ];
        let out = format_column_matches(&dupes, "email");
        assert!(out.contains("dbo.Orders.email") && out.contains("dbo.Usuario.email"));
    }

    #[test]
    fn format_column_matches_empty_suggests_discovery() {
        let out = format_column_matches(&[], "zzz_noexiste");
        assert!(out.contains("zzz_noexiste"), "got: {out}");
        assert!(
            out.contains("list_tables"),
            "no-match must point back to discovery, got: {out}"
        );
    }

    fn sample_detail() -> (String, String, TableDetail) {
        use serde_json::json;
        (
            "dbo".into(),
            "Orders".into(),
            TableDetail {
                columns: vec![crate::database::ColumnInfo {
                    column: "id".into(),
                    data_type: "int".into(),
                    nullable: false,
                    ordinal: 1,
                }],
                primary_keys: vec!["id".into()],
                foreign_keys: vec![ForeignKeyInfo {
                    column: "user_id".into(),
                    ref_schema: "dbo".into(),
                    ref_table: "Usuario".into(),
                    ref_column: "id".into(),
                }],
                view_definition: None,
                sample_rows: vec![json!({"id": 1})],
                row_count: 42,
            },
        )
    }

    #[test]
    fn format_table_detail_full_table_sections() {
        let (s, t, d) = sample_detail();
        let out = format_table_detail(&s, &t, &d);
        assert!(out.contains("ESTRUCTURA"), "got: {out}");
        assert!(
            out.contains("PRIMARY KEY") || out.contains("id"),
            "got: {out}"
        );
        assert!(
            out.contains("dbo.Usuario"),
            "FK target must appear, got: {out}"
        );
        assert!(out.contains("42"), "COUNT(*) must appear, got: {out}");
        assert!(out.contains("MUESTRA"), "got: {out}");
    }

    #[test]
    fn format_table_detail_view_and_empty() {
        let d = TableDetail {
            columns: vec![],
            primary_keys: vec![],
            foreign_keys: vec![],
            view_definition: Some("SELECT id FROM dbo.Orders".into()),
            sample_rows: vec![],
            row_count: 0,
        };
        let out = format_table_detail("dbo", "VwEmpty", &d);
        assert!(out.contains("VIEW"), "view marker must appear, got: {out}");
        assert!(
            out.contains("SELECT id FROM dbo.Orders"),
            "view definition must appear, got: {out}"
        );
        assert!(out.contains('0'), "zero count must appear, got: {out}");
    }

    #[test]
    fn format_table_detail_none_definition_shows_permissions_hint() {
        // Views without VIEW DEFINITION grant arrive with None; the formatter
        // must hint instead of staying silent.
        let d = TableDetail {
            columns: vec![],
            primary_keys: vec![],
            foreign_keys: vec![],
            view_definition: None,
            sample_rows: vec![],
            row_count: 10,
        };
        let out = format_table_detail("dbo", "VwHidden", &d);
        assert!(
            out.contains("definition unavailable (permissions)"),
            "missing definition must hint permissions, got: {out}"
        );
    }

    #[test]
    fn format_table_detail_unknown_count_still_shows_structure() {
        // Bounded COUNT timeout yields row_count -1; structure must survive.
        let (s, t, mut d) = sample_detail();
        d.row_count = -1;
        let out = format_table_detail(&s, &t, &d);
        assert!(
            out.contains("ESTRUCTURA"),
            "structure must survive, got: {out}"
        );
        assert!(
            out.contains("MUESTRA"),
            "sample section must survive, got: {out}"
        );
        assert!(
            out.contains("unknown"),
            "unknown count must be explicit, got: {out}"
        );
    }

    // ===== slice-1a: render redaction (RED) =====

    #[test]
    fn format_query_result_redacts_sensitive_header() {
        // Residual leak at render must show [REDACTED], safe cols stay visible.
        let rows = vec![serde_json::json!({"id": 1, "password": "secret123"})];
        let out = format_query_result(&rows, false);
        assert!(
            out.contains("[REDACTED]"),
            "sensitive value must be redacted, got: {out}"
        );
        assert!(
            !out.contains("secret123"),
            "raw sensitive value must never leak, got: {out}"
        );
        assert!(
            out.contains('1'),
            "safe value must stay visible, got: {out}"
        );
    }

    #[test]
    fn format_query_result_redacts_token_header_case_insensitive() {
        // Triangulation: different header casing and column (TOKEN family).
        let rows = vec![serde_json::json!({"MY_TOKEN": "abc", "name": "ana"})];
        let out = format_query_result(&rows, false);
        assert!(
            out.contains("[REDACTED]"),
            "token header must be redacted, got: {out}"
        );
        assert!(
            !out.contains("abc"),
            "raw token must never leak, got: {out}"
        );
        assert!(
            out.contains("ana"),
            "safe value must stay visible, got: {out}"
        );
    }

    #[test]
    fn format_query_result_inlines_columns_per_row() {
        let rows = vec![
            serde_json::json!({"id": 1, "estado": "activo"}),
            serde_json::json!({"id": 2, "estado": "pendiente"}),
        ];
        let out = format_query_result(&rows, false);
        // One line per row (all columns inline), not one line per column.
        let fila_lines = out.lines().filter(|l| l.starts_with("Fila")).count();
        assert_eq!(fila_lines, 2, "must inline columns per row, got: {out}");
        assert!(out.contains("id=1"), "got: {out}");
        assert!(out.contains("estado=activo"), "got: {out}");
    }

    #[test]
    fn extract_sql_detects_plain_and_fenced() {
        assert_eq!(
            extract_sql("SELECT * FROM dbo.T"),
            Some("SELECT * FROM dbo.T".to_string())
        );
        assert_eq!(
            extract_sql("```sql\nSELECT 1\n```"),
            Some("SELECT 1".to_string())
        );
        assert_eq!(
            extract_sql("```\nWITH x AS (SELECT 1) SELECT * FROM x\n```"),
            Some("WITH x AS (SELECT 1) SELECT * FROM x".to_string())
        );
        assert_eq!(extract_sql("Hola, cómo estás"), None);
        assert_eq!(extract_sql("DELETE FROM dbo.T"), None);
    }

    #[test]
    fn looks_like_sql_matches_extract_sql() {
        assert!(looks_like_sql("SELECT 1"));
        assert!(looks_like_sql("```sql\nSELECT 1\n```"));
        assert!(!looks_like_sql("hola"));
        assert!(!looks_like_sql("```sql\nUPDATE t SET x=1\n```"));
    }

    #[test]
    fn format_distinct_values_lists_values() {
        let rows = vec![
            serde_json::json!({"estado": "activo"}),
            serde_json::json!({"estado": "pendiente"}),
        ];
        let out = format_distinct_values(&rows, "estado");
        assert!(out.contains("VALORES DISTINTOS"), "got: {out}");
        assert!(
            out.contains("activo") && out.contains("pendiente"),
            "got: {out}"
        );
        assert!(
            out.contains("WHERE"),
            "must hint WHERE filtering, got: {out}"
        );
    }

    #[test]
    fn format_table_detail_redacts_sensitive_sample() {
        // Describe sample reaching the formatter with a sensitive key
        // must render [REDACTED] instead of the raw value.
        let d = TableDetail {
            columns: vec![],
            primary_keys: vec![],
            foreign_keys: vec![],
            view_definition: None,
            sample_rows: vec![serde_json::json!({"id": 1, "api_key": "k-123"})],
            row_count: 1,
        };
        let out = format_table_detail("dbo", "Users", &d);
        assert!(
            out.contains("[REDACTED]"),
            "sensitive sample value must be redacted, got: {out}"
        );
        assert!(
            !out.contains("k-123"),
            "raw sensitive sample must never leak, got: {out}"
        );
    }
}
