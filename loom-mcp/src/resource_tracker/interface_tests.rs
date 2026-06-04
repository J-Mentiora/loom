// Interface tests for `ResourceTracker`. Verifies URI-scheme round-trip,
// TTL knob, and the active-session filter shape.

use super::resource_tracker::{
    Resource, ResourceContents, ResourceTracker, SessionInfo, DEFAULT_TTL, SESSION_URI_PREFIX,
    SESSION_URI_SUFFIX,
};
use std::path::PathBuf;
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
        id: "01HZ".into(),
        manifest_path: PathBuf::from("/tmp/manifest.jsonl"),
        status: "active".into(),
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

// === SessionInfo mirror has the fields we read ===

#[test]
fn session_info_has_id_manifest_path_status() {
    let s = SessionInfo {
        id: "01H".into(),
        manifest_path: PathBuf::from("/x"),
        status: "active".into(),
    };
    assert_eq!(s.status, "active");
    assert!(!s.manifest_path.as_os_str().is_empty());
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
