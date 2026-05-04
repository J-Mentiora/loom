// Re-export of the locked Phase 5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-cli/modules/VersionCommand/interface_tests.rs` instead.
// Interface tests for `VersionCommand`. Verifies SR-CLI-01 RPC bypass
// by ensuring `print` does NOT take an `RpcClient` parameter.

use super::version_command::{print, resolve, VersionInfo};
use crate::CliError;

#[test]
fn version_info_carries_version_git_sha_target() {
    let v = VersionInfo {
        version: "1.0.0".into(),
        git_sha: "abc123".into(),
        target: "aarch64-apple-darwin".into(),
    };
    assert_eq!(v.version, "1.0.0");
}

#[test]
fn resolve_signature_is_pure() {
    fn _ck() -> VersionInfo {
        resolve()
    }
    let _ = _ck;
}

// === SR-CLI-01: bypasses RpcClient entirely ===
//
// Encoded structurally: `print` is parameterless. If it ever needs an
// `RpcClient`, this test should be reviewed alongside an audit of the
// AC-NFR-PERF latency budget.
#[test]
fn print_takes_no_rpc_client() {
    fn _ck() -> Result<(), CliError> {
        print()
    }
    let _ = _ck;
}

#[test]
fn version_info_serialises_to_json_object() {
    let v = VersionInfo {
        version: "1.0.0".into(),
        git_sha: "abc".into(),
        target: "aarch64-apple-darwin".into(),
    };
    let s = serde_json::to_string(&v).unwrap();
    assert!(s.contains("\"version\""));
    assert!(s.contains("\"git_sha\""));
    assert!(s.contains("\"target\""));
}
