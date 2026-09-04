use crate::agent::Session;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
