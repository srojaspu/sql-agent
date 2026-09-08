//! History formatting for TUI.
//!
//! Extracted from `agent::format` to keep history formatting separate.

use crate::agent::session::Session;

/// Format history for /history command - shows readable user/agent pairs instead of raw tool messages.
pub fn format_history_readable(session: &Session) -> String {
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
                    out.push(format!(
                        "🤖 Agente → herramientas: {}",
                        tool_names.join(", ")
                    ));
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
    use crate::agent::session::Session;
    use crate::llm::{Message, ToolCall, ToolFunction};

    #[test]
    fn format_history_readable_pairs_tool_calls_with_results() {
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
        session.push(Message::tool_with_call_id(
            "search_schema",
            "✓ TABLAS ENCONTRADAS: 1\n  • dbo.Usuario".into(),
            Some("call_1".into()),
        ));
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
        session.push(Message::tool_with_call_id(
            "describe_table",
            "✓ ESTRUCTURA DE dbo.Usuario...".into(),
            Some("call_2".into()),
        ));
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
        let session = Session::new();
        assert_eq!(format_history_readable(&session), "Historial vacío");
    }
}
