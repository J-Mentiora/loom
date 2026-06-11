//! `loom session validate` output-contract golden tests (CLI-level).
//!
//! Regression: `--json` used to print a bare `PASS`/`FAIL` string — not JSON —
//! so scripted consumers (`... --json | jq .passed`) had to string-match and
//! `reasons[]` was unreachable on the happy path. The contract is:
//! - `--json` emits the full wire ValidationResult as one canonical-JSON
//!   object: `{"passed":…,"reasons":[…],"session_id":…}`.
//! - exit 0 when passed, exit 1 (receipt-error class) when failed — with the
//!   synthetic `session-validation-failed` receipt on stderr, never stdout.
//! - `--pretty` keeps the human PASS/FAIL rendering.
//!
//! Sessions are fabricated directly on disk under the daemon's
//! `<data_root>/sessions/<sid>/manifest.wal` — validation is a pure
//! chain + blob check, so no browser is needed (same fixture approach as
//! loom-core's `session_export_behavior.rs`).

mod common;

use common::daemon_test_harness::DaemonTestHarness;
use loom_core::manifest_writer::ManifestEntry;
use std::path::Path;

/// 26-char lowercase-alnum session ids (the daemon's path-traversal gate
/// rejects anything else before validation runs).
const SID_PASS: &str = "01hzvalidatepass0000000000";
const SID_FAIL: &str = "01hzvalidatefail0000000000";

fn header_entry(sid: &str) -> ManifestEntry {
    ManifestEntry::Header {
        session_id: sid.to_string(),
        started_at_ms: 0,
        prev_hash: None,
        budgets: None,
        capture_policy: None,
        seed: None,
        determinism_enabled: None,
    }
}

/// Write `<sessions_root>/<sid>/manifest.wal` with the given entries.
fn write_wal(sessions_root: &Path, sid: &str, entries: &[ManifestEntry]) {
    let dir = sessions_root.join(sid);
    std::fs::create_dir_all(&dir).unwrap();
    let lines: Vec<String> = entries
        .iter()
        .map(|e| serde_json::to_string(e).unwrap())
        .collect();
    std::fs::write(dir.join("manifest.wal"), lines.join("\n")).unwrap();
}

/// Harness with the data root pinned inside the hermetic home so the test
/// can fabricate sessions where the daemon will look for them. The CLI's
/// auth dir must follow the daemon's (`<data_root>/auth`) for HELLO. The
/// reaper sweep is parked (long interval) so it can't quarantine the
/// deliberately-corrupt fixture mid-test.
fn pinned_harness() -> (DaemonTestHarness, std::path::PathBuf) {
    let h0 = DaemonTestHarness::new();
    let data_root = h0.home().join("loom-data");
    let sessions_root = data_root.join("sessions");
    std::fs::create_dir_all(&sessions_root).unwrap();
    let h = h0
        .env("LOOM_DATA_ROOT", &data_root)
        .env("LOOM_AUTH_DIR", data_root.join("auth"))
        .env("LOOM_REAPER_SWEEP_SECS", "300");
    (h, sessions_root)
}

#[test]
fn validate_json_emits_full_validation_result_on_pass_and_fail() {
    let (mut h, sessions_root) = pinned_harness();
    h.start();

    // Fixtures are written AFTER start: the daemon's startup crash-recovery
    // sweep quarantines broken-chain sessions, which would turn the fail
    // path into session_not_found before validate ever sees it.
    //
    // Pass path: a header-only WAL has an intact (trivial) chain and no blobs.
    write_wal(&sessions_root, SID_PASS, &[header_entry(SID_PASS)]);
    // Fail path: an ActionReceipt whose prev_hash matches nothing → chain break.
    write_wal(
        &sessions_root,
        SID_FAIL,
        &[
            header_entry(SID_FAIL),
            ManifestEntry::ActionReceipt {
                action_id: 1,
                emitted_at_ms: 1000,
                receipt_canonical_bytes: b"{}".to_vec(),
                prev_hash: "f".repeat(64),
            },
        ],
    );

    // --- pass path: exit 0, stdout is one canonical-JSON ValidationResult ---
    let out = h
        .loom_command()
        .args(["session", "validate", SID_PASS, "--json"])
        .output()
        .expect("run loom session validate (pass)");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "pass-path validate must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("--json stdout must parse as JSON ({e}); got: {stdout:?}"));
    assert_eq!(v["passed"], serde_json::json!(true), "got: {v}");
    assert_eq!(v["session_id"], serde_json::json!(SID_PASS), "got: {v}");
    assert_eq!(v["reasons"], serde_json::json!([]), "got: {v}");

    // --- fail path: exit 1, stdout STILL one parseable ValidationResult ---
    let out = h
        .loom_command()
        .args(["session", "validate", SID_FAIL, "--json"])
        .output()
        .expect("run loom session validate (fail)");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "fail-path validate must exit 1; stdout: {stdout:?} stderr: {stderr:?}"
    );
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("fail-path --json stdout must parse as JSON ({e}); got: {stdout:?}")
    });
    assert_eq!(v["passed"], serde_json::json!(false), "got: {v}");
    assert_eq!(v["session_id"], serde_json::json!(SID_FAIL), "got: {v}");
    let reasons = v["reasons"].as_array().expect("reasons must be an array");
    assert!(
        !reasons.is_empty(),
        "fail-path reasons[] must carry the chain break; got: {v}"
    );
    // The synthetic error receipt stays on stderr, never polluting stdout.
    assert!(
        stderr.contains("session-validation-failed"),
        "stderr must carry the typed receipt error; got: {stderr:?}"
    );
}

#[test]
fn validate_pretty_keeps_human_pass_fail_rendering() {
    let (mut h, sessions_root) = pinned_harness();
    h.start();
    write_wal(&sessions_root, SID_PASS, &[header_entry(SID_PASS)]);

    let out = h
        .loom_command()
        .args(["session", "validate", SID_PASS, "--pretty", "--no-color"])
        .output()
        .expect("run loom session validate --pretty");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "pretty pass-path must exit 0");
    assert!(
        stdout.contains("PASS"),
        "--pretty must keep the human PASS rendering; got: {stdout:?}"
    );
    // And the pretty path must NOT be a bare JSON object.
    assert!(
        serde_json::from_str::<serde_json::Value>(stdout.trim()).is_err(),
        "--pretty output should be prose, not JSON; got: {stdout:?}"
    );
}
