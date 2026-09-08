# ADR-006: TUI event-driven with dirty-check rendering

## Status
Accepted

## Context
The TUI was originally a simple loop that redrew the entire screen on every iteration. This caused:
- **High CPU usage**: Continuous redraw even when nothing changed
- **Flickering**: Full-screen redraw causes visual artifacts
- **Input lag**: Blocking on redraw delays key processing

The original `main.rs` had a monolithic event loop with inline rendering logic.

## Decision
Implement event-driven TUI with dirty-check rendering:

1. **Event loop**: Single `tokio::select!` with two branches:
   - Agent events (via `mpsc::channel`)
   - Crossterm events (keys, resize)
   
2. **Dirty-check rendering**: Only redraw when `app.version != last_drawn`
   - `app.version` increments on any state mutation
   - Idle ticks (20ms) do zero work if no events

3. **Extracted modules**:
   - `tui/runner.rs` - Event loop extracted from `main.rs`
   - `tui/state.rs` - `AppState` with `version` counter
   - `tui/handlers` - Key/command handling

```rust
// Dirty check
if app.version != last_drawn {
    terminal.draw(|f| tui::ui::draw(f, &app))?;
    last_drawn = app.version;
}
```

## Consequences

### Positive
- **Low CPU**: Zero work during idle
- **Smooth UX**: No flickering, responsive input
- **Separation of concerns**: Runner, state, UI separated
- **Testability**: Runner logic testable without terminal

### Negative
- **Version management**: Must remember to increment `version` on all state changes
- **Complexity**: More moving parts than simple loop

## Alternatives Considered
1. **Immediate mode GUI (egui, imgui)**: Adds heavy dependency. Rejected - ratatui is lighter.
2. **Fixed interval render (60fps)**: Simpler but wastes CPU. Rejected.
3. **Reactive framework (druid, iced)**: Heavy abstraction. Rejected.

## References
- [tui/runner.rs](../src/tui/runner.rs)
- [tui/state.rs](../src/tui/state.rs)
- [tui/ui.rs](../src/tui/ui.rs)