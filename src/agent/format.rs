//! LLM-facing text formatters plus small text helpers (pure domain logic).
//!
//! Extracted verbatim from `agent::core` (slice D, step 1): table/column/result
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

pub(crate) fn split_table(s: &str) -> (String, String) {
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

pub(crate) fn limit_text(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }

    let mut end = max.min(text.len());

    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }

    format!("{}...[resultado recortado]", &text[..end])
}

/*
 * ====================================================================
 * SQL DETECTION
 * ====================================================================
 */

pub(crate) fn looks_like_sql(s: &str) -> bool {
    let t = s.trim_start().to_ascii_uppercase();

    t.starts_with("SELECT ") || t == "SELECT" || t.starts_with("WITH ") || t == "WITH"
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
            formatted.push_str(&format!("Fila {}:\n", idx + 1));
            if let Some(obj) = row.as_object() {
                for (key, val) in obj {
                    let display_val = if is_sensitive_column(key) {
                        "[REDACTED]".to_string()
                    } else {
                        format_cell_value(val)
                    };
                    formatted.push_str(&format!("  {key} = {display_val}\n"));
                }
            } else {
                formatted.push_str(&format!("  {row}\n"));
            }
            formatted.push('\n');
        }
    }
    if truncated {
        formatted.push_str("\n[Nota: Resultado limitado al máximo configurado]\n");
    }
    formatted
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

/// Format history for /history command - shows readable user/agent pairs instead of raw tool messages.
pub fn format_history_readable(session: &crate::agent::session::Session) -> String {
    let mut out = Vec::new();
    let mut i = 0;
    while i < session.messages.len() {
        let msg = &session.messages[i];
        match msg.role.as_str() {
            "user" => {
                out.push(format!("👤 Usuario: {}", msg.content));
                i += 1;
            }
            "assistant" => {
                if !msg.tool_calls.is_empty() {
                    // This is a tool call message - show tool calls and skip to results
                    let tool_names: Vec<String> = msg
                        .tool_calls
                        .iter()
                        .map(|c| c.function.name.clone())
                        .collect();
                    out.push(format!("🤖 Agente → herramientas: {}", tool_names.join(", ")));
                    // Skip tool result messages
                    i += 1;
                    while i < session.messages.len() && session.messages[i].role == "tool" {
                        i += 1;
                    }
                } else {
                    // Regular assistant response
                    out.push(format!("🤖 Agente: {}", msg.content));
                    i += 1;
                }
            }
            "tool" => {
                // Standalone tool message (shouldn't happen in normal flow, but handle gracefully)
                out.push(format!("🔧 Herramienta: {}", msg.content));
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
    if out.is_empty() {
        "Historial vacío".to_string()
    } else {
        out.join("\n")
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

    #[test]
    fn format_history_readable_pairs_tool_calls_with_results() {
        use crate::agent::session::Session;
        use crate::llm::{Message, ToolCall, ToolFunction};

        let mut session = Session::new();
        // User question
        session.push(Message::user("cuantos usuarios hay".into()));
        // Assistant tool calls (first)
        session.push(Message {
            role: "assistant".into(),
            content: "".into(),
            tool_calls: vec![ToolCall {
                id: Some("call_1".into()),
                function: ToolFunction {
                    name: "search_schema".into(),
                    arguments: serde_json::json!({"query": "usuarios"}),
                },
            }],
            name: None,
            tool_call_id: None,
        });
        // Tool result
        session.push(Message::tool_with_call_id("search_schema", "✓ TABLAS ENCONTRADAS: 1\n  • dbo.Usuario".into(), Some("call_1".into())));
        // Assistant tool calls (second)
        session.push(Message {
            role: "assistant".into(),
            content: "".into(),
            tool_calls: vec![ToolCall {
                id: Some("call_2".into()),
                function: ToolFunction {
                    name: "describe_table".into(),
                    arguments: serde_json::json!({"table": "dbo.Usuario"}),
                },
            }],
            name: None,
            tool_call_id: None,
        });
        // Tool result
        session.push(Message::tool_with_call_id("describe_table", "✓ ESTRUCTURA DE dbo.Usuario...".into(), Some("call_2".into())));
        // Final assistant response
        session.push(Message::assistant("Hay 1 tabla: dbo.Usuario".into()));

        let formatted = format_history_readable(&session);
        // Should show user, agent with tools, and final response
        assert!(formatted.contains("👤 Usuario: cuantos usuarios hay"));
        // Each tool call message gets its own line
        assert!(formatted.contains("🤖 Agente → herramientas: search_schema"));
        assert!(formatted.contains("🤖 Agente → herramientas: describe_table"));
        assert!(formatted.contains("🤖 Agente: Hay 1 tabla: dbo.Usuario"));
        // Tool results should NOT appear in readable format
        assert!(!formatted.contains("TABLAS ENCONTRADAS"));
        assert!(!formatted.contains("ESTRUCTURA DE dbo.Usuario"));
    }

    #[test]
    fn format_history_readable_empty_session() {
        use crate::agent::session::Session;
        let session = Session::new();
        assert_eq!(format_history_readable(&session), "Historial vacío");
    }
}
