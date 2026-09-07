//! Audit sink abstraction breaking the `agent::core → audit → agent::session` cycle.
//!
//! The agent layer owns an `Arc<dyn AuditSink>` and delegates event writes
//! to it; this module (plus its leaves) never imports `agent::*`.

use crate::error::AuditError;

/// Destination for audit events. Implementations must stay best-effort
/// and bounded; failures surface as [`AuditError`].
#[async_trait::async_trait]
pub trait AuditSink: Send + Sync {
    /// Persist one audit event with its JSON payload.
    async fn write(
        &self,
        event: &str,
        payload: serde_json::Value,
    ) -> Result<(), AuditError>;
}

/// JSONL file sink: appends redacted events with rotation bounds.
/// Constructed once from the configured audit path.
pub struct FileAuditSink {
    /// JSONL audit-log path (parent directories are created on write).
    pub path: String,
}

impl FileAuditSink {
    /// Build a file sink for the given audit-log path.
    pub fn new(path: String) -> Self {
        Self { path }
    }
}

#[async_trait::async_trait]
impl AuditSink for FileAuditSink {
    async fn write(
        &self,
        event: &str,
        payload: serde_json::Value,
    ) -> Result<(), AuditError> {
        crate::audit::file_sink::write(&self.path, event, payload).await
    }
}

/// Sink that accepts events and persists nothing (tests, disabled audit).
pub struct NoopAuditSink;

#[async_trait::async_trait]
impl AuditSink for NoopAuditSink {
    async fn write(
        &self,
        _event: &str,
        _payload: serde_json::Value,
    ) -> Result<(), AuditError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct CountingSink {
        events: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl AuditSink for CountingSink {
        async fn write(
            &self,
            _event: &str,
            _payload: serde_json::Value,
        ) -> Result<(), AuditError> {
            self.events.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn fake_sink_counts_events() {
        let events = Arc::new(AtomicUsize::new(0));
        let sink = CountingSink {
            events: events.clone(),
        };
        for i in 0..3 {
            sink.write("request", json!({ "n": i })).await.unwrap();
        }
        assert_eq!(events.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn noop_sink_accepts_writes() {
        NoopAuditSink
            .write("request", json!({ "n": 1 }))
            .await
            .expect("noop ok");
    }
}
