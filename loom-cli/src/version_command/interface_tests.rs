// Interface tests for `VersionCommand`. Verifies SR-CLI-01 RPC bypass
// (`print` does NOT take an `RpcClient`) plus AC-VER-02 build provenance
// (`LOOM_VERSION` includes git SHA + build date).

use super::version_command::{print, resolve, VersionInfo, LOOM_VERSION};
use crate::CliError;

#[test]
fn version_info_carries_version_git_sha_build_date_target() {
    let v = VersionInfo {
        version: env!("CARGO_PKG_VERSION").into(),
        git_sha: "abc1234".into(),
        build_date: "2026-05-04".into(),
        target: "aarch64-apple-darwin".into(),
    };
    assert_eq!(v.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(v.build_date, "2026-05-04");
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
        version: env!("CARGO_PKG_VERSION").into(),
        git_sha: "abc".into(),
        build_date: "2026-05-04".into(),
        target: "aarch64-apple-darwin".into(),
    };
    let s = serde_json::to_string(&v).unwrap();
    assert!(s.contains("\"version\""));
    assert!(s.contains("\"git_sha\""));
    assert!(s.contains("\"build_date\""));
    assert!(s.contains("\"target\""));
}

// === AC-VER-02: --version output includes git sha + build date ===

#[test]
fn loom_version_starts_with_cargo_pkg_version() {
    let prefix = concat!(env!("CARGO_PKG_VERSION"), " (");
    assert!(
        LOOM_VERSION.starts_with(prefix),
        "LOOM_VERSION {LOOM_VERSION:?} should start with {prefix:?}"
    );
}

#[test]
fn loom_version_has_provenance_parens() {
    assert!(
        LOOM_VERSION.ends_with(')'),
        "LOOM_VERSION {LOOM_VERSION:?} should end with ')'"
    );
    let open = LOOM_VERSION.find('(').expect("LOOM_VERSION missing '('");
    let close = LOOM_VERSION.rfind(')').expect("LOOM_VERSION missing ')'");
    let inside = &LOOM_VERSION[open + 1..close];
    let parts: Vec<&str> = inside.split(' ').collect();
    assert_eq!(
        parts.len(),
        2,
        "LOOM_VERSION provenance section {inside:?} should be `<sha> <date>`"
    );
    let (sha, date) = (parts[0], parts[1]);
    assert!(!sha.is_empty(), "sha empty");
    assert!(!date.is_empty(), "date empty");
}

#[test]
fn resolve_build_date_is_set() {
    let v = resolve();
    assert!(!v.build_date.is_empty(), "build_date empty");
    // Either YYYY-MM-DD or "unknown" — both honored shapes from build.rs.
    assert!(
        v.build_date == "unknown" || (v.build_date.len() == 10 && v.build_date.as_bytes()[4] == b'-' && v.build_date.as_bytes()[7] == b'-'),
        "build_date {:?} should be YYYY-MM-DD or 'unknown'",
        v.build_date
    );
}
