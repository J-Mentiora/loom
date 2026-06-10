// ResourceTracker — resources/list + resources/read over session RPC.
// Types only. Implementation in mod.rs.

use crate::rpc_client::RpcClient;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// MCP Resource shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Resource {
    pub uri: String,
    pub name: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
}

/// One element of the `resources/read` result's `contents` array (the
/// dispatcher wraps it as `{ "contents": [...] }` per MCP 2024-11-05). Per
/// the spec a resource carries EITHER `text` (text resources) OR `blob`
/// (base64 binary resources), never both.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceContents {
    pub uri: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
}

/// Default cache TTL.
pub const DEFAULT_TTL: Duration = Duration::from_secs(30);

/// URI scheme prefix.
pub const SESSION_URI_PREFIX: &str = "loom://session/";

/// URI suffix path component.
pub const SESSION_URI_SUFFIX: &str = "/manifest";

/// URI scheme prefix for a content-store blob addressed by sha256 hex.
/// `resources/read` of `loom://blob/<hash>` resolves the bytes (e.g. a
/// screenshot whose `screenshot_after_hash` a client holds) into a base64
/// `blob` resource.
pub const BLOB_URI_PREFIX: &str = "loom://blob/";

/// Concrete tracker.
pub struct ResourceTracker {
    pub(crate) rpc: Arc<RpcClient>,
    pub(crate) cache: Arc<tokio::sync::RwLock<Option<CachedList>>>,
    pub(crate) ttl: Duration,
}

/// Internal cache entry.
pub struct CachedList {
    pub resources: Vec<Resource>,
    pub populated_at: Instant,
}

/// Module-local mirror of loom-rpc's wire `SessionInfo`
/// (`loom-rpc/src/core_service_adapter`): the daemon serialises
/// `session_id` / `status` / `created_at_ms` plus an optional `reason`.
/// Field names must match that serialisation exactly — a drift here makes
/// every `session.list` row fail to parse and empties `resources/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub status: String,
    pub created_at_ms: u64,
    /// Free-form reason carried by `session.abort`; absent for
    /// active/closed/crashed sessions (the daemon skips it when `None`).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub reason: Option<String>,
}
