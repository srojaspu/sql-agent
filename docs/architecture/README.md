# Architecture Decision Records (ADRs)

This directory contains Architecture Decision Records for the sql-agent project.

## ADR Index

| ADR | Title | Status | Date |
|-----|-------|--------|------|
| 001 | [ValidatedSql newtype for SQL safety](./001-validated-sql-newtype.md) | Accepted | 2026-09-07 |
| 002 | [AuditSink trait for observability](./002-auditsink-trait.md) | Accepted | 2026-09-07 |
| 003 | [DatabaseRepository trait for testability](./003-database-repository-trait.md) | Accepted | 2026-09-07 |
| 004 | [Config typed with aliases and precedence](./004-config-typed-aliases.md) | Accepted | 2026-09-07 |
| 005 | [Ranking module for schema discovery](./005-ranking-module.md) | Accepted | 2026-09-07 |
| 006 | [TUI event-driven with dirty-check rendering](./006-tui-event-driven.md) | Accepted | 2026-09-07 |
| 007 | [Split core.rs into focused modules](./007-split-core-module.md) | Accepted | 2026-09-07 |
| 008 | [Extract TUI runner from main.rs](./008-extract-tui-runner.md) | Accepted | 2026-09-07 |
| 009 | [Split format.rs into focused modules](./009-split-format-module.md) | Accepted | 2026-09-07 |

---

## ADR Template

When creating a new ADR, use this template:

```markdown
# ADR-XXX: [Title]

## Status
[Proposed | Accepted | Superseded | Deprecated]

## Context
What is the issue that we're seeing that is motivating this decision or change?

## Decision
What is the change that we're proposing or have decided to implement?

## Consequences
What becomes easier or more difficult to do because of this change?

### Positive
- ...

### Negative
- ...

## Alternatives Considered
- ...

## References
- ...
```