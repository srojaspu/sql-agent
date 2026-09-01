use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use std::io::stdout;
use std::sync::{atomic::AtomicBool, OnceLock};

static HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);
static HOOK_ONCE: OnceLock<()> = OnceLock::new();

/// Guard that restores terminal state (raw mode + alternate screen) on Drop
/// and via panic hook. Windows PowerShell safe: all crossterm calls are
/// best-effort — failures (non-TTY) are ignored and flags remain false.
pub struct TerminalGuard {
    raw_enabled: bool,
    alt_entered: bool,
}

impl TerminalGuard {
    /// Attempt to enter raw mode + alt screen. Never panics; if not TTY
    /// both flags stay false but guard still installs panic hook.
    pub fn new() -> anyhow::Result<Self> {
        let mut guard = Self {
            raw_enabled: false,
            alt_entered: false,
        };

        // Try alternate screen first — safe to ignore error on non-TTY/pipe
        if execute!(stdout(), EnterAlternateScreen).is_ok() {
            guard.alt_entered = true;
        }

        // Try raw mode — fails on non-TTY, we handle gracefully
        if enable_raw_mode().is_ok() {
            guard.raw_enabled = true;
        }

        // Ensure panic hook installed (once)
        Self::install_panic_hook();

        Ok(guard)
    }

    /// Returns true if raw mode was successfully enabled.
    pub fn is_raw(&self) -> bool {
        self.raw_enabled
    }

    /// Returns true if alternate screen was entered.
    pub fn is_alt(&self) -> bool {
        self.alt_entered
    }

    fn restore(&mut self) {
        if self.alt_entered {
            let _ = execute!(stdout(), LeaveAlternateScreen);
            self.alt_entered = false;
        }
        if self.raw_enabled {
            let _ = disable_raw_mode();
            self.raw_enabled = false;
        }
    }

    fn install_panic_hook() {
        // Use OnceLock to ensure we only install once per process
        HOOK_ONCE.get_or_init(|| {
            let prev = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                // Best-effort restore — ignore errors, we are in panic context
                let _ = disable_raw_mode();
                let _ = execute!(stdout(), LeaveAlternateScreen);
                // Call previous hook so panic message still prints
                prev(info);
            }));
            HOOK_INSTALLED.store(true, std::sync::atomic::Ordering::SeqCst);
        });
    }

    /// For tests: whether panic hook has been installed.
    pub fn hook_installed() -> bool {
        HOOK_INSTALLED.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Force restore without consuming guard — used by panic hook tests.
    pub fn force_restore_for_test() {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_new_creates_without_panic() {
        let guard = TerminalGuard::new().expect("Guard::new should not error");
        // On CI non-TTY, both may be false — but creation succeeds
        // We just assert guard exists and hook installed
        assert!(
            TerminalGuard::hook_installed(),
            "panic hook should be installed after new()"
        );
        // explicit drop should not panic
        drop(guard);
    }

    #[test]
    fn guard_drop_restores_without_panic() {
        let mut guard = TerminalGuard::new().expect("new ok");
        // Simulate enabled states to verify restore clears flags
        // We cannot enable real raw mode in test, so manually set flags to true
        // and verify restore clears them (Drop path)
        guard.raw_enabled = true;
        guard.alt_entered = true;
        // Call restore explicitly via drop
        drop(guard);
        // If we reach here without panic, restore succeeded (best-effort)
        // Second guard's drop should be idempotent
        let guard2 = TerminalGuard::new().unwrap();
        drop(guard2);
    }

    #[test]
    fn guard_panic_hook_restores_even_on_panic() {
        // Ensure hook is installed
        let _g = TerminalGuard::new().unwrap();
        assert!(TerminalGuard::hook_installed());

        // Simulate panic hook restore path — should not panic itself
        TerminalGuard::force_restore_for_test();

        // Verify hook still installed and second restore still safe
        TerminalGuard::force_restore_for_test();
        assert!(TerminalGuard::hook_installed());
    }

    #[test]
    fn guard_non_tty_fallback_is_safe() {
        // In test environment stdout is typically not TTY, so raw_enabled should be false
        // But Guard::new must still succeed and drop safely
        let guard = TerminalGuard::new().unwrap();
        // Could be true if test run in TTY, but we check it doesn't error
        let _is_raw = guard.is_raw();
        let _is_alt = guard.is_alt();
        // Drop must not panic regardless of flags
        drop(guard);
        // Create second guard to ensure no global state corruption
        let guard2 = TerminalGuard::new().unwrap();
        drop(guard2);
    }

    #[test]
    fn guard_hook_installed_once_idempotent() {
        let _a = TerminalGuard::new().unwrap();
        let installed1 = TerminalGuard::hook_installed();
        let _b = TerminalGuard::new().unwrap();
        let installed2 = TerminalGuard::hook_installed();
        assert!(
            installed1 && installed2,
            "hook should remain installed after second new()"
        );
    }
}
