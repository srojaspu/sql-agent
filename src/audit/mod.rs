use anyhow::Result;
use serde::Serialize;
use tokio::io::AsyncWriteExt;

use crate::agent::session::{redact_content, JSONL_MAX_BYTES, JSONL_MAX_LINES};

#[derive(Serialize)]
pub struct AuditEvent<'a> {
    pub timestamp: String,
    pub event: &'a str,
    pub payload: serde_json::Value,
}

/// Recursively redact sensitive strings in an audit payload so SQL text and
/// user questions never persist PII/secrets verbatim.
fn redact_value(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(s) => serde_json::Value::String(redact_content(&s)),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(redact_value).collect())
        }
        serde_json::Value::Object(map) => {
            serde_json::Value::Object(map.into_iter().map(|(k, v)| (k, redact_value(v))).collect())
        }
        other => other,
    }
}

pub async fn write(path: &str, event: &str, payload: serde_json::Value) -> Result<()> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Bounded log: reuse the session JSONL rotation policy (size + line caps).
    let file_path = std::path::Path::new(path);
    if file_path.exists() {
        if let Ok(meta) = tokio::fs::metadata(file_path).await {
            if meta.len() > JSONL_MAX_BYTES {
                crate::agent::session::Session::rotate_file(file_path).await?;
            } else if let Ok(content) = tokio::fs::read_to_string(file_path).await {
                if content.lines().count() >= JSONL_MAX_LINES {
                    crate::agent::session::Session::rotate_file(file_path).await?;
                }
            }
        }
    }

    let record = AuditEvent {
        timestamp: chrono::Utc::now().to_rfc3339(),
        event,
        payload: redact_value(payload),
    };

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;

    file.write_all(serde_json::to_string(&record)?.as_bytes())
        .await?;
    file.write_all(b"\n").await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn audit_redacts_pii_before_persist() {
        let dir =
            std::env::temp_dir().join(format!("sql-agent-audit-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("audit.jsonl");
        let path_str = path.to_string_lossy().to_string();
        write(
            &path_str,
            "request",
            json!({ "question": "what is the password?", "sql": "SELECT password FROM t" }),
        )
        .await
        .expect("audit write ok");
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(
            content.contains("[REDACTED"),
            "audit must redact PII, got {content}"
        );
        assert!(
            !content.contains("what is the password?"),
            "raw question must not persist, got {content}"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn audit_rotates_bounded_log() {
        let dir =
            std::env::temp_dir().join(format!("sql-agent-audit-test-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("audit.jsonl");
        let path_str = path.to_string_lossy().to_string();
        // Fill to the line cap, then one more write must rotate (keep 5000 + 1).
        let mut lines = String::new();
        for i in 0..JSONL_MAX_LINES {
            lines.push_str(&format!("{{\"event\":\"e{i}\"}}\n"));
        }
        tokio::fs::write(&path, lines).await.unwrap();
        write(&path_str, "request", json!({ "question": "hello" }))
            .await
            .expect("audit write ok");
        let after = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(
            after.lines().count(),
            5001,
            "audit log must stay bounded via rotation"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
