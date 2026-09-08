# ADR-008: Extract TUI runner from main.rs

## Status
Accepted

## Context
`main.rs` contained the full TUI event loop (~200 lines) mixed with binary entry point logic:
- Argument parsing
- Config loading
- Banner printing
- One-shot vs TUI decision
- TUI event loop with command handling

This made `main.rs` a "god module" that was difficult to test and violated separation of concerns.

## Decision
Extract TUI event loop into `src/tui/runner.rs`:

```rust
// In tui/runner.rs
pub async fn run_tui(agent: Agent) -> Result<()> { ... }

// In main.rs
use crate::tui::run_tui;
// ...
match run_tui(agent).await { ... }
```

Created `src/tui/runner.rs` with:
- `run_tui(agent: Agent) -> Result<()>` - Main entry point
- `handle_key_event()` - Extracted key handling logic
- `spawn_agent_task()` - Helper for spawning agent tasks
- Unit tests for command parsing

Updated `main.rs` to:
- Import `run_tui` from `tui`
- Remove inline `run_tui` function
- Remove `TerminalGuard` import (used by runner)

## Consequences

### Positive
- **Clean main.rs**: Binary entry point now <100 lines
- **Testability**: Runner logic testable without binary
- **Reusability**: `run_tui` can be called from other binaries
- **Separation**: Binary concerns (args, config) separated from TUI logic

### Negative
- **One more module**: Slight increase in module count
- **Re-export**: Need to export `run_tui` from `tui` module

## Alternatives Considered
1. **Keep in main.rs**: Simpler but untestable. Rejected.
2. **Separate binary for TUI**: Overkill for single binary. Rejected.
3. **Extract to separate crate**: Overkill. Rejected.

## References
- [tui/runner.rs](../src/tui/runner.rs)
- [main.rs](../src/main.rs)
- [tui/mod.rs](../src/tui/mod.rs)