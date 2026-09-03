use crate::agent::Session;
use crate::tui::event::AppEvent;
use tokio::sync::mpsc;

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

    fn bump(&mut self) {
        self.version = self.version.wrapping_add(1);
    }

    pub fn push_line(&mut self, role: impl Into<String>, content: impl Into<String>) {
        self.messages.push(ChatLine::new(role, content));
        self.bump();
        // Auto-follow if at bottom; otherwise keep offset (user scrolled up)
        if self.scroll_offset == 0 {
            // stay at bottom — nothing to do
        } else {
            // keep scroll position stable relative to bottom: increase offset to stay at same historic position?
            // For simplicity, leave offset unchanged — user must scroll down manually
        }
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

    /// Version-bumping input helpers so the render loop can dirty-check
    /// keystrokes without polling the string every frame.
    pub fn push_input(&mut self, c: char) {
        self.input.push(c);
        self.bump();
    }

    pub fn pop_input(&mut self) {
        if self.input.pop().is_some() {
            self.bump();
        }
    }

    pub fn clear_input(&mut self) {
        if !self.input.is_empty() {
            self.input.clear();
            self.bump();
        }
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.tool_trace.clear();
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
        self.status = format!("Paso {step} — {}", self.current_tool.as_ref().unwrap());
        self.is_loading = true;
        self.bump();
    }

    pub fn set_done(&mut self, result: impl Into<String>) {
        let r = result.into();
        self.push_assistant(r.clone());
        self.status = "Completado".to_string();
        self.current_tool = None;
        self.is_loading = false;
        self.scroll_offset = 0; // follow bottom on done
                                // push_assistant already bumped; status/is_loading changed too.
        self.bump();
    }

    pub fn set_error(&mut self, err: impl Into<String>) {
        let e = err.into();
        self.push_line("error", e.clone());
        self.status = format!("Error: {e}");
        self.is_loading = false;
        // push_line already bumped; status changed too.
        self.bump();
    }

    // Scroll: offset = lines scrolled up from bottom (0 = bottom)
    pub fn scroll_up(&mut self) {
        let max = self.messages.len();
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
        let max = self.messages.len();
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

    /// Handle an AppEvent from mpsc channel, updating state accordingly.
    pub fn handle_event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Input(s) => {
                self.push_user(s);
                self.is_loading = true;
                self.status = "Enviando...".to_string();
            }
            AppEvent::AgentStep { step, tool } => {
                self.set_tool(step, tool);
            }
            AppEvent::AgentTool { name, content } => {
                // No contaminar el chat: la traza va a `tool_trace` y al status.
                let truncated: String = content.chars().take(200).collect();
                self.tool_trace
                    .push(ChatLine::new(format!("tool:{name}"), truncated));
                self.current_tool = Some(name.clone());
                self.status = format!("Herramienta {name} completada");
                self.bump();
            }
            AppEvent::AgentDone(result) => {
                self.set_done(result);
            }
            AppEvent::Error(e) => {
                self.set_error(e);
            }
            AppEvent::Quit => {
                self.status = "Saliendo...".to_string();
                self.bump();
            }
        }
    }

    /// Handle slash commands typed in input bar. Returns true if handled as command,
    /// false if it was a normal message.
    pub fn handle_command(&mut self, input: &str) -> bool {
        let trimmed = input.trim();
        match trimmed {
            "/clear" => {
                self.clear();
                true
            }
            "/history" => {
                self.toggle_history();
                true
            }
            "/help" | "/?" => {
                self.toggle_help();
                true
            }
            "/quit" | "/exit" | "/q" => {
                self.status = "Saliendo...".to_string();
                true
            }
            s if s.starts_with("/describe ") => {
                let table = s.trim_start_matches("/describe ").trim();
                self.status = format!("Describiendo {table}...");
                self.is_loading = true;
                true
            }
            "/tables" => {
                self.status = "Listando tablas...".to_string();
                self.is_loading = true;
                true
            }
            "/refresh" => {
                self.status = "Refrescando esquema...".to_string();
                self.is_loading = true;
                true
            }
            _ if trimmed.starts_with('/') => {
                self.status = format!("Comando desconocido: {trimmed} — usa /help");
                true
            }
            _ => false,
        }
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

    /// Create a bounded mpsc channel for Agent→UI events
    pub fn channel(buffer: usize) -> (mpsc::Sender<AppEvent>, mpsc::Receiver<AppEvent>) {
        mpsc::channel(buffer)
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
    fn app_status_tool_trace_shows_step_and_tool() {
        let mut app = make_state();
        app.handle_event(AppEvent::AgentStep {
            step: 2,
            tool: "search_schema".to_string(),
        });
        assert_eq!(app.current_step, 2);
        assert_eq!(app.current_tool.as_deref(), Some("search_schema"));
        assert!(app.status.contains("Paso 2"));
        assert!(app.status.contains("search_schema"));
        assert!(app.is_loading);
        // second step
        app.handle_event(AppEvent::AgentStep {
            step: 3,
            tool: "execute_read_query".to_string(),
        });
        assert_eq!(app.current_step, 3);
        assert_eq!(app.current_tool.as_deref(), Some("execute_read_query"));
        // done resets
        app.handle_event(AppEvent::AgentDone("resultado final".into()));
        assert!(!app.is_loading);
        assert_eq!(app.status, "Completado");
        assert!(app.messages.iter().any(|m| m.content == "resultado final"));
    }

    #[test]
    fn app_handle_input_and_tool_events_via_mpsc_logic() {
        let mut app = make_state();
        app.handle_event(AppEvent::Input("cuantos usuarios hay".into()));
        assert!(app
            .messages
            .iter()
            .any(|m| m.content == "cuantos usuarios hay"));
        assert!(app.is_loading);
        app.handle_event(AppEvent::AgentTool {
            name: "search_schema".into(),
            content: "✓ TABLAS ENCONTRADAS: 1".into(),
        });
        // Chat must stay clean — no tool lines in `messages`
        assert!(!app.messages.iter().any(|m| m.role == "tool:search_schema"));
        assert!(app.status.contains("search_schema"));
        assert_eq!(app.tool_trace.len(), 1);
        assert_eq!(app.tool_trace[0].role, "tool:search_schema");
        // content truncated to 200 chars
        assert!(app.tool_trace[0].content.contains("TABLAS ENCONTRADAS"));
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
    fn app_handle_command_clear_and_unknown() {
        let mut app = make_state();
        app.push_user("msg");
        let handled = app.handle_command("/clear");
        assert!(handled);
        assert!(app.messages.is_empty());
        let handled2 = app.handle_command("/history");
        assert!(handled2);
        assert!(app.history_visible);
        let handled3 = app.handle_command("/unknowncmd");
        assert!(handled3);
        assert!(app.status.contains("Comando desconocido"));
        let not_cmd = app.handle_command("pregunta normal");
        assert!(!not_cmd, "normal text should not be treated as command");
        // triangulation: /tables and /describe
        let h = app.handle_command("/tables");
        assert!(h);
        assert!(app.status.contains("Listando"));
        let hd = app.handle_command("/describe dbo.Usuario");
        assert!(hd);
        assert!(app.status.contains("Describiendo"));
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

    #[tokio::test]
    async fn app_mpsc_channel_agent_to_ui() {
        let (tx, mut rx) = AppState::channel(8);
        let mut app = make_state();
        // Spawn agent-like task sending events
        let tx2 = tx.clone();
        tokio::spawn(async move {
            tx2.send(AppEvent::AgentStep {
                step: 1,
                tool: "search_schema".into(),
            })
            .await
            .unwrap();
            tx2.send(AppEvent::AgentTool {
                name: "search_schema".into(),
                content: "ok".into(),
            })
            .await
            .unwrap();
            tx2.send(AppEvent::AgentDone("done!".into())).await.unwrap();
        });
        // Receive and handle
        while let Some(ev) = rx.recv().await {
            let is_done = matches!(ev, AppEvent::AgentDone(_));
            app.handle_event(ev);
            if is_done {
                break;
            }
        }
        assert_eq!(app.current_step, 1);
        assert!(app.messages.iter().any(|m| m.content == "done!"));
        assert!(!app.is_loading);
    }

    #[tokio::test]
    async fn app_mpsc_error_and_quit() {
        let (tx, mut rx) = AppState::channel(4);
        let mut app = make_state();
        tx.send(AppEvent::Error("boom".into())).await.unwrap();
        let ev = rx.recv().await.unwrap();
        app.handle_event(ev);
        assert!(app.status.contains("Error"));
        assert!(app.messages.iter().any(|m| m.role == "error"));
        tx.send(AppEvent::Quit).await.unwrap();
        let ev2 = rx.recv().await.unwrap();
        app.handle_event(ev2);
        assert_eq!(app.status, "Saliendo...");
    }
}
