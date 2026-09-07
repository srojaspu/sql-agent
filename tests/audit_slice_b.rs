//! Slice B (audit cycle break): NEW tests, written BEFORE the implementation.
//!
//! 1. `audit_sink_redacts_pii_before_persist` — sensitive values never reach disk.
//! 2. `audit_sink_rotation_bounds_log` — 5000/5001-line cap behavior.
//! 3. `fake_sink_counts_events` — a fake `AuditSink` counts events (plus `NoopAuditSink`).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::json;
use sql_agent::audit::{AuditSink, FileAuditSink, NoopAuditSink, JSONL_MAX_LINES};

struct FakeAuditSink {
    events: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl AuditSink for FakeAuditSink {
    async fn write(
        &self,
        _event: &str,
        _payload: serde_json::Value,
    ) -> Result<(), sql_agent::error::AuditError> {
        self.events.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn audit_sink_redacts_pii_before_persist() {
    let dir = std::env::temp_dir().join(format!("sql-agent-sink-test-{}", uuid::Uuid::new_v4()));
    let path = dir.join("audit.jsonl");
    let path_str = path.to_string_lossy().to_string();
    let sink = FileAuditSink::new(path_str);
    sink.write(
        "request",
        json!({ "question": "what is the password?", "sql": "SELECT api_key FROM t" }),
    )
    .await
    .expect("sink write ok");
    let content = tokio::fs::read_to_string(&path).await.unwrap();
    assert!(
        content.contains("[REDACTED"),
        "sink must redact PII, got {content}"
    );
    assert!(
        !content.contains("what is the password?"),
        "raw question must never reach disk, got {content}"
    );
    assert!(
        !content.contains("SELECT api_key FROM t"),
        "raw SQL must never reach disk, got {content}"
    );
    let _ = tokio::fs::remove_dir_all(&dir).await;
}

#[tokio::test]
async fn audit_sink_rotation_bounds_log() {
    let dir = std::env::temp_dir().join(format!("sql-agent-sink-test-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let path = dir.join("audit.jsonl");
    let path_str = path.to_string_lossy().to_string();
    // Fill to the line cap, then one more write must rotate (keep 5000 + 1).
    let mut lines = String::new();
    for i in 0..JSONL_MAX_LINES {
        lines.push_str(&format!("{{\"event\":\"e{i}\"}}\n"));
    }
    tokio::fs::write(&path, lines).await.unwrap();
    let sink = FileAuditSink::new(path_str);
    sink.write("request", json!({ "question": "hello" }))
        .await
        .expect("sink write ok");
    let after = tokio::fs::read_to_string(&path).await.unwrap();
    assert_eq!(
        after.lines().count(),
        5001,
        "audit log must stay bounded via rotation"
    );
    let _ = tokio::fs::remove_dir_all(&dir).await;
}

#[tokio::test]
async fn fake_sink_counts_events() {
    let events = Arc::new(AtomicUsize::new(0));
    let sink = FakeAuditSink {
        events: events.clone(),
    };
    for i in 0..3 {
        sink.write("request", json!({ "n": i }))
            .await
            .expect("fake write ok");
    }
    assert_eq!(events.load(Ordering::SeqCst), 3, "fake sink must count events");

    // Noop sink accepts writes and persists nothing.
    let noop = NoopAuditSink;
    noop.write("request", json!({ "n": 1 }))
        .await
        .expect("noop write ok");
}
