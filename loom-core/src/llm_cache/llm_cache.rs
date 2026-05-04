// LlmCache — session-scoped in-memory LLM response cache (AC-DET-07.1).
// Live/Mixed modes are stubs; upstream proxy is a future integration concern.

use loom_core::error::{LoomError, LoomErrorCode};
use loom_shared::llm_types::{LlmCacheKey, LlmCallResult, LlmMode, LlmResponse};
use parking_lot::Mutex;
use std::collections::HashMap;

/// Session-scoped in-memory LLM response cache (FR-DET-07, AC-DET-07.1).
pub struct LlmCache {
    pub(crate) mode: LlmMode,
    pub(crate) store: Mutex<HashMap<String, LlmResponse>>,
}

impl LlmCache {
    pub fn new(mode: LlmMode) -> Self {
        Self {
            mode,
            store: Mutex::new(HashMap::new()),
        }
    }

    /// Insert a response. Used during recording and by ReplayEngine seeding.
    pub fn insert(&self, key: &LlmCacheKey, response: LlmResponse) {
        self.store.lock().insert(key.cache_key_string(), response);
    }

    /// Look up by key. Recorded: returns stored response or LlmCacheMiss.
    /// Live/Mixed: stub — returns LlmCacheMiss (upstream proxy is future work).
    pub fn call(&self, key: &LlmCacheKey) -> Result<LlmCallResult, LoomError> {
        match self.mode {
            LlmMode::Recorded => {
                let store = self.store.lock();
                match store.get(&key.cache_key_string()) {
                    Some(response) => Ok(LlmCallResult {
                        response: response.clone(),
                        cache_hit: true,
                    }),
                    None => Err(LoomError::new(
                        LoomErrorCode::LlmCacheMiss,
                        format!(
                            "recorded mode: no cached LLM response for key {}:{}:{}",
                            key.model, key.prompt_hash, key.tool_schema_version
                        ),
                    )),
                }
            }
            LlmMode::Live | LlmMode::Mixed => Err(LoomError::new(
                LoomErrorCode::LlmCacheMiss,
                "LLM upstream proxy not implemented in this feature; use Recorded mode",
            )),
        }
    }

    pub fn len(&self) -> usize {
        self.store.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
