//! Bounded-JSONL rotation policy shared by audit and session logs.
//!
//! Pure leaf module: no `agent::*` imports. Caps are product policy
//! (5000 kept / 10k lines / 5MB); do not retune here.

use std::path::Path;

use crate::error::AuditError;

/// Line cap that triggers rotation on the next append.
pub const JSONL_MAX_LINES: usize = 10_000;
/// Byte cap that triggers rotation on the next append.
pub const JSONL_MAX_BYTES: u64 = 5 * 1024 * 1024;
/// Lines kept when rotation truncates a log file.
pub const JSONL_KEEP_LINES: usize = 5000;

/// Truncate a JSONL file to its last [`JSONL_KEEP_LINES`] lines.
///
/// Missing files read as empty; the truncated content atomically
/// replaces the original in a single write.
pub(crate) async fn rotate_file(path: &Path) -> Result<(), AuditError> {
    let content = tokio::fs::read_to_string(path).await.unwrap_or_default();
    let lines: Vec<&str> = content.lines().collect();
    // Keep last 5000 lines
    let keep = if lines.len() > JSONL_KEEP_LINES {
        &lines[lines.len() - JSONL_KEEP_LINES..]
    } else {
        &lines[..]
    };
    let mut tmp = String::new();
    for l in keep {
        tmp.push_str(l);
        tmp.push('\n');
    }
    tokio::fs::write(path, tmp)
        .await
        .map_err(|e| AuditError::Io(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rotate_file_keeps_last_5000() {
        let dir = std::env::temp_dir().join(format!("sql-agent-rotate-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("history.jsonl");
        let mut lines = String::new();
        for i in 0..10_000 {
            lines.push_str(&format!("{{\"id\":\"{i}\",\"messages\":[],\"schema_memory\":{{}},\"created_at\":\"2020-01-01T00:00:00Z\",\"updated_at\":\"2020-01-01T00:00:00Z\"}}\n"));
        }
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(&path, lines).await.unwrap();
        rotate_file(&path).await.unwrap();
        let after = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(after.lines().count(), 5000, "rotate should keep 5000");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
