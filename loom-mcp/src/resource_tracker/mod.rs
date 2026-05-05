pub mod resource_tracker;
pub use resource_tracker::*;

#[cfg(test)]
mod interface_tests;

use loom_rpc::error::LoomError;
use std::sync::Arc;
use std::time::Duration;

impl ResourceTracker {
    pub fn new(rpc: Arc<crate::rpc_client::RpcClient>) -> Arc<Self> {
        Self::with_ttl(rpc, DEFAULT_TTL)
    }

    pub fn with_ttl(rpc: Arc<crate::rpc_client::RpcClient>, ttl: Duration) -> Arc<Self> {
        Arc::new(Self {
            rpc,
            cache: Arc::new(tokio::sync::RwLock::new(None)),
            ttl,
        })
    }

    pub async fn list(self: &Arc<Self>) -> Result<Vec<Resource>, LoomError> {
        {
            let cache = self.cache.read().await;
            if let Some(cached) = &*cache {
                if cached.populated_at.elapsed() < self.ttl {
                    return Ok(cached.resources.clone());
                }
            }
        }
        match self
            .rpc
            .call("session.list", serde_json::json!({"status": "active"}))
            .await
        {
            Ok(raw) => {
                let sessions: Vec<SessionInfo> = serde_json::from_value(raw).unwrap_or_default();
                let resources: Vec<Resource> =
                    sessions.iter().map(Self::resource_from_session).collect();
                let mut cache = self.cache.write().await;
                *cache = Some(CachedList {
                    resources: resources.clone(),
                    populated_at: std::time::Instant::now(),
                });
                Ok(resources)
            }
            Err(_e) => {
                let cache = self.cache.read().await;
                if let Some(cached) = &*cache {
                    tracing::info!("session.list failed, serving stale resource cache");
                    Ok(cached.resources.clone())
                } else {
                    Ok(vec![])
                }
            }
        }
    }

    pub async fn read(self: &Arc<Self>, uri: &str) -> Result<ResourceContents, LoomError> {
        let ulid = Self::ulid_from_uri(uri).ok_or_else(|| {
            loom_rpc::error::LoomError::new(
                loom_rpc::error::LoomErrorCode::InvalidArgument,
                format!("invalid resource URI: {uri}"),
            )
        })?;
        let raw = self
            .rpc
            .call("session.inspect", serde_json::json!({"id": ulid}))
            .await?;
        let text = serde_jcs::to_string(&raw).unwrap_or_else(|_| raw.to_string());
        Ok(ResourceContents {
            uri: uri.to_string(),
            mime_type: "application/json".to_string(),
            text,
        })
    }

    pub fn uri_for_session(ulid: &str) -> String {
        format!("{SESSION_URI_PREFIX}{ulid}{SESSION_URI_SUFFIX}")
    }

    pub fn ulid_from_uri(uri: &str) -> Option<&str> {
        let after = uri.strip_prefix(SESSION_URI_PREFIX)?;
        after.strip_suffix(SESSION_URI_SUFFIX)
    }

    pub fn resource_from_session(info: &SessionInfo) -> Resource {
        Resource {
            uri: Self::uri_for_session(&info.id),
            name: format!("Session {}", info.id),
            mime_type: "application/json".to_string(),
        }
    }

    pub async fn cache_snapshot(&self) -> Option<(Vec<Resource>, std::time::Instant)> {
        let cache = self.cache.read().await;
        cache
            .as_ref()
            .map(|c| (c.resources.clone(), c.populated_at))
    }
}
