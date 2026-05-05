// Output mode resolved from --quiet/--json/--pretty + TTY detection.
// Per D-7 precedence: quiet > json > pretty > auto-detect.
//
// `Json` is the canonical-JSON path pinned byte-for-byte.
// `PrettyCurated` routes through the curated registry; `PrettyFallback`
// is reached only internally when no curated renderer matches OR when a
// curated renderer returns an error (D-23).

use serde::{Deserialize, Serialize};

// Canonical JSON is the default when nothing else is
// resolvable (e.g. compiled defaults before flag/env resolution).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputMode {
    Quiet,
    #[default]
    Json,
    PrettyCurated,
    PrettyFallback,
}

impl OutputMode {
    /// Resolve from raw flag inputs per D-7 precedence:
    /// `--quiet > --json > --pretty > auto-detect (TTY=PrettyCurated, pipe=Json)`.
    pub fn resolve(quiet: bool, json: bool, pretty: bool, stdout_is_terminal: bool) -> Self {
        if quiet {
            return OutputMode::Quiet;
        }
        if json {
            return OutputMode::Json;
        }
        if pretty {
            return OutputMode::PrettyCurated;
        }
        if stdout_is_terminal {
            OutputMode::PrettyCurated
        } else {
            OutputMode::Json
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_wins_everything() {
        assert_eq!(
            OutputMode::resolve(true, true, true, true),
            OutputMode::Quiet
        );
        assert_eq!(
            OutputMode::resolve(true, false, false, false),
            OutputMode::Quiet
        );
    }

    #[test]
    fn json_beats_pretty_and_auto() {
        assert_eq!(
            OutputMode::resolve(false, true, true, true),
            OutputMode::Json
        );
        assert_eq!(
            OutputMode::resolve(false, true, false, true),
            OutputMode::Json
        );
    }

    #[test]
    fn pretty_overrides_pipe() {
        // --pretty into a pipe → pretty (matches the "force human" intent)
        assert_eq!(
            OutputMode::resolve(false, false, true, false),
            OutputMode::PrettyCurated
        );
    }

    #[test]
    fn auto_detect_tty_yields_pretty() {
        assert_eq!(
            OutputMode::resolve(false, false, false, true),
            OutputMode::PrettyCurated
        );
    }

    #[test]
    fn auto_detect_pipe_yields_json() {
        assert_eq!(
            OutputMode::resolve(false, false, false, false),
            OutputMode::Json
        );
    }
}
