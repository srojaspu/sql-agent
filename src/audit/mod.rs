use anyhow::Result;
use serde::Serialize;
use tokio::io::AsyncWriteExt;

#[derive(Serialize)]
pub struct AuditEvent<'a> {
    pub timestamp: String,
    pub event: &'a str,
    pub payload: serde_json::Value,
}

pub async fn write(path: &str, event: &str, payload: serde_json::Value) -> Result<()> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let record = AuditEvent {
        timestamp: chrono::Utc::now().to_rfc3339(),
        event,
        payload,
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
