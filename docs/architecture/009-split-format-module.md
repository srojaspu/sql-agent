# ADR-009: Split format.rs into focused modules

## Status
Accepted

## Context
`src/agent/format.rs` grew to ~700 lines with three distinct concerns:
1. **Core formatters**: Table/column/cell/result formatting, SQL detection, invalid object re-inject
2. **Export utilities**: CSV/JSON export with escaping
3. **History formatting**: Human-readable chat history for `/history` command

This violated SRP and made the file hard to navigate. Tests were also mixed (formatters + export + history all in one test module).

## Decision
Split `format.rs` into three modules under `src/agent/`:

| Module | Responsibility | Key Exports |
|--------|---------------|-------------|
| `formatters.rs` | Core formatters (table/column/cell/result, SQL detection, re-inject) | `format_table_list`, `format_column_matches`, `format_query_result`, `format_table_detail`, `format_invalid_reinject`, `limit_text`, `looks_like_sql`, `split_table`, `split_table_pub`, `format_cell_value`, `format_column_matches` |
| `export.rs` | CSV/JSON export with RFC 4180 escaping | `csv_escape`, `export_messages_csv`, `export_messages_json` |
| `history.rs` | Human-readable history formatting | `format_history_readable` |
| `format.rs` | **Facade** - re-exports all for backward compatibility | Re-exports all public API |

Updated `src/agent/mod.rs` to re-export from the new modules.

## Consequences

### Positive
- **Single Responsibility**: Each module has one clear purpose
- **Test organization**: Tests grouped by concern (formatters, export, history)
- **Import clarity**: Consumers can import specific modules or use facade
- **Maintainability**: Changes to export logic don't touch core formatters

### Negative
- **Module count**: +3 modules in agent
- **Facade overhead**: `format.rs` is now just re-exports
- **Re-export management**: Must keep facade in sync

## Alternatives Considered
1. **Keep monolithic format.rs**: Simpler but unmaintainable. Rejected.
2. **Subdirectory (agent/format/)**: Adds directory depth. Rejected - flat is simpler.
3. **Separate crates**: Overkill for internal modules. Rejected.

## References
- [formatters.rs](../src/agent/formatters.rs)
- [export.rs](../src/agent/export.rs)
- [history.rs](../src/agent/history.rs)
- [format.rs (facade)](../src/agent/format.rs)