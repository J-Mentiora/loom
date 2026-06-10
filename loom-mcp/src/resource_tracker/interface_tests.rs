// Interface tests for `ResourceTracker`. Verifies URI-scheme round-trip,
// TTL knob, and the active-session filter shape.

use super::resource_tracker::{
    Resource, ResourceContents, ResourceTracker, SessionInfo, DEFAULT_TTL, SESSION_URI_PREFIX,
    SESSION_URI_SUFFIX,
};
use std::time::Duration;

// === URI scheme + round-trip ===

#[test]
fn uri_scheme_is_loom_session_manifest() {
    assert_eq!(SESSION_URI_PREFIX, "loom://session/");
    assert_eq!(SESSION_URI_SUFFIX, "/manifest");
}

#[test]
fn uri_for_session_matches_loom_session_ulid_manifest() {
    let u = ResourceTracker::uri_for_session("01HZABC");
    assert_eq!(u, "loom://session/01HZABC/manifest");
}

#[test]
fn ulid_from_uri_inverts_uri_for_session() {
    let id = "01HZXYZ";
    let u = ResourceTracker::uri_for_session(id);
    assert_eq!(ResourceTracker::ulid_from_uri(&u), Some(id));
}

#[test]
fn ulid_from_uri_returns_none_for_malformed_uri() {
    assert!(ResourceTracker::ulid_from_uri("https://example.com").is_none());
    assert!(ResourceTracker::ulid_from_uri("loom://session/01HZ/wrong").is_none());
}

#[test]
fn resource_from_session_uses_application_json_mime() {
    let info = SessionInfo {
        session_id: "01HZ".into(),
        status: "active".into(),
        created_at_ms: 1_718_000_000_000,
        reason: None,
    };
    let r = ResourceTracker::resource_from_session(&info);
    assert_eq!(r.mime_type, "application/json");
    assert_eq!(r.uri, "loom://session/01HZ/manifest");
    assert!(r.name.contains("01HZ"));
}

// === Resource serialisation: camelCase mimeType ===

#[test]
fn resource_serialises_mime_type_as_camel_case() {
    let r = Resource {
        uri: "loom://session/x/manifest".into(),
        name: "Session x".into(),
        mime_type: "application/json".into(),
    };
    let s = serde_json::to_string(&r).unwrap();
    assert!(s.contains("\"mimeType\""), "got {s}");
}

#[test]
fn resource_contents_serialises_mime_type_as_camel_case() {
    let rc = ResourceContents {
        uri: "loom://session/x/manifest".into(),
        mime_type: "application/json".into(),
        text: Some("{}".into()),
        blob: None,
    };
    let s = serde_json::to_string(&rc).unwrap();
    assert!(s.contains("\"mimeType\""), "got {s}");
}

// === TTL knob: default 30 s, customisable ===

#[test]
fn default_ttl_is_30_seconds() {
    assert_eq!(DEFAULT_TTL, Duration::from_secs(30));
}

#[test]
fn with_ttl_signature() {
    fn _ck(
        rpc: std::sync::Arc<crate::rpc_client::RpcClient>,
        d: Duration,
    ) -> std::sync::Arc<ResourceTracker> {
        ResourceTracker::with_ttl(rpc, d)
    }
    let _ = _ck;
}

// === list / read signatures ===

#[test]
fn list_returns_vec_resource_or_loom_error() {
    fn _ck(
        t: std::sync::Arc<ResourceTracker>,
    ) -> Box<dyn std::future::Future<Output = Result<Vec<Resource>, loom_rpc::error::LoomError>>>
    {
        Box::new(async move { t.list().await })
    }
    let _ = _ck;
}

#[test]
fn read_takes_uri_returns_resource_contents() {
    fn _ck(
        t: std::sync::Arc<ResourceTracker>,
        u: &str,
    ) -> Box<
        dyn std::future::Future<Output = Result<ResourceContents, loom_rpc::error::LoomError>> + '_,
    > {
        let u = u.to_owned();
        Box::new(async move { t.read(&u).await })
    }
    let _ = _ck;
}

// === SessionInfo mirror matches the daemon's wire shape ===

#[test]
fn session_info_mirror_has_wire_fields() {
    let s = SessionInfo {
        session_id: "01H".into(),
        status: "active".into(),
        created_at_ms: 1_718_000_000_000,
        reason: None,
    };
    assert_eq!(s.status, "active");
    assert_eq!(s.session_id, "01H");
}

// Regression (mcp-resources): the mirror must deserialise the REAL daemon
// wire JSON. The fixture is exactly what loom-rpc's core_service_adapter
// SessionInfo serialises (`session_id`/`status`/`created_at_ms`, optional
// `reason`); the old mirror required `id`/`manifest_path`, so every
// non-empty session.list failed to parse and resources/list went empty.
#[test]
fn session_info_deserializes_daemon_wire_json() {
    let wire = json!([
        {
            "session_id": "01HZAAAAAAAAAAAAAAAAAAAAAA",
            "status": "active",
            "created_at_ms": 1_718_000_000_000u64
        },
        {
            "session_id": "01HZBBBBBBBBBBBBBBBBBBBBBB",
            "status": "aborted",
            "created_at_ms": 1_718_000_001_000u64,
            "reason": "operator abort"
        }
    ]);
    let sessions: Vec<SessionInfo> = serde_json::from_value(wire)
        .expect("mirror must deserialise the daemon's SessionInfo wire JSON");
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].session_id, "01HZAAAAAAAAAAAAAAAAAAAAAA");
    assert_eq!(sessions[0].status, "active");
    assert_eq!(sessions[0].reason, None);
    assert_eq!(sessions[1].reason.as_deref(), Some("operator abort"));
}

// Belt-and-braces: a true round-trip through the loom-rpc wire type, so the
// mirror can never silently drift from the daemon's serialisation again.
#[test]
fn session_info_round_trips_loom_rpc_wire_type() {
    let wire = loom_rpc::core_service_adapter::SessionInfo {
        session_id: "01HZCCCCCCCCCCCCCCCCCCCCCC".into(),
        status: "active".into(),
        created_at_ms: 1_718_000_002_000,
        reason: None,
    };
    let raw = serde_json::to_value(&wire).unwrap();
    let mirrored: SessionInfo =
        serde_json::from_value(raw).expect("mirror must accept loom-rpc's serialised SessionInfo");
    assert_eq!(mirrored.session_id, wire.session_id);
    assert_eq!(mirrored.created_at_ms, wire.created_at_ms);
}

// === cache_snapshot lets us assert TTL behaviour without poking internals ===

#[test]
fn cache_snapshot_signature() {
    fn _ck(
        t: &ResourceTracker,
    ) -> Box<dyn std::future::Future<Output = Option<(Vec<Resource>, std::time::Instant)>> + '_>
    {
        Box::new(async move { t.cache_snapshot().await })
    }
    let _ = _ck;
}

// === Reproduce-first (mcp-screenshot-delivery): a client holding a
// screenshot hash must be able to resolve it to PNG bytes via a
// `loom://blob/<hash>` resource. Today `read()` only understands
// `loom://session/.../manifest`, so this FAILS until the blob resource
// lands. ===

use crate::rpc_client::{JsonRpcCaller, RpcClient};
use loom_rpc::error::LoomError;
use serde_json::json;
use std::sync::Arc;

const BLOB_PNG_HEX: &str = "89504e470d0a1a0a0000000d49484452";
const BLOB_HASH: &str = "2222222222222222222222222222222222222222222222222222222222222222";

struct BlobFakeCaller;

#[async_trait::async_trait]
impl JsonRpcCaller for BlobFakeCaller {
    async fn raw_call(
        &self,
        method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, LoomError> {
        match method {
            "content.get" => Ok(json!({
                "artifact_ref": BLOB_HASH,
                "data_hex": BLOB_PNG_HEX,
                "size_bytes": 16u64
            })),
            other => Err(LoomError::new(
                loom_rpc::error::LoomErrorCode::InvalidArgument,
                format!("unexpected method in fake: {other}"),
            )),
        }
    }
}

#[tokio::test]
async fn read_blob_uri_returns_png_bytes() {
    let rpc: Arc<RpcClient> = RpcClient::with_caller_for_test(Box::new(BlobFakeCaller));
    let tracker = ResourceTracker::new(rpc);

    let uri = format!("loom://blob/{BLOB_HASH}");
    let contents: ResourceContents = tracker
        .read(&uri)
        .await
        .expect("resources/read of a loom://blob/<hash> URI must succeed");

    let v = serde_json::to_value(&contents).unwrap();
    assert_eq!(
        v.get("mimeType").and_then(|m| m.as_str()),
        Some("image/png"),
        "blob resource must be served as image/png"
    );
    let blob = v
        .get("blob")
        .and_then(|b| b.as_str())
        .expect("blob resource must carry a base64 `blob` field per MCP spec");
    assert!(
        blob.starts_with("iVBORw0KGg"),
        "blob must be base64 of a PNG (prefix iVBORw0KGg); got {:?}",
        &blob.chars().take(12).collect::<String>()
    );
}

// === Regression (mcp-resources): read() must send the param key the daemon
// router actually reads. Every session.* router arm extracts "session_id"
// with unwrap_or(""); the old {"id": ...} payload made the daemon inspect
// the empty session on every manifest read. ===

const SESSION_ULID: &str = "01HZDDDDDDDDDDDDDDDDDDDDDD";

struct InspectFakeCaller;

#[async_trait::async_trait]
impl JsonRpcCaller for InspectFakeCaller {
    async fn raw_call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, LoomError> {
        match method {
            "session.inspect" => {
                // Mirror the daemon router's extraction exactly:
                // params.get("session_id").and_then(|v| v.as_str()).unwrap_or("")
                let sid = params
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if sid != SESSION_ULID {
                    return Err(LoomError::new(
                        loom_rpc::error::LoomErrorCode::InvalidArgument,
                        format!("router would inspect {sid:?}, not the requested session"),
                    ));
                }
                Ok(json!({
                    "session_id": sid,
                    "at_action": null,
                    "manifest_summary": { "action_count": 3 }
                }))
            }
            other => Err(LoomError::new(
                loom_rpc::error::LoomErrorCode::InvalidArgument,
                format!("unexpected method in fake: {other}"),
            )),
        }
    }
}

#[tokio::test]
async fn read_session_manifest_sends_session_id_param_key() {
    let rpc: Arc<RpcClient> = RpcClient::with_caller_for_test(Box::new(InspectFakeCaller));
    let tracker = ResourceTracker::new(rpc);

    let uri = ResourceTracker::uri_for_session(SESSION_ULID);
    let contents = tracker
        .read(&uri)
        .await
        .expect("resources/read of a session manifest must reach session.inspect via session_id");
    assert_eq!(contents.mime_type, "application/json");
    let text = contents.text.expect("manifest resource must carry text");
    assert!(text.contains(SESSION_ULID), "got {text}");
}

// === Regression (mcp-resources): list() must parse the REAL daemon wire
// shape and must surface — not swallow — a shape drift. ===

struct ListFakeCaller {
    result: serde_json::Value,
}

#[async_trait::async_trait]
impl JsonRpcCaller for ListFakeCaller {
    async fn raw_call(
        &self,
        method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, LoomError> {
        match method {
            "session.list" => Ok(self.result.clone()),
            other => Err(LoomError::new(
                loom_rpc::error::LoomErrorCode::InvalidArgument,
                format!("unexpected method in fake: {other}"),
            )),
        }
    }
}

#[tokio::test]
async fn list_parses_daemon_wire_session_list() {
    let rpc: Arc<RpcClient> = RpcClient::with_caller_for_test(Box::new(ListFakeCaller {
        result: json!([
            {
                "session_id": SESSION_ULID,
                "status": "active",
                "created_at_ms": 1_718_000_000_000u64
            }
        ]),
    }));
    let tracker = ResourceTracker::new(rpc);

    let resources = tracker
        .list()
        .await
        .expect("list() must parse the daemon's SessionInfo wire JSON");
    assert_eq!(resources.len(), 1, "wire-shaped session must surface");
    assert_eq!(
        resources[0].uri,
        format!("loom://session/{SESSION_ULID}/manifest")
    );
}

#[tokio::test]
async fn list_surfaces_wire_shape_drift_as_error() {
    // A response that doesn't match the mirror (the pre-fix daemon-drift
    // scenario) must error out loudly, not cache an empty list.
    let rpc: Arc<RpcClient> = RpcClient::with_caller_for_test(Box::new(ListFakeCaller {
        result: json!([ { "id": "01HZ", "manifest_path": "/x", "status": "active" } ]),
    }));
    let tracker = ResourceTracker::new(rpc);

    let err = tracker
        .list()
        .await
        .expect_err("a SessionInfo shape drift must surface as an error, not an empty list");
    assert!(
        err.message.contains("SessionInfo wire shape"),
        "got {}",
        err.message
    );
}
