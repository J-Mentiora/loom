// ResourceTracker — resources/list + resources/read over session RPC.
// Types only. Implementation in mod.rs.

use crate::rpc_client::RpcClient;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
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

/// Body of `resources/read`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceContents {
    pub uri: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    pub text: String,
}

/// Default cache TTL.
pub const DEFAULT_TTL: Duration = Duration::from_secs(30);

/// URI scheme prefix.
pub const SESSION_URI_PREFIX: &str = "loom://session/";

/// URI suffix path component.
pub const SESSION_URI_SUFFIX: &str = "/manifest";

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

/// Module-local mirror of loom-rpc's SessionInfo.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub manifest_path: PathBuf,
    pub status: String,
}
