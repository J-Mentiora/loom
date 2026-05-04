//! LLM passthrough shared types (FR-DET-07).
//! Shared here so loom-rpc/loom-mcp can reference without depending on loom-core.

use serde::{Deserialize, Serialize};

/// LLM passthrough mode for a session (FR-DET-07).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LlmMode {
    #[default]
    Live,
    Recorded,
    Mixed,
}

/// Cache key: `(model, prompt_hash, tool_schema_version)`.
/// `prompt_hash` must be sha256(JCS(prompt_json)) — 64 lowercase hex chars.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LlmCacheKey {
    pub model: String,
    pub prompt_hash: String,
    pub tool_schema_version: String,
}

impl LlmCacheKey {
    /// Stable colon-delimited string for HashMap keying.
    pub fn cache_key_string(&self) -> String {
        debug_assert!(
            self.prompt_hash.len() == 64 && self.prompt_hash.chars().all(|c| c.is_ascii_hexdigit()),
            "prompt_hash must be 64 lowercase hex chars, got: {:?}",
            self.prompt_hash
        );
        format!(
            "{}:{}:{}",
            self.model, self.prompt_hash, self.tool_schema_version
        )
    }
}

/// A stored LLM response body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponse {
    pub body_json: serde_json::Value,
}

/// Result of `LlmCache::call` — includes cache-hit flag for receipt population.
#[derive(Debug)]
pub struct LlmCallResult {
    pub response: LlmResponse,
    pub cache_hit: bool,
}
