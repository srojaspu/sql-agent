use crate::agent::Session;

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

/// Chat line displayed in TUI
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatLine {
    pub role: String,
    pub content: String,
}

impl ChatLine {
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
        }
    }
}

/// Application state held in TUI. Wraps PR2 Session and adds UI concerns.
#[derive(Debug)]
pub struct AppState {
    pub session: Session,
    pub input: String,
    pub cursor_index: usize,
    pub messages: Vec<ChatLine>,
    /// Traza interna de herramientas — no se muestra en el chat, solo en status/debug.
    pub tool_trace: Vec<ChatLine>,
    pub scroll_offset: usize,
    pub status: String,
    pub current_tool: Option<String>,
    pub current_step: usize,
    pub is_loading: bool,
    pub show_help: bool,
    pub history_visible: bool,
    /// Monotonic render epoch, bumped on every visible mutation.
    /// The TUI loop draws only when this differs from the last drawn value,
    /// so idle frames without events skip the rebuild entirely.
    pub version: u64,
}

impl AppState {
    pub fn new(session: Session) -> Self {
        Self {
            session,
            input: String::new(),
            cursor_index: 0,
            messages: Vec::new(),
            tool_trace: Vec::new(),
            scroll_offset: 0,
            status: "Listo — escribe tu pregunta o /help".to_string(),
            current_tool: None,
            current_step: 0,
            is_loading: false,
            show_help: false,
            history_visible: false,
            version: 0,
        }
    }

    pub(super) fn bump(&mut self) {
        self.version = self.version.wrapping_add(1);
    }

    pub fn push_line(&mut self, role: impl Into<String>, content: impl Into<String>) {
        self.messages.push(ChatLine::new(role, content));
        self.bump();
    }

    pub fn push_user(&mut self, content: impl Into<String>) {
        self.push_line("user", content);
    }

    pub fn push_assistant(&mut self, content: impl Into<String>) {
        self.push_line("assistant", content);
    }

    pub fn push_tool(&mut self, name: &str, content: impl Into<String>) {
        // Compat: mantiene el método pero la traza interna ya no contamina `messages`.
        // Se conserva para tests/compatibilidad; el flujo normal usa `tool_trace`.
        let truncated: String = content.into().chars().take(200).collect();
        self.tool_trace
            .push(ChatLine::new(format!("tool:{name}"), truncated));
        self.bump();
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.tool_trace.clear();
        self.cursor_index = 0;
        self.scroll_offset = 0;
        self.status = "Historial limpiado".to_string();
        self.current_tool = None;
        self.current_step = 0;
        self.bump();
    }

    pub fn set_status(&mut self, s: impl Into<String>) {
        self.status = s.into();
        self.bump();
    }

    pub fn set_tool(&mut self, step: usize, tool: impl Into<String>) {
        self.current_step = step;
        self.current_tool = Some(tool.into());
        if let Some(t) = &self.current_tool {
            self.status = format!("Paso {step} — {t}");
        }
        self.is_loading = true;
        self.bump();
    }

    pub fn set_done(&mut self, result: impl Into<String>) {
        let r = result.into();
        self.push_assistant(r.clone());
        if self.session.messages.last().map(|m| &m.content) != Some(&r) {
            self.session.push(crate::llm::Message {
                role: "assistant".into(),
                content: r,
                tool_calls: vec![],
                name: None,
                tool_call_id: None,
            });
        }
        self.status = "Completado".to_string();
        self.current_tool = None;
        self.is_loading = false;
        self.scroll_offset = 0; // follow bottom on done
        self.bump();
    }

    pub fn set_error(&mut self, err: impl Into<String>) {
        let e = err.into();
        self.push_line("error", e.clone());
        self.status = format!("Error: {e}");
        self.is_loading = false;
        self.bump();
    }

    pub fn scroll_limit(&self) -> usize {
        let line_count: usize = self
            .messages
            .iter()
            .map(|m| m.content.split('\n').count().max(1))
            .sum();
        line_count.max(self.messages.len())
    }

    // Scroll: offset = lines scrolled up from bottom (0 = bottom)
    pub fn scroll_up(&mut self) {
        let max = self.scroll_limit();
        if self.scroll_offset < max {
            self.scroll_offset += 1;
            self.bump();
        }
    }

    pub fn scroll_down(&mut self) {
        if self.scroll_offset > 0 {
            self.scroll_offset -= 1;
            self.bump();
        }
    }

    pub fn scroll_page_up(&mut self) {
        let max = self.scroll_limit();
        let next = (self.scroll_offset + 10).min(max);
        if next != self.scroll_offset {
            self.scroll_offset = next;
            self.bump();
        }
    }

    pub fn scroll_page_down(&mut self) {
        let next = self.scroll_offset.saturating_sub(10);
        if next != self.scroll_offset {
            self.scroll_offset = next;
            self.bump();
        }
    }

    pub fn scroll_to_bottom(&mut self) {
        if self.scroll_offset != 0 {
            self.scroll_offset = 0;
            self.bump();
        }
    }

    pub fn toggle_history(&mut self) {
        self.history_visible = !self.history_visible;
        if self.history_visible {
            self.status = format!("Historial: {} mensajes", self.session.messages.len());
        } else {
            self.status = "Vista chat".to_string();
        }
        self.bump();
    }

    pub fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
        self.bump();
    }

    /// Visible messages given scroll_offset (bottom-anchored)
    pub fn visible_messages(&self, height: usize) -> &[ChatLine] {
        if self.messages.is_empty() || height == 0 {
            return &[];
        }
        let total = self.messages.len();
        if self.scroll_offset == 0 {
            let start = total.saturating_sub(height);
            &self.messages[start..]
        } else {
            // scroll_offset counts lines up from bottom
            let end = total.saturating_sub(self.scroll_offset);
            let start = end.saturating_sub(height);
            &self.messages[start..end]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Session;

    fn make_state() -> AppState {
        AppState::new(Session::new())
    }

    #[test]
    fn app_new_holds_session_and_defaults() {
        let s = Session::new();
        let id = s.id.clone();
        let app = AppState::new(s);
        assert_eq!(app.session.id, id);
        assert!(app.messages.is_empty());
        assert_eq!(app.scroll_offset, 0);
        assert!(!app.is_loading);
        assert_eq!(app.status, "Listo — escribe tu pregunta o /help");
        // second case: new session should have empty messages
        let app2 = make_state();
        assert_eq!(app2.session.messages.len(), 0);
    }

    #[test]
    fn app_push_and_clear_resets_scroll() {
        let mut app = make_state();
        app.push_user("hola");
        app.push_assistant("respuesta");
        assert_eq!(app.messages.len(), 2);
        app.scroll_up();
        assert_eq!(app.scroll_offset, 1);
        app.clear();
        assert!(app.messages.is_empty(), "clear should empty chat");
        assert_eq!(app.scroll_offset, 0, "clear resets scroll");
        assert_eq!(app.status, "Historial limpiado");
        // triangulation: clear on already empty
        app.clear();
        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn app_scroll_up_down_bounds() {
        let mut app = make_state();
        for i in 0..5 {
            app.push_user(format!("msg {i}"));
        }
        assert_eq!(app.scroll_offset, 0);
        app.scroll_up();
        assert_eq!(app.scroll_offset, 1);
        app.scroll_page_up();
        // page up +10 but capped at len 5
        assert_eq!(app.scroll_offset, 5, "should cap at len");
        // further up stays capped
        app.scroll_up();
        assert_eq!(app.scroll_offset, 5);
        app.scroll_down();
        assert_eq!(app.scroll_offset, 4);
        app.scroll_page_down();
        assert_eq!(app.scroll_offset, 0, "page down should clamp to 0");
        app.scroll_down();
        assert_eq!(app.scroll_offset, 0, "stay at 0");
    }

    #[test]
    fn app_history_toggle_and_help() {
        let mut app = make_state();
        assert!(!app.history_visible);
        app.toggle_history();
        assert!(app.history_visible);
        assert!(app.status.contains("Historial"));
        app.toggle_history();
        assert!(!app.history_visible);
        // help toggle triangulation
        assert!(!app.show_help);
        app.toggle_help();
        assert!(app.show_help);
        app.toggle_help();
        assert!(!app.show_help);
    }

    #[test]
    fn app_visible_messages_respects_scroll() {
        let mut app = make_state();
        for i in 0..10 {
            app.push_user(format!("msg {i}"));
        }
        // bottom 3
        let vis = app.visible_messages(3);
        assert_eq!(vis.len(), 3);
        assert_eq!(vis[0].content, "msg 7");
        assert_eq!(vis[2].content, "msg 9");
        // scroll up 2
        app.scroll_up();
        app.scroll_up();
        let vis2 = app.visible_messages(3);
        assert_eq!(vis2[0].content, "msg 5");
        assert_eq!(vis2[2].content, "msg 7");
        // page scenario — reset to bottom first
        app.scroll_down();
        app.scroll_down();
        let vis3 = app.visible_messages(20);
        assert_eq!(vis3.len(), 10, "height larger than total returns all");
    }

    // ===== S4: export_messages + csv_escape tests =====

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
        assert_eq!(parsed[3]["content"], "code fence:\n```\nSELECT * FROM t\n```");
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
