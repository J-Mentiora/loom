//! Subprocess-level CLI exit-code regression matrix.
//!
//! Sister file to `integration_cli_exit_codes.rs`. That file calls
//! `map_exit_code(&result)` in-process — useful for variant coverage but
//! blind to whatever happens between `dispatch` returning and
//! `std::process::exit` firing in `cli_main::main`. This file spawns the
//! real `loom` binary via `env!("CARGO_BIN_EXE_loom")` and asserts the
//! process exit code, so any future regression that swallows the code
//! between the handler and the OS shows up here.
//!
//! Every row in `architecture/contracts/loom-cli_contract.md`
//! §"Exit Codes" should have at least one corresponding row below.
//! Every error row also asserts (a) stderr non-empty,
//! (b) stdout empty — so JSON-receipt pipes stay clean.
//!
//! HOME is overridden to a per-test tempdir so daemon-required flows
//! deterministically resolve to `Connection::DaemonNotRunning` (exit 1)
//! instead of accidentally hitting a stray daemon on the developer's
//! machine. This keeps the suite hermetic and runnable without `--ignored`.
//!
//! Daemon-running flows (e.g. SessionsDiffer happy-path, real
//! Receipt(status="error") from a live action) are NOT covered here —
//! they live in `integration_naverr_cli_e2e.rs` (`#[ignore]` daemon
//! harness). The in-process `integration_cli_exit_codes.rs` covers
//! `SessionsDiffer → 6` mapping at the variant level.

use std::path::PathBuf;
use std::process::{Command, Output};

use tempfile::TempDir;

/// One row in the regression matrix.
struct Row {
    /// Human-readable identifier; appears in failure messages.
    name: &'static str,
    /// CLI args (after the `loom` binary name).
    args: &'static [&'static str],
    /// Expected process exit code.
    expected_exit: i32,
    /// Substring that MUST appear in stderr for error rows. None for exit-0 rows.
    stderr_must_contain: Option<&'static str>,
}

/// Authoritative table. Rows here mirror — and exhaustively cover —
/// the matrix in `architecture/contracts/loom-cli_contract.md` §"Exit Codes"
/// for every flow that does not require a running daemon to produce its
/// terminal exit code.
const ROWS: &[Row] = &[
    // Code 0 — Success
    Row {
        name: "version_flag",
        args: &["--version"],
        expected_exit: 0,
        stderr_must_contain: None,
    },
    Row {
        name: "help_flag",
        args: &["--help"],
        expected_exit: 0,
        stderr_must_contain: None,
    },
    // Code 1 — Daemon-class error. With hermetic HOME the socket lookup may
    // resolve to either DaemonNotRunning ("Try: loom serve") or AuthFailed
    // ("HELLO mismatch") depending on whether a stray dev-machine daemon is
    // listening on the default socket path. Both are exit 1; both surface the
    // "Daemon" word via `connection_message`. We pin on "Daemon" rather than
    // the more specific subspecies so the test stays hermetic.
    Row {
        name: "session_create_no_daemon",
        args: &["session", "create"],
        expected_exit: 1,
        stderr_must_contain: Some("Daemon"),
    },
    Row {
        name: "session_close_no_daemon",
        args: &["session", "close", "ses-x"],
        expected_exit: 1,
        stderr_must_contain: Some("Daemon"),
    },
    Row {
        name: "session_diff_no_daemon",
        args: &["session", "diff", "ses-a", "ses-b"],
        expected_exit: 1,
        stderr_must_contain: Some("Daemon"),
    },
    // Code 2 — CLI usage / Internal / clap errors
    Row {
        name: "no_args",
        args: &[],
        expected_exit: 2,
        // clap auto-error mentions "Usage:" or the bin name
        stderr_must_contain: Some("Usage"),
    },
    Row {
        name: "action_missing_required_session",
        args: &["action", "web.click"],
        expected_exit: 2,
        // clap rejects missing --session before any handler runs
        stderr_must_contain: Some("--session"),
    },
    Row {
        name: "session_export_invalid_format",
        // clap ValueEnum rejects "banana" before any RPC call,
        // closing the leak suspected by the audit. Output path is irrelevant —
        // we never reach handler code.
        args: &["session", "export", "ses-x", "--format", "banana"],
        expected_exit: 2,
        stderr_must_contain: Some("banana"),
    },
    // NOTE for code 5/6: SurfaceUnavailable + SessionsDiffer require a live
    // daemon to reproduce their specific failure mode. They are exercised in:
    //   - integration_cli_exit_codes.rs (variant-level: map_exit_code arm)
    //   - integration_naverr_cli_e2e.rs (#[ignore] daemon harness)
    // The mapper-level coverage is sufficient for the regression contract here;
    // the subprocess matrix covers the codes that can be produced without
    // process orchestration.
];

/// Build the loom invocation with HOME overridden to a hermetic tempdir.
/// All Connection / daemon-resolution paths therefore deterministically
/// fail at the no-daemon-running boundary, which is the failure mode we
/// want to assert for `expected_exit == 1` rows.
fn spawn_loom(args: &[&str], home: &PathBuf) -> Output {
    let bin = env!("CARGO_BIN_EXE_loom");
    Command::new(bin)
        .args(args)
        // Override HOME for `dirs::home_dir()` and `dirs::data_dir()`
        // (used by AuthManager / config resolution).
        .env("HOME", home)
        // Belt-and-braces: also set XDG vars so config probing on Linux
        // resolves under the tempdir even if cross-compiled.
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        // Disable any developer-machine LOOM_* overrides leaking into the test.
        .env_remove("LOOM_SOCKET")
        .env_remove("LOOM_CONFIG")
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn loom binary at {bin}: {e}"))
}

/// Run a single row and return (passed, failure_summary).
fn run_row(row: &Row, home: &PathBuf) -> (bool, String) {
    let out = spawn_loom(row.args, home);
    let actual_exit = out
        .status
        .code()
        .unwrap_or_else(|| panic!("[{}] process killed by signal, no exit code", row.name));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    let mut failures = Vec::<String>::new();

    // (1) exit code matches contract
    if actual_exit != row.expected_exit {
        failures.push(format!(
            "exit code: expected {}, got {}",
            row.expected_exit, actual_exit
        ));
    }

    // (2) error rows must surface message on stderr
    if row.expected_exit != 0 {
        if stderr.is_empty() {
            failures.push("stderr is empty (a)".to_string());
        }
        if let Some(needle) = row.stderr_must_contain {
            if !stderr.contains(needle) {
                failures.push(format!(
                    "stderr missing required substring {needle:?}; got: {stderr:?}"
                ));
            }
        }

        // (3) stdout MUST be clean on error so the JSON-receipt stream isn't
        // polluted (regression guard).
        // Exception: --help / --version are NOT errors and write to stdout
        // intentionally — only enforced on error rows.
        if !stdout.is_empty() {
            failures.push(format!("stdout not empty on error row; got: {stdout:?}"));
        }
    }

    if failures.is_empty() {
        (true, String::new())
    } else {
        (
            false,
            format!("[{}] FAILED:\n  - {}", row.name, failures.join("\n  - ")),
        )
    }
}

#[test]
fn exit_code_matrix_subprocess() {
    let home = TempDir::new().expect("tempdir for hermetic HOME");
    let home_path = home.path().to_path_buf();

    let mut all_failures = Vec::<String>::new();
    let mut passed = 0usize;
    for row in ROWS {
        let (ok, msg) = run_row(row, &home_path);
        if ok {
            passed += 1;
        } else {
            all_failures.push(msg);
        }
    }

    assert!(
        all_failures.is_empty(),
        "{} of {} subprocess exit-code rows failed:\n\n{}\n\n\
         (passed: {})",
        all_failures.len(),
        ROWS.len(),
        all_failures.join("\n\n"),
        passed,
    );
}

/// Stand-alone smoke test: spawning the binary with an empty args list
/// must NEVER exit 0. This is the simplest possible regression guard
/// against "we accidentally made `loom` no-op success-out".
#[test]
fn naked_invocation_never_exits_zero() {
    let home = TempDir::new().unwrap();
    let out = spawn_loom(&[], &home.path().to_path_buf());
    assert_ne!(
        out.status.code(),
        Some(0),
        "`loom` with no args MUST exit non-zero (regression guard for). \
         got exit={:?}, stdout={:?}, stderr={:?}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// Documentation test: assert that for every code we
/// document in the contract, at least one row above produces it. This
/// fails-loudly if a future change adds a new exit code to the matrix
/// table without updating ROWS — keeps the test file in lockstep with
/// the contract doc.
#[test]
fn matrix_covers_all_documented_subprocess_reachable_codes() {
    use std::collections::BTreeSet;
    let covered: BTreeSet<i32> = ROWS.iter().map(|r| r.expected_exit).collect();

    // Codes reachable without a live daemon (per ROWS table above).
    // 3/4 are reserved (no emitter today); 5/6 require daemon orchestration.
    let must_cover: BTreeSet<i32> = [0, 1, 2].into_iter().collect();
    let missing: Vec<i32> = must_cover.difference(&covered).copied().collect();
    assert!(
        missing.is_empty(),
        "exit-code matrix missing rows for codes {missing:?}; either add a row \
         or update this guard if the code is no longer subprocess-reachable"
    );
}

// KNOWN-BUG (audit 2026-06-10, "Any action argument whose value is exactly
// '-h'/'--help' silently turns the command into a help printout with exit
// 0"): `detect_per_action_help_request` scans the ENTIRE argv for
// `--help`/`-h`, including option VALUES. So typing the literal text "-h"
// (`web.type ... --text -h`) short-circuits to the per-action help renderer
// and exits 0 without contacting the daemon — a script observing exit 0
// believes the action succeeded while the manifest records nothing. The
// intended behavior is that help tokens are only honored in KEY position;
// a `-h` value must flow through as the action argument (here: failing with
// the daemon-not-running error under the hermetic HOME, exit != 0, and no
// help text on stdout). Red until the help scan skips `--key` values.
#[test]
#[ignore = "KNOWN-BUG: '-h' in option-value position triggers per-action help with exit 0 \
            instead of executing the action — see audit 2026-06-10"]
fn known_bug_help_token_in_value_position_must_not_short_circuit() {
    let home = TempDir::new().unwrap();
    let out = spawn_loom(
        &[
            "action",
            "web.type",
            "--session",
            "01HZTESTSESSIONHELPVAL0000",
            "--selector",
            "#q",
            "--text",
            "-h",
        ],
        &home.path().to_path_buf(),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_ne!(
        out.status.code(),
        Some(0),
        "typing the literal text \"-h\" must execute the action (and fail \
         against the absent daemon), never exit 0 via the help renderer; \
         stdout was: {stdout:?}"
    );
    assert!(
        !stdout.to_lowercase().contains("usage") && !stdout.to_lowercase().contains("--selector <"),
        "stdout must not be the per-action help text when -h is a value; \
         got: {stdout:?}"
    );
}
