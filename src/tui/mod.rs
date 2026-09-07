pub mod app;
pub mod commands;
pub mod draw;
pub mod events;
pub mod guard;
pub mod input;
pub mod state;
pub mod ui;

pub use app::AppState;
pub use events::AppEvent;
pub use guard::TerminalGuard;

// Helper to determine if we should launch TUI (pure logic for testing)
pub fn should_use_tui(no_tui: bool, stdout_is_tty: bool, stdin_is_tty: bool) -> bool {
    !no_tui && stdout_is_tty && stdin_is_tty
}

/// Check if current process is interactive (both stdout and stdin are TTY)
pub fn is_interactive(no_tui: bool) -> bool {
    use std::io::IsTerminal;
    !no_tui && std::io::stdout().is_terminal() && std::io::stdin().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_use_tui_true_when_tty_and_not_no_tui() {
        assert!(should_use_tui(false, true, true));
    }

    #[test]
    fn should_use_tui_false_when_no_tui_flag() {
        assert!(!should_use_tui(true, true, true));
    }

    #[test]
    fn should_use_tui_false_when_not_tty() {
        assert!(!should_use_tui(false, false, true));
        assert!(!should_use_tui(false, true, false));
        assert!(!should_use_tui(false, false, false));
        // triangulation: pipe via non-tty should fallback
        assert!(!should_use_tui(false, false, false));
    }

    #[test]
    fn should_use_tui_pipe_fallback() {
        // Simulate `echo "hi" | cargo run` -> stdin not tty, stdout piped not tty
        assert!(!should_use_tui(false, false, false));
        // Bare run in TTY -> true
        assert!(should_use_tui(false, true, true));
        // Forced --no-tui even with TTY -> false
        assert!(!should_use_tui(true, true, true));
    }
}
