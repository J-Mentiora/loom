// Interface tests for `ImportCommands`. Verifies the wire-size
// pre-flight (regression: traces whose hex-in-JSON frame exceeded the
// daemon's 16 MiB frame cap were sent anyway; the dropped connection
// surfaced as a misleading "no daemon running" error).

use super::import_commands::{import_playwright, max_importable_trace_bytes, ImportPlaywrightArgs};
use crate::rpc_client::{RpcClient, RpcClientConfig};
use crate::CliError;

fn test_client(dir: &std::path::Path) -> RpcClient {
    // Nonexistent socket + empty auth dir: any attempt to actually
    // connect fails with a Connection error, so a Receipt error proves
    // the handler bailed BEFORE touching the daemon.
    RpcClient::new(RpcClientConfig {
        socket_path: dir.join("no-daemon.sock"),
        auth_dir: dir.join("no-auth"),
        request_timeout: std::time::Duration::from_millis(100),
    })
}

fn test_cfg() -> crate::CliConfig {
    crate::cli_config::cli_config::compiled_defaults()
}

#[test]
fn max_importable_trace_bytes_is_just_under_half_the_frame_cap() {
    let max = max_importable_trace_bytes();
    let cap = loom_rpc::frame_handler::frame_handler::MAX_FRAME_BYTES;
    // Hex doubles the payload, plus envelope headroom.
    assert!(max < cap / 2, "hex doubling must be accounted for");
    assert!(
        max > cap / 2 - 4096,
        "headroom should be small (envelope-sized), got max={max}"
    );
}

#[tokio::test]
async fn oversized_trace_fails_with_typed_receipt_before_connecting() {
    let dir = tempfile::tempdir().unwrap();
    let trace_path = dir.path().join("trace.zip");
    // One byte over the cap — must be rejected client-side.
    let oversized = vec![0u8; max_importable_trace_bytes() + 1];
    std::fs::write(&trace_path, &oversized).unwrap();

    let rpc = test_client(dir.path());
    let err = import_playwright(&rpc, &test_cfg(), ImportPlaywrightArgs { trace_path })
        .await
        .expect_err("oversized trace must fail");
    match err {
        CliError::Receipt(v) => {
            assert_eq!(v["code"], "trace_too_large", "got: {v}");
            assert_eq!(
                v["data"]["max_trace_bytes"],
                serde_json::json!(max_importable_trace_bytes())
            );
            assert!(
                v["message"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("frame cap"),
                "message must explain the wire limit: {v}"
            );
        }
        other => panic!(
            "expected CliError::Receipt(trace_too_large), not a connection \
             error — the size check must run before connecting; got: {other:?}"
        ),
    }
}

#[tokio::test]
async fn trace_at_the_cap_passes_the_size_check() {
    let dir = tempfile::tempdir().unwrap();
    let trace_path = dir.path().join("trace.zip");
    // Exactly at the cap: the pre-flight must NOT trip — the next failure
    // is the (intentionally dead) connection, proving the check passed.
    let at_cap = vec![0u8; max_importable_trace_bytes()];
    std::fs::write(&trace_path, &at_cap).unwrap();

    let rpc = test_client(dir.path());
    let err = import_playwright(&rpc, &test_cfg(), ImportPlaywrightArgs { trace_path })
        .await
        .expect_err("no daemon is running, so the RPC must fail");
    assert!(
        matches!(err, CliError::Connection(_)),
        "an at-cap trace must reach the connection stage, got: {err:?}"
    );
}
