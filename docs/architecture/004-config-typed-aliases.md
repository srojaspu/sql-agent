# ADR-004: Config typed with aliases and precedence

## Status
Accepted

## Context
Configuration was originally a flat `Config` struct with all fields at the top level. As the project grew, issues emerged:
- **Flat structure**: Related settings scattered (DB, LLM, policy, limits all mixed)
- **String-only values**: No validation, typos only caught at runtime
- **No aliases**: Users had to remember exact env var names (e.g., `DATABASE_NAME` vs `DB_NAME`)
- **No precedence**: Conflicting settings (ALLOWED_TABLES vs BLOCKED_TABLES) had undefined behavior

## Decision
Restructure configuration into typed sub-structs with explicit aliases and clear precedence:

```rust
pub struct Config {
    pub db: DbConfig,
    pub llm: LlmConfig,
    pub policy: PolicyConfig,
    pub limits: LimitsConfig,
    pub audit: AuditConfig,
    pub verbose: bool,
}

// Alias resolution in loader.rs
// BLOCKED_TABLES -> allowed_tables (inverted)
// ALLOWED_TABLES -> allowed_tables
// MAX_AGENT_STEPS -> limits.max_steps
```

Loader implements:
1. **Type-safe parsing**: Each field parsed with proper type (u64, bool, String, etc.)
2. **Alias resolution**: Multiple env vars map to same config field with defined precedence
3. **Validation**: Reject invalid values early (empty DB name, invalid retry count, etc.)
4. **Defaults**: Sensible defaults for optional fields

## Consequences

### Positive
- **Type safety**: Config errors caught at startup, not runtime
- **Discoverability**: Related settings grouped logically
- **User-friendly**: Multiple aliases for same setting (DB_NAME, DATABASE_NAME)
- **Explicit precedence**: Clear rules for conflicting aliases
- **Validation**: Early failure with descriptive messages

### Negative
- **More code**: Loader module adds complexity
- **Migration**: Existing configs may need updates

## Alternatives Considered
1. **Flat config with serde**: Simple but no validation/aliases. Rejected.
2. **External config library (figment, config-rs)**: Adds dependency. Rejected - custom loader is simple enough.
3. **TOML/JSON config files**: Adds file I/O complexity. Rejected - env vars sufficient for containers.

## References
- [Config structs](../src/config/mod.rs)
- [Loader with alias resolution](../src/config/loader.rs)
- [Config tests](../tests/config_loader.rs)