//! Session history context: capped LLM window plus anaphora detection.
//!
//! Pure helpers over [`crate::agent::session::Session`]; the message/char
//! caps live with the store and are re-exported here for context callers.

use crate::agent::session::Session;
use crate::llm::Message;

pub use super::store::{MAX_CHARS, MAX_MESSAGES};

/// Build LLM message context from session history with caps and schema hints.
/// Pure function for testability; used by Agent::run_with_history.
pub fn build_history_context(session: &Session, question: &str, max_chars: usize) -> Vec<Message> {
    let mut out = Vec::new();
    // Add history messages, respecting char cap
    let mut total = 0usize;
    // Iterate history and question, but enforce max_chars
    let mut all: Vec<Message> = session.messages.clone();
    all.push(Message::user(question.to_string()));
    for m in &all {
        let len = m.content.len();
        if total + len > max_chars && !out.is_empty() {
            // Find oldest tool to drop, else drop oldest
            if let Some(pos) = out.iter().position(|x: &Message| x.role == "tool") {
                total -= out[pos].content.len();
                out.remove(pos);
            } else {
                total -= out[0].content.len();
                out.remove(0);
            }
            if total + len > max_chars {
                // truncate current message
                let truncated: String = m.content.chars().take(max_chars - total).collect();
                let mut nm = m.clone();
                nm.content = truncated;
                out.push(nm);
                break;
            }
        }
        total += len;
        out.push(m.clone());
        // Also enforce 40 message cap on context window
        while out.len() > MAX_MESSAGES {
            if let Some(pos) = out.iter().position(|x: &Message| x.role == "tool") {
                total -= out[pos].content.len();
                out.remove(pos);
            } else {
                total -= out[0].content.len();
                out.remove(0);
            }
        }
    }
    out
}

/// Detect anaphora that refers to previous results (y de esos, de esos, etc.)
pub fn is_anaphoric(question: &str) -> bool {
    let lower = question.to_ascii_lowercase();
    lower.contains("de esos")
        || lower.contains("de esas")
        || lower.contains("y esos")
        || lower.contains("y esas")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_history_context_includes_history() {
        let mut s = Session::new();
        s.push(Message::user("cuantos usuarios hay".into()));
        s.push(Message::tool(
            "execute_read_query",
            "✓ RESULTADOS (2 filas) Fila 1: name=Juan".into(),
        ));
        let ctx = build_history_context(&s, "y de esos cuantos activos", 30_000);
        // Should contain history + new question
        assert!(ctx.len() >= 3, "should include history + question");
        assert!(ctx
            .iter()
            .any(|m| m.content.contains("cuantos usuarios hay")));
        assert!(ctx
            .iter()
            .any(|m| m.content.contains("y de esos cuantos activos")));
    }

    #[test]
    fn is_anaphoric_detects_y_de_esos() {
        assert!(is_anaphoric("y de esos cuantos activos"));
        assert!(is_anaphoric("y de esas cuantas activas"));
        assert!(!is_anaphoric("cuantos usuarios hay en total"));
    }
}
