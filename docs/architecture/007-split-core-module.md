# ADR-007: Split core.rs into focused modules

## Status
Accepted

## Context
`src/agent/core.rs` grew to ~1200 lines with multiple responsibilities:
- Agent struct + constructors
- Primary `run()` method (one-shot)
- `run_with_history()` (multi-turn)
- Tool implementations (`search_schema_ranked`, `describe_table_tool`, `execute_read_tool`, etc.)
- Schema caching (`cached_schema`, `cached_tables`)
- TUI helpers (`tui_list_tables`, `tui_describe`, `refresh_cache`)
- Tests (20+ test functions)

This violated SRP and made the file difficult to navigate, test, and maintain.

## Decision
Split `core.rs` into focused modules within `src/agent/`:

| Module | Responsibility | Key Exports |
|--------|---------------|-------------|
| `core.rs` | Agent struct, constructors, `run()` | `Agent`, `run()` |
| `loop_impl.rs` | `run_with_history()`, `build_messages_with_history()` | `run_with_history()` |
| `tools.rs` | Tool implementations (`search_schema_ranked`, `execute_read_tool`, etc.) | `search_schema_ranked`, `execute_read_tool`, etc. |
| `schema_cache.rs` | Schema caching (`cached_schema`, `cached_tables`) | `cached_schema()`, `cached_tables()` |
| `tui_helpers.rs` | TUI integration (`tui_list_tables`, `tui_describe`, `refresh_cache`) | `tui_list_tables`, `tui_describe`, `refresh_cache` |
| `dispatcher.rs` | Unified tool dispatch | `dispatch_tool()` |

Each module has its own `impl Agent` block with `pub(crate)` methods.

## Consequences

### Positive
- **Single Responsibility**: Each module has one clear purpose
- **Navigability**: Related code grouped logically
- **Compile times**: Smaller modules = faster incremental builds
- **Testability**: Each module can be tested in isolation
- **Ownership**: Clear ownership boundaries for team scaling

### Negative
- **Module proliferation**: More files to navigate
- **Cross-module imports**: More `use super::` imports needed
- **Initial refactor effort**: Time investment

## Alternatives Considered
1. **Keep monolithic core.rs**: Simpler initially but unmaintainable. Rejected.
2. **Subdirectory per concern (agent/loop/, agent/tools/)**: Adds directory depth. Rejected - flat is simpler.
3. **Keep tests in core.rs**: Tests moved with their code. Rejected - tests stay with their module.

## References
- [core.rs](../src/agent/core.rs)
- [loop_impl.rs](../src/agent/loop_impl.rs)
- [tools.rs](../src/agent/tools.rs)
- [schema_cache.rs](../src/agent/schema_cache.rs)
- [tui_helpers.rs](../src/agent/tui_helpers.rs)