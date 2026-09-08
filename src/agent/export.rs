//! CSV/JSON export utilities for chat history.
//!
//! Extracted from `agent::format` to keep export logic separate.

use crate::tui::state::ChatLine;

/// Escape a field for CSV: wrap in quotes if it contains comma, quote, or newline.
/// Double any existing quotes.
pub fn csv_escape(s: &str) -> String {
    let needs_quotes = s.contains(',') || s.contains('"') || s.contains('\n');
    let escaped = s.replace('"', "\"\"");
    if needs_quotes {
        format!("\"{escaped}\"")
    } else {
        escaped
    }
}

/// Export chat messages to CSV format.
/// Returns CSV string with header and rows.
/// Each message becomes a row with: role,content
pub fn export_messages_csv(messages: &[ChatLine]) -> String {
    let mut out = String::new();
    out.push_str("role,content\n");
    for msg in messages {
        out.push_str(&format!("{},{}\n", msg.role, csv_escape(&msg.content)));
    }
    out
}

/// Export chat messages to JSON format.
/// Returns JSON array of objects with role and content.
pub fn export_messages_json(messages: &[ChatLine]) -> String {
    let json_messages: Vec<serde_json::Value> = messages
        .iter()
        .map(|m| {
            serde_json::json!({
                "role": m.role,
                "content": m.content
            })
        })
        .collect();
    serde_json::to_string_pretty(&json_messages).unwrap_or_else(|_| "[]".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_escape_no_special_chars() {
        assert_eq!(csv_escape("hola"), "hola");
        assert_eq!(csv_escape("mundo"), "mundo");
        assert_eq!(csv_escape("simple text"), "simple text");
    }

    #[test]
    fn csv_escape_comma_wraps_in_quotes() {
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
        assert_eq!(csv_escape("col1,col2,col3"), "\"col1,col2,col3\"");
    }

    #[test]
    fn csv_escape_quotes_doubled_and_wrapped() {
        assert_eq!(csv_escape("he said \"hello\""), "\"he said \"\"hello\"\"\"");
        assert_eq!(csv_escape("\"quoted\""), "\"\"\"quoted\"\"\"");
    }

    #[test]
    fn csv_escape_newline_wraps_in_quotes() {
        assert_eq!(csv_escape("line1\nline2"), "\"line1\nline2\"");
        assert_eq!(csv_escape("a\nb\nc"), "\"a\nb\nc\"");
    }

    #[test]
    fn csv_escape_comma_quote_newline_all() {
        let input = "a,\"b\"\nc";
        let escaped = csv_escape(input);
        assert!(escaped.starts_with('"') && escaped.ends_with('"'));
        assert!(escaped.contains("\"\""));
        assert!(escaped.contains("\n"));
    }

    #[test]
    fn export_messages_csv_basic() {
        let msgs = vec![
            ChatLine::new("user", "hola"),
            ChatLine::new("assistant", "¿en qué puedo ayudarte?"),
            ChatLine::new("user", "cuantos usuarios hay"),
        ];
        let csv = export_messages_csv(&msgs);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines[0], "role,content");
        assert_eq!(lines[1], "user,hola");
        assert_eq!(lines[2], "assistant,¿en qué puedo ayudarte?");
        assert_eq!(lines[3], "user,cuantos usuarios hay");
    }

    #[test]
    fn export_messages_csv_escapes_special_chars() {
        let msgs = vec![
            ChatLine::new("user", "a,b"),
            ChatLine::new("assistant", "he said \"hello\""),
            ChatLine::new("user", "line1\nline2"),
        ];
        let csv = export_messages_csv(&msgs);
        // CSV with multiline: split by lines but note that multiline content
        // creates multiple lines in the output. We verify the structure differently.
        assert!(csv.starts_with("role,content\n"));
        assert!(csv.contains("user,\"a,b\""));
        assert!(csv.contains("assistant,\"he said \"\"hello\"\"\""));
        // The multiline content will be split across lines in the CSV string
        assert!(csv.contains("user,\"line1"));
        assert!(csv.contains("line2\""));
    }

    #[test]
    fn export_messages_json_basic() {
        let msgs = vec![
            ChatLine::new("user", "hola"),
            ChatLine::new("assistant", "¿en qué puedo ayudarte?"),
        ];
        let json = export_messages_json(&msgs);
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert!(parsed.is_array());
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["role"], "user");
        assert_eq!(arr[0]["content"], "hola");
        assert_eq!(arr[1]["role"], "assistant");
        assert_eq!(arr[1]["content"], "¿en qué puedo ayudarte?");
    }

    #[test]
    fn export_messages_json_roundtrip_preserves_content() {
        let msgs = vec![
            ChatLine::new("user", "a,b"),
            ChatLine::new("assistant", "he said \"hello\""),
            ChatLine::new("user", "line1\nline2"),
            ChatLine::new("assistant", "code fence:\n```\nSELECT * FROM t\n```"),
        ];
        let json = export_messages_json(&msgs);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed.len(), 4);
        // content must match exactly (no split('|') parsing)
        assert_eq!(parsed[0]["content"], "a,b");
        assert_eq!(parsed[1]["content"], "he said \"hello\"");
        assert_eq!(parsed[2]["content"], "line1\nline2");
        assert_eq!(
            parsed[3]["content"],
            "code fence:\n```\nSELECT * FROM t\n```"
        );
    }

    #[test]
    fn export_messages_json_then_csv_roundtrip() {
        let msgs = vec![
            ChatLine::new("user", "test"),
            ChatLine::new("assistant", "response"),
        ];
        // JSON -> parse -> CSV
        let json = export_messages_json(&msgs);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        let rec_msgs: Vec<ChatLine> = parsed
            .into_iter()
            .map(|v| ChatLine::new(v["role"].as_str().unwrap(), v["content"].as_str().unwrap()))
            .collect();
        let csv = export_messages_csv(&rec_msgs);
        assert!(csv.contains("user,test"));
        assert!(csv.contains("assistant,response"));
    }

    #[test]
    fn export_messages_empty() {
        let csv = export_messages_csv(&[]);
        assert_eq!(csv, "role,content\n");
        let json = export_messages_json(&[]);
        assert_eq!(json, "[]");
    }
}
