//! Mock harness for `loom-core`. Tagged `#[cfg(any(test, feature = "mock"))]`
//! at the lib.rs declaration so feature workers can enable mocks during
//! their TDD phase before real sibling features merge.
//!
//! Deterministic canned responses; no I/O, no time, no randomness.

use crate::content_store::{ContentRef, ContentStore, GcReport};
use crate::manifest_writer::SessionId;
use crate::session_manager::{AbortReason, SessionCreateOpts};
use loom_shared::error_format::{LoomError, LoomErrorCode};
use parking_lot::Mutex;
use ring::digest::{digest, SHA256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Stable mock SessionId used across the mock harness.
pub const MOCK_SESSION_ID: &str = "01HZTESTABC123";

pub fn mock_session_id() -> SessionId {
    // SessionId is a transparent ULID newtype in the locked interface.
    // Phase 6 may switch to `SessionId::from_str` once ManifestWriter
    // stabilizes the constructor. For now, we stub via JSON round-trip
    // through serde, which is locked.
    serde_json::from_str(&format!("\"{MOCK_SESSION_ID}\""))
        .expect("MOCK_SESSION_ID is a valid ULID string")
}

/// In-process facade implementing the SessionManager surface area with
/// deterministic responses. Phase 6 may swap this for trait-based
/// dynamic dispatch once `LocalSessionManager` exposes a trait.
pub struct MockSessionManager;

impl MockSessionManager {
    pub fn create(&self, _opts: SessionCreateOpts) -> Result<SessionId, LoomError> {
        Ok(mock_session_id())
    }

    pub fn close(&self, _id: SessionId) -> Result<(), LoomError> {
        Ok(())
    }

    pub fn abort(&self, _id: SessionId, _reason: AbortReason) -> Result<(), LoomError> {
        Ok(())
    }

    pub fn get_status_unknown() -> LoomError {
        LoomError::new(LoomErrorCode::SessionNotFound, "mock: session not found")
    }
}

/// Mock CoreApiFacade returns deterministic recovery + no-op transitions.
pub struct MockCoreApiFacade;

impl MockCoreApiFacade {
    pub fn arc() -> Arc<Self> {
        Arc::new(Self)
    }
}

/// In-memory content store — no disk I/O. Suitable for unit tests of features
/// that depend on content-store (receipt-system, wasm-host, etc.).
pub struct MockContentStore {
    blobs: Mutex<HashMap<String, Vec<u8>>>,
}

impl MockContentStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            blobs: Mutex::new(HashMap::new()),
        })
    }
}

impl ContentStore for MockContentStore {
    fn put(&self, bytes: &[u8]) -> Result<ContentRef, LoomError> {
        let d = digest(&SHA256, bytes);
        let sha256: String = d.as_ref().iter().map(|b| format!("{b:02x}")).collect();
        let size_bytes = bytes.len() as u64;
        self.blobs
            .lock()
            .entry(sha256.clone())
            .or_insert_with(|| bytes.to_vec());
        Ok(ContentRef { sha256, size_bytes })
    }

    fn get(&self, r: &ContentRef) -> Result<Vec<u8>, LoomError> {
        self.blobs.lock().get(&r.sha256).cloned().ok_or_else(|| {
            LoomError::new(
                LoomErrorCode::StoreNotFound,
                format!("mock: blob not found: {}", r.sha256),
            )
        })
    }

    fn gc(&self, _ttl: Duration) -> Result<GcReport, LoomError> {
        Ok(GcReport {
            blobs_scanned: 0,
            blobs_collected: 0,
            bytes_freed: 0,
        })
    }
}

/// Mock vault — refuses all secret reads with a stable label so tests
/// can assert vault rejection paths without setting up keychain state.
pub struct MockVault;

impl MockVault {
    pub fn reject(&self, label: &str) -> LoomError {
        LoomError::new(
            LoomErrorCode::VaultRejection,
            format!("mock vault denies '{label}'"),
        )
    }
}
