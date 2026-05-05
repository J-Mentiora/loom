// ANSI escape helpers for the pretty renderer.
//
// Surface is intentionally tiny — pretty output uses at most six SGR codes.
// Hand-rolled rather than pulling in a color crate (D-4): the workspace's
// `clap` dep already transitively pulls `anstyle` so a runner-up exists, but
// the surface is small enough that a few const strs and one `paint` helper
// keep the audit trail trivial. NO_COLOR / TERM=dumb / per-stream
// IsTerminal compliance lives in the resolver (cli_main + cli_config),
// not here.

pub const RESET: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";
pub const DIM: &str = "\x1b[2m";
pub const RED: &str = "\x1b[31m";
pub const GREEN: &str = "\x1b[32m";
pub const YELLOW: &str = "\x1b[33m";
pub const CYAN: &str = "\x1b[36m";

/// Wrap `text` with `code` and `RESET` when `enabled`; otherwise return
/// `text` unchanged. Allocates a new `String` only on the colored path so
/// the canonical-JSON / non-TTY hot path stays alloc-free.
pub fn paint(text: &str, code: &str, enabled: bool) -> String {
    if enabled {
        let mut out = String::with_capacity(text.len() + code.len() + RESET.len());
        out.push_str(code);
        out.push_str(text);
        out.push_str(RESET);
        out
    } else {
        text.to_string()
    }
}

/// Concatenate two SGR codes (e.g. `BOLD` + `RED`) into a single escape so
/// `paint(text, &combine(BOLD, RED), enabled)` emits one open + one RESET.
/// Allocation is unavoidable; only used at the call site, not in a hot loop.
pub fn combine(a: &str, b: &str) -> String {
    // Both `a` and `b` are of the form "\x1b[<n>m"; splice them into
    // "\x1b[<a>;<b>m" so the terminal applies both at once.
    fn strip(s: &str) -> &str {
        s.trim_start_matches("\x1b[").trim_end_matches('m')
    }
    format!("\x1b[{};{}m", strip(a), strip(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paint_disabled_returns_text_unchanged() {
        assert_eq!(paint("hi", RED, false), "hi");
    }

    #[test]
    fn paint_enabled_wraps_with_reset() {
        let out = paint("hi", RED, true);
        assert!(out.starts_with(RED));
        assert!(out.ends_with(RESET));
        assert!(out.contains("hi"));
    }

    #[test]
    fn combine_merges_two_codes() {
        let merged = combine(BOLD, RED);
        // Should be of the form "\x1b[1;31m"
        assert!(merged.starts_with("\x1b["));
        assert!(merged.ends_with('m'));
        assert!(merged.contains(';'));
        assert!(merged.contains('1'));
        assert!(merged.contains("31"));
    }
}
