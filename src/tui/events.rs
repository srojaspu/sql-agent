use super::state::{AppState, ChatLine};
use crate::agent::Session;
use tokio::sync::mpsc;

/// TUI events — Agent→UI and UI internal
#[derive(Clone, Debug)]
pub enum AppEvent {
    Input(String),
    AgentStep { step: usize, tool: String },
    AgentTool { name: String, content: String },
    AgentDone(String),
    SessionUpdate(Box<Session>),
    Error(String),
    Quit,
}

impl PartialEq for AppEvent {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (AppEvent::Input(a), AppEvent::Input(b)) => a == b,
            (
                AppEvent::AgentStep { step: s1, tool: t1 },
                AppEvent::AgentStep { step: s2, tool: t2 },
            ) => s1 == s2 && t1 == t2,
            (
                AppEvent::AgentTool {
                    name: n1,
                    content: c1,
                },
                AppEvent::AgentTool {
                    name: n2,
                    content: c2,
                },
            ) => n1 == n2 && c1 == c2,
            (AppEvent::AgentDone(a), AppEvent::AgentDone(b)) => a == b,
            (AppEvent::SessionUpdate(a), AppEvent::SessionUpdate(b)) => a.id == b.id,
            (AppEvent::Error(a), AppEvent::Error(b)) => a == b,
            (AppEvent::Quit, AppEvent::Quit) => true,
            _ => false,
        }
    }
}

impl Eq for AppEvent {}

impl AppState {
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
            AppEvent::SessionUpdate(sess) => {
                self.session = *sess;
                self.bump();
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
    fn event_input_creates() {
        let e = AppEvent::Input("hola".into());
        assert_eq!(e, AppEvent::Input("hola".into()));
    }

    #[test]
    fn event_agent_step_and_tool() {
        let s = AppEvent::AgentStep {
            step: 1,
            tool: "search_schema".into(),
        };
        assert!(matches!(s, AppEvent::AgentStep { step: 1, .. }));
        let t = AppEvent::AgentTool {
            name: "execute_read_query".into(),
            content: "ok".into(),
        };
        assert!(matches!(t, AppEvent::AgentTool { .. }));
    }

    #[test]
    fn event_done_and_error() {
        let d = AppEvent::AgentDone("done".into());
        assert_eq!(d, AppEvent::AgentDone("done".into()));
        let e = AppEvent::Error("boom".into());
        assert!(matches!(e, AppEvent::Error(_)));
        let q = AppEvent::Quit;
        assert_eq!(q, AppEvent::Quit);
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

    #[test]
    fn app_session_update_event_syncs_session() {
        let mut app = make_state();
        let mut new_sess = Session::new();
        new_sess.push(crate::llm::Message::user("pregunta previa".into()));
        new_sess.push(crate::llm::Message {
            role: "assistant".into(),
            content: "respuesta previa".into(),
            tool_calls: vec![],
            name: None,
            tool_call_id: None,
        });
        let sess_id = new_sess.id.clone();
        app.handle_event(AppEvent::SessionUpdate(Box::new(new_sess)));
        assert_eq!(app.session.id, sess_id);
        assert_eq!(app.session.messages.len(), 2);
    }
}
