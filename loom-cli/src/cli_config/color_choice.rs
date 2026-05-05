// Per-stream color choice resolver per D-22 / D-20.
//
// Precedence ladder for each stream:
//   1. --color=always              → enabled
//   2. --color=never / --no-color  → disabled
//   3. CLICOLOR_FORCE non-empty    → enabled
//   4. NO_COLOR non-empty (D-16)   → disabled
//   5. CLICOLOR=0                  → disabled
//   6. TERM=dumb                   → disabled
//   7. IsTerminal(stream)          → that stream's verdict
//
// Stdout and stderr are resolved independently (D-20) so a piped stdout
// can still ship colored stderr error prose.

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[clap(rename_all = "lowercase")]
pub enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

/// Resolve color enablement for one stream. `is_tty` is the
/// `std::io::IsTerminal` verdict for that stream.
pub fn resolve_color(choice: ColorChoice, is_tty: bool) -> bool {
    match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => resolve_color_env(is_tty),
    }
}

/// Auto-mode: env-var ladder + IsTerminal fallback.
fn resolve_color_env(is_tty: bool) -> bool {
    if std::env::var("CLICOLOR_FORCE")
        .map(|s| !s.is_empty() && s != "0")
        .unwrap_or(false)
    {
        return true;
    }
    if std::env::var("NO_COLOR")
        .map(|s| !s.is_empty())
        .unwrap_or(false)
    {
        return false;
    }
    if std::env::var("CLICOLOR").map(|s| s == "0").unwrap_or(false) {
        return false;
    }
    if std::env::var("TERM").ok().as_deref() == Some("dumb") {
        return false;
    }
    is_tty
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn clear() {
        std::env::remove_var("NO_COLOR");
        std::env::remove_var("CLICOLOR_FORCE");
        std::env::remove_var("CLICOLOR");
        std::env::remove_var("TERM");
    }

    #[test]
    fn always_forces_color_even_in_pipe() {
        let _g = ENV_LOCK.lock().unwrap();
        clear();
        assert!(resolve_color(ColorChoice::Always, false));
    }

    #[test]
    fn never_disables_even_at_tty() {
        let _g = ENV_LOCK.lock().unwrap();
        clear();
        assert!(!resolve_color(ColorChoice::Never, true));
    }

    #[test]
    fn auto_clicolor_force_overrides_pipe() {
        let _g = ENV_LOCK.lock().unwrap();
        clear();
        std::env::set_var("CLICOLOR_FORCE", "1");
        assert!(resolve_color(ColorChoice::Auto, false));
        clear();
    }

    #[test]
    fn auto_no_color_disables() {
        let _g = ENV_LOCK.lock().unwrap();
        clear();
        std::env::set_var("NO_COLOR", "1");
        assert!(!resolve_color(ColorChoice::Auto, true));
        clear();
    }

    #[test]
    fn auto_no_color_empty_does_not_disable() {
        let _g = ENV_LOCK.lock().unwrap();
        clear();
        std::env::set_var("NO_COLOR", "");
        // Per spec D-16: empty is NOT a disable signal.
        assert!(resolve_color(ColorChoice::Auto, true));
        clear();
    }

    #[test]
    fn auto_clicolor_zero_disables() {
        let _g = ENV_LOCK.lock().unwrap();
        clear();
        std::env::set_var("CLICOLOR", "0");
        assert!(!resolve_color(ColorChoice::Auto, true));
        clear();
    }

    #[test]
    fn auto_term_dumb_disables() {
        let _g = ENV_LOCK.lock().unwrap();
        clear();
        std::env::set_var("TERM", "dumb");
        assert!(!resolve_color(ColorChoice::Auto, true));
        clear();
    }

    #[test]
    fn auto_falls_through_to_is_tty() {
        let _g = ENV_LOCK.lock().unwrap();
        clear();
        assert!(resolve_color(ColorChoice::Auto, true));
        assert!(!resolve_color(ColorChoice::Auto, false));
    }

    #[test]
    fn force_beats_no_color() {
        let _g = ENV_LOCK.lock().unwrap();
        clear();
        std::env::set_var("CLICOLOR_FORCE", "1");
        std::env::set_var("NO_COLOR", "1");
        // CLICOLOR_FORCE wins per ladder ordering.
        assert!(resolve_color(ColorChoice::Auto, false));
        clear();
    }
}
