# ADR-002: AuditSink trait for observability

## Status
Accepted

## Context
The system needs to log various events for debugging, security auditing, and monitoring:
- User requests and responses
- SQL queries executed (with PII redaction)
- Tool calls and their results
- Errors and retries
- Schema cache operations

The original code used direct `println!` and file writes scattered throughout the codebase, making it impossible to:
- Redirect logs to different outputs (file, stdout, external service)
- Control log levels dynamically
- Test logging behavior
- Redact PII consistently

## Decision
Introduce an `AuditSink` trait with a default `FileAuditSink` implementation:

```rust
#[async_trait]
pub trait AuditSink: Send + Sync {
    async fn write(&self, event: &str, payload: serde_json::Value) -> Result<()>;
}

pub struct FileAuditSink {
    path: PathBuf,
    // ... rotation config
}
```

The `Agent` holds an `Arc<dyn AuditSink>` and calls `audit(event, payload)` for all significant events.

PII redaction happens at the sink level via `redact_content()` before persistence.

## Consequences

### Positive
- **Pluggable backends**: Can swap `FileAuditSink` for `NoopAuditSink` (tests) or remote sink (production)
- **Consistent PII handling**: Redaction happens at single point in `FileAuditSink::write`
- **Structured logging**: All events use consistent JSON payload format
- **Testability**: `NoopAuditSink` and `FakeAuditSink` for unit tests
- **Rotation**: Built-in log rotation with configurable bounds

### Negative
- **Async overhead**: All audit writes are async
- **Additional abstraction**: Slight complexity increase

## Alternatives Considered
1. **Direct logging crate**: Use `tracing`/`log` directly. Rejected - doesn't provide structured event payloads or PII redaction guarantees.
2. **Event sourcing**: Full event store. Rejected - overkill for current needs.
3. **Global logger**: Single global `AuditSink`. Rejected - prevents testing with different sinks.

## References
- [AuditSink trait](../src/audit/sink.rs)
- [FileAuditSink implementation](../src/audit/file_sink.rs)
- [Redaction helper](../src/audit/redaction.rs)