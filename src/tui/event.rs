/// TUI events — Agent→UI and UI internal
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppEvent {
    Input(String),
    AgentStep { step: usize, tool: String },
    AgentTool { name: String, content: String },
    AgentDone(String),
    Error(String),
    Quit,
}

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
