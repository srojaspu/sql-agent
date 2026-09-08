//! Session persistence: bounded in-memory history plus redacted JSONL log.
//!
//! Imports redaction/rotation from the audit leaves only — never the
//! audit sink concrete types, keeping `agent::session` out of the
//! audit cycle.

use crate::audit::redaction::redact_content;
use crate::audit::rotation::{rotate_file, JSONL_MAX_BYTES, JSONL_MAX_LINES};
use crate::database::schema::SchemaMemoryEntry;
use crate::llm::Message;
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

pub const MAX_MESSAGES: usize = 40;
pub const MAX_CHARS: usize = 30_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub messages: Vec<Message>,
    pub schema_memory: HashMap<String, SchemaMemoryEntry>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Session {
    pub fn new() -> Self {
        let now = Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            messages: Vec::new(),
            schema_memory: HashMap::new(),
            created_at: now,
            updated_at: now,
        }
    }

    pub fn total_chars(&self) -> usize {
        self.messages.iter().map(|m| m.content.len()).sum()
    }

    pub fn push(&mut self, msg: Message) {
        self.messages.push(msg);
        self.updated_at = Utc::now();
        self.enforce_caps();
    }

    fn enforce_caps(&mut self) {
        // Cap message count: remove complete tool-turns atomically first.
        // A "tool turn" = one assistant message with tool_calls + all immediately
        // following tool messages that are its results. Removing partial turns
        // would leave an assistant message with unresolved tool_calls, causing
        // HTTP 400 errors in providers like Anthropic and Gemini.
        while self.messages.len() > MAX_MESSAGES {
            if !self.remove_oldest_tool_turn() {
                // No complete tool turn found; fall back to removing oldest tool message
                // (to preserve user/assistant conversation flow), then oldest non-tool.
                if let Some(pos) = self
                    .messages
                    .iter()
                    .position(|m: &Message| m.role == "tool")
                {
                    self.messages.remove(pos);
                } else if self.messages.len() > 1 {
                    self.messages.remove(0);
                } else {
                    break;
                }
            }
        }
        // Cap total chars: same atomic-turn strategy first.
        // If no complete tool turn exists, remove oldest tool message (not truncate).
        // If no tool messages exist, truncate the single huge message.
        while self.total_chars() > MAX_CHARS {
            if !self.remove_oldest_tool_turn() {
                // No complete tool turn exists. Try to remove oldest tool message.
                if let Some(pos) = self
                    .messages
                    .iter()
                    .position(|m: &Message| m.role == "tool")
                {
                    self.messages.remove(pos);
                    continue;
                }
                // No tool messages at all - truncate the largest message if it's huge,
                // otherwise remove oldest non-system message.
                if self.messages.len() == 1 {
                    let msg = &mut self.messages[0];
                    if msg.content.len() > MAX_CHARS {
                        msg.content.truncate(MAX_CHARS);
                        msg.content.push_str("...[truncated]");
                        break;
                    }
                }
                // Multiple messages but no tools - remove oldest non-system
                let mut removed = false;
                for i in 0..self.messages.len() {
                    if self.messages[i].role != "system" {
                        self.messages.remove(i);
                        removed = true;
                        break;
                    }
                }
                if !removed {
                    break;
                }
            }
            if self.messages.is_empty() {
                break;
            }
        }
        // Summarization fallback: if still over and we have many messages, keep last half
        // For now, aggressive truncation already handles it; this is placeholder for future summary
    }

    /// Remove the oldest complete tool turn atomically:
    /// one `assistant` message with non-empty `tool_calls` plus all immediately
    /// following `tool` messages (which are its results).
    ///
    /// Returns `true` if a turn was removed, `false` if none found.
    fn remove_oldest_tool_turn(&mut self) -> bool {
        // Find the first assistant message that has tool_calls.
        let Some(assistant_pos) = self
            .messages
            .iter()
            .position(|m| m.role == "assistant" && !m.tool_calls.is_empty())
        else {
            return false;
        };
        // Collect all consecutive `tool` messages that follow it.
        let mut end = assistant_pos + 1;
        while end < self.messages.len() && self.messages[end].role == "tool" {
            end += 1;
        }
        // Drain the assistant + all its tool results together.
        self.messages.drain(assistant_pos..end);
        true
    }

    pub fn history_path() -> PathBuf {
        // Try home dir via env, fallback to ./logs/chat-history.jsonl
        if let Ok(home) = std::env::var("HOME") {
            let p = PathBuf::from(home).join(".sql-agent").join("history.jsonl");
            return p;
        }
        if let Ok(up) = std::env::var("USERPROFILE") {
            let p = PathBuf::from(up).join(".sql-agent").join("history.jsonl");
            return p;
        }
        // fallback without dirs crate
        PathBuf::from("./logs/chat-history.jsonl")
    }

    pub fn history_path_with_dirs() -> PathBuf {
        Self::history_path()
    }

    /// Thin delegate to the shared audit rotation policy.
    pub(crate) async fn rotate_file(path: &Path) -> Result<()> {
        rotate_file(path)
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    }

    pub async fn persist(&self) -> Result<()> {
        // Single persistence path: default location delegates to the
        // shared persist_to implementation (rotation + redaction + append).
        self.persist_to(&Self::history_path()).await
    }

    pub async fn load_last() -> Result<Option<Self>> {
        let path = Self::history_path();
        if !path.exists() {
            return Ok(None);
        }
        let content = tokio::fs::read_to_string(&path).await?;
        let mut last: Option<Self> = None;
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Self>(line) {
                Ok(sess) => last = Some(sess),
                Err(_) => {
                    tracing::warn!("skipping corrupted JSONL line");
                    continue;
                }
            }
        }
        Ok(last)
    }

    /// Helper for tests: load from specific path
    pub async fn load_last_from(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let content = tokio::fs::read_to_string(path).await?;
        let mut last: Option<Self> = None;
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Self>(line) {
                Ok(sess) => last = Some(sess),
                Err(_) => continue,
            }
        }
        Ok(last)
    }

    pub async fn persist_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        if path.exists() {
            if let Ok(meta) = tokio::fs::metadata(path).await {
                if meta.len() > JSONL_MAX_BYTES {
                    Self::rotate_file(path).await?;
                } else if let Ok(content) = tokio::fs::read_to_string(path).await {
                    if content.lines().count() >= JSONL_MAX_LINES {
                        Self::rotate_file(path).await?;
                    }
                }
            }
        }
        let mut redacted = self.clone();
        for msg in &mut redacted.messages {
            msg.content = redact_content(&msg.content);
        }
        let json = serde_json::to_string(&redacted)?;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await?;
        file.write_all(json.as_bytes()).await?;
        file.write_all(b"\n").await?;
        Ok(())
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{Message, ToolCall, ToolFunction};

    /// One assistant-with-`tool_calls` plus its tool result (a whole turn).
    fn tool_turn(call_id: &str, tool_name: &str, result: &str) -> Vec<Message> {
        let mut assistant = Message::assistant("working".to_string());
        assistant.tool_calls = vec![ToolCall {
            id: Some(call_id.to_string()),
            function: ToolFunction {
                name: tool_name.to_string(),
                arguments: serde_json::json!({}),
            },
        }];
        vec![
            assistant,
            Message::tool_with_call_id(tool_name, result.to_string(), Some(call_id.to_string())),
        ]
    }

    /// Invariant: every assistant `tool_call` id keeps a matching tool result.
    fn assert_tool_calls_paired(s: &Session) {
        for m in &s.messages {
            for call in &m.tool_calls {
                let id = call
                    .id
                    .clone()
                    .unwrap_or_else(|| call.function.name.clone());
                assert!(
                    s.messages.iter().any(|r| r.role == "tool"
                        && r.tool_call_id.as_deref() == Some(id.as_str())),
                    "orphan tool_call {id}: no tool result follows"
                );
            }
        }
    }

    #[test]
    fn remove_oldest_tool_turn_drains_oldest_whole_turn() {
        let mut s = Session::new();
        s.messages.push(Message::user("q".to_string()));
        s.messages
            .extend(tool_turn("c1", "search_schema", "result one"));
        s.messages
            .extend(tool_turn("c2", "search_schema", "result two"));

        assert!(s.remove_oldest_tool_turn(), "oldest turn must drain");
        assert_eq!(
            s.messages.len(),
            3,
            "assistant + tool of c1 removed together"
        );
        assert!(
            !s.messages.iter().any(|m| m.content == "result one"),
            "oldest result must be gone"
        );
        assert!(
            s.messages.iter().any(|m| m.content == "result two"),
            "newest turn must survive"
        );
        assert_tool_calls_paired(&s);
    }

    #[test]
    fn remove_oldest_tool_turn_empty_is_noop() {
        let mut s = Session::new();
        assert!(!s.remove_oldest_tool_turn(), "empty history drains nothing");
        assert!(s.messages.is_empty(), "history stays empty");
    }

    #[test]
    fn remove_oldest_tool_turn_without_turns_returns_false() {
        let mut s = Session::new();
        s.messages.push(Message::user("hello".to_string()));
        s.messages
            .push(Message::tool("search_schema", "rows".to_string()));
        assert!(
            !s.remove_oldest_tool_turn(),
            "no assistant[tool_calls] means no whole turn"
        );
        assert_eq!(s.messages.len(), 2, "history untouched");
    }

    #[test]
    fn enforce_caps_evicts_whole_turns_preserving_pairing() {
        let mut s = Session::new();
        for i in 0..20 {
            s.messages.extend(tool_turn(
                &format!("c{i}"),
                "search_schema",
                &format!("result {i}"),
            ));
        }
        assert_eq!(s.messages.len(), 40);
        s.push(Message::user("latest question".to_string()));
        assert!(s.messages.len() <= MAX_MESSAGES, "cap enforced");
        assert!(
            !s.messages.iter().any(|m| m.content == "result 0"),
            "oldest whole turn evicted first"
        );
        assert!(
            s.messages.iter().any(|m| m.content == "latest question"),
            "newest message retained"
        );
        assert_tool_calls_paired(&s);
    }

    #[test]
    fn session_new_has_uuid_and_timestamps() {
        let s = Session::new();
        assert!(!s.id.is_empty(), "id should be uuid");
        assert!(s.messages.is_empty());
        assert!(s.schema_memory.is_empty());
        // timestamps should be recent
        let now = Utc::now();
        assert!((now - s.created_at).num_seconds().abs() < 5);
    }

    #[test]
    fn session_push_adds_message() {
        let mut s = Session::new();
        s.push(Message::user("hola".into()));
        assert_eq!(s.messages.len(), 1);
        assert_eq!(s.messages[0].role, "user");
    }

    #[test]
    fn session_total_chars_counts() {
        let mut s = Session::new();
        s.push(Message::user("hello".into())); // 5
        s.push(Message::tool("execute_read_query", "world".into())); // 5
        assert_eq!(s.total_chars(), 10);
    }

    #[test]
    fn session_caps_40_truncates_oldest_tool_first() {
        let mut s = Session::new();
        // push 10 user + 31 tool = 41 total, should truncate oldest tool first
        for i in 0..10 {
            s.push(Message::user(format!("user {i}")));
        }
        for i in 0..31 {
            s.push(Message::tool(
                "execute_read_query",
                format!("tool result {i}"),
            ));
        }
        assert_eq!(s.messages.len(), 40, "should cap at 40");
        // oldest tool "tool result 0" should be removed, user messages preserved
        let has_tool_0 = s.messages.iter().any(|m| m.content == "tool result 0");
        assert!(!has_tool_0, "oldest tool should be truncated first");
        let has_tool_30 = s.messages.iter().any(|m| m.content == "tool result 30");
        assert!(has_tool_30, "newest tool should remain");
    }

    #[test]
    fn session_caps_preserves_recent_user_when_no_tool() {
        let mut s = Session::new();
        for i in 0..41 {
            s.push(Message::user(format!("msg {i}")));
        }
        assert_eq!(s.messages.len(), 40);
        // oldest should be removed (msg 0), newest retained
        assert!(!s.messages.iter().any(|m| m.content == "msg 0"));
        assert!(s.messages.iter().any(|m| m.content == "msg 40"));
    }

    #[test]
    fn session_caps_30k_truncates_oldest_tool_results() {
        let mut s = Session::new();
        // Each tool result 10k chars, 4 of them => 40k >30k, should truncate oldest
        let big = "x".repeat(10_000);
        for _ in 0..4 {
            s.push(Message::tool("execute_read_query", big.clone()));
        }
        assert!(
            s.total_chars() <= MAX_CHARS,
            "total chars {} should be <= {}",
            s.total_chars(),
            MAX_CHARS
        );
        assert!(
            s.messages.len() < 4,
            "should have truncated at least one tool result"
        );
    }

    #[test]
    fn session_caps_single_huge_message_truncated() {
        let mut s = Session::new();
        let huge = "y".repeat(40_000);
        s.push(Message::user(huge));
        assert!(
            s.total_chars() <= MAX_CHARS + 20,
            "single huge should be truncated to cap"
        );
        assert_eq!(s.messages.len(), 1);
        assert!(s.messages[0].content.contains("[truncated]"));
    }

    #[test]
    fn redact_uses_audit_single_source() {
        // Pins the import: session redaction IS audit redaction.
        let c = "SELECT password FROM users";
        assert_eq!(redact_content(c), "[REDACTED sensitive content]");
        let ok = "SELECT name FROM users";
        assert_eq!(redact_content(ok), ok);
    }

    #[tokio::test]
    async fn jsonl_persist_and_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("sql-agent-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("history.jsonl");
        let mut s = Session::new();
        s.push(Message::user("hello".into()));
        s.persist_to(&path).await.expect("persist ok");
        let loaded = Session::load_last_from(&path)
            .await
            .expect("load ok")
            .expect("some");
        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(loaded.messages[0].content, "hello");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn jsonl_corrupted_line_tolerance() {
        let dir = std::env::temp_dir().join(format!("sql-agent-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("history.jsonl");
        let mut s = Session::new();
        s.push(Message::user("first".into()));
        s.persist_to(&path).await.unwrap();
        // Append corrupted line
        tokio::fs::write(&path, "not json\n".as_bytes())
            .await
            .unwrap();
        // Actually need append: read existing and append corrupted
        let mut f = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .unwrap();
        f.write_all(b"not json line\n").await.unwrap();
        // Append second valid session
        let mut s2 = Session::new();
        s2.push(Message::user("second".into()));
        // Manually write second valid json
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .unwrap();
        let json = serde_json::to_string(&s2).unwrap();
        file.write_all(json.as_bytes()).await.unwrap();
        file.write_all(b"\n").await.unwrap();

        // Now load_last should return second, ignoring corrupted
        let loaded = Session::load_last_from(&path).await.unwrap().unwrap();
        assert_eq!(loaded.messages[0].content, "second");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn jsonl_redaction_on_persist() {
        let dir = std::env::temp_dir().join(format!("sql-agent-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("history.jsonl");
        let mut s = Session::new();
        s.push(Message::user("SELECT password FROM users".into()));
        s.persist_to(&path).await.unwrap();
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(
            content.contains("[REDACTED"),
            "should redact sensitive content, got {content}"
        );
        assert!(
            !content.to_ascii_lowercase().contains("password") || content.contains("[REDACTED"),
            "password should be redacted"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn jsonl_rotation_at_10k_lines() {
        let dir = std::env::temp_dir().join(format!("sql-agent-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("history.jsonl");
        // Simulate rotation helper: create file with 10k lines then persist should rotate
        // Instead test rotate_file directly keeps last 5000
        let mut lines = String::new();
        for i in 0..10_000 {
            lines.push_str(&format!("{{\"id\":\"{i}\",\"messages\":[],\"schema_memory\":{{}},\"created_at\":\"2020-01-01T00:00:00Z\",\"updated_at\":\"2020-01-01T00:00:00Z\"}}\n"));
        }
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(&path, lines).await.unwrap();
        // Now call rotate
        Session::rotate_file(&path).await.unwrap();
        let after = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(after.lines().count(), 5000, "rotate should keep 5000");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[test]
    fn history_path_fallback_exists() {
        let p = Session::history_path();
        assert!(p.to_string_lossy().contains("history.jsonl"));
    }
}
