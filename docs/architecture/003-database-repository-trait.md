# ADR-003: DatabaseRepository trait for testability

## Status
Accepted

## Context
The `SqlServer` implementation directly uses `tiberius` for database connectivity. This creates tight coupling between the agent logic and the database driver, making it difficult to:
- Unit test agent logic without a live database
- Swap database implementations (e.g., for different SQL dialects)
- Test error handling paths (connection failures, query errors)

The original code had `Agent` directly holding a `SqlServer` instance.

## Decision
Extract a `DatabaseRepository` trait that defines the database operations needed by the agent:

```rust
#[async_trait]
pub trait DatabaseRepository: Send + Sync {
    async fn ping(&self) -> Result<(String, String)>;
    async fn list_tables(&self) -> Result<Vec<TableInfo>>;
    async fn describe_table_full(&self, table: &str) -> Result<TableDetail>;
    async fn search_columns(&self, query: &str) -> Result<Vec<ColumnMatch>>;
    async fn execute_read(&self, sql: ValidatedSql) -> Result<QueryResult>;
}
```

The `Agent` now holds `Arc<dyn DatabaseRepository>` and is constructed via:
- `Agent::new(config)` - uses real `SqlServer`
- `Agent::with_repository(config, llm, db)` - for testing with mock/fake implementations

## Consequences

### Positive
- **Full testability**: Agent logic can be tested with `MockDatabaseRepository` without database
- **Dependency inversion**: Agent depends on abstraction, not concrete implementation
- **Multiple backends**: Can add `PostgresRepository`, `MockRepository`, etc.
- **Fast tests**: Unit tests run in milliseconds without database

### Negative
- **Trait overhead**: Slight indirection cost (negligible for DB operations)
- **Trait object overhead**: `Arc<dyn Trait>` allocation per agent

## Alternatives Considered
1. **Keep SqlServer directly**: Simple but untestable without live DB. Rejected.
2. **Generics instead of trait objects**: `Agent<R: DatabaseRepository>`. Rejected - complicates API, prevents heterogeneous collections.
3. **Integration tests only**: Rely on integration tests with real DB. Rejected - too slow for CI, flaky.

## References
- [DatabaseRepository trait](../src/database/repository.rs)
- [SqlServer implementation](../src/database/sqlserver.rs)
- [Mock implementation for tests](../tests/security_seam.rs)