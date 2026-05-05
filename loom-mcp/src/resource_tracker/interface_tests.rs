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
        text: "{}".into(),
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
