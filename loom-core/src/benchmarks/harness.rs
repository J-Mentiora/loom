//! Shared in-process mock stack for benchmark tests.
//! Provides LocalSessionManager built from in-memory-backed implementations.

use crate::budget_enforcer::{
    Action, BudgetEnforcer, BudgetLimits, KillCallback, ResourceKind, SessionCounters,
};
use crate::content_store::{ContentRef, ContentStore, GcReport};
use crate::determinism_harness::DeterminismHarness;
use crate::error::LoomError;
use crate::manifest_writer::{AuditKind, ManifestEntry, ManifestWriter, SessionId, WriterHandle};
use crate::observability::Observability;
use crate::session_manager::LocalSessionManager;
use crate::vault::{
    AddCredentialOpts, AddCredentialReceipt, GrantId, GrantOpts, GrantSnapshot, NetRequest,
    RevokeReason, Vault,
};
use parking_lot::Mutex;
use ring::digest::{digest, SHA256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// In-memory ContentStore for benchmarks (no disk I/O).
pub struct BenchmarkContentStore {
    blobs: Mutex<HashMap<String, Vec<u8>>>,
}

impl BenchmarkContentStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            blobs: Mutex::new(HashMap::new()),
        })
    }
}

impl ContentStore for BenchmarkContentStore {
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
                loom_shared::error_format::LoomErrorCode::StoreIntegrityFailed,
                format!("benchmark store: blob {} not found", r.sha256),
            )
        })
    }

    fn gc(&self, _ttl: std::time::Duration) -> Result<GcReport, LoomError> {
        Ok(GcReport {
            blobs_scanned: 0,
            blobs_collected: 0,
            bytes_freed: 0,
        })
    }
}

/// No-op BudgetEnforcer that never blocks or kills.
pub struct MockBudgetEnforcer;

impl BudgetEnforcer for MockBudgetEnforcer {
    fn check(&self, _session: SessionId, _action: &Action) -> Result<(), LoomError> {
        Ok(())
    }

    fn account(
        &self,
        _session: SessionId,
        _kind: ResourceKind,
        _delta: u64,
    ) -> Result<(), LoomError> {
        Ok(())
    }

    fn register_session(
        &self,
        _id: SessionId,
        _counters: Arc<SessionCounters>,
        _limits: BudgetLimits,
        _kill: KillCallback,
    ) {
    }

    fn unregister_session(&self, _id: SessionId) {}
}

/// No-op Vault for benchmarks. Rejects all credentials operations.
pub struct BenchmarkVault;

impl Vault for BenchmarkVault {
    fn grant(&self, _session: SessionId, _opts: GrantOpts) -> Result<GrantId, LoomError> {
        Err(LoomError::new(
            loom_shared::error_format::LoomErrorCode::VaultRejection,
            "benchmark vault: no credentials configured",
        ))
    }

    fn substitute(&self, _grant: GrantId, _req: &mut NetRequest) -> Result<(), LoomError> {
        Err(LoomError::new(
            loom_shared::error_format::LoomErrorCode::VaultRejection,
            "benchmark vault: no credentials configured",
        ))
    }

    fn revoke(&self, _grant: GrantId, _reason: RevokeReason) -> Result<(), LoomError> {
        Ok(())
    }

    fn add_credential(&self, _opts: AddCredentialOpts) -> Result<AddCredentialReceipt, LoomError> {
        Err(LoomError::new(
            loom_shared::error_format::LoomErrorCode::VaultRejection,
            "benchmark vault: no credentials configured",
        ))
    }

    fn list_grants(&self, _session: Option<SessionId>) -> Result<Vec<GrantSnapshot>, LoomError> {
        Ok(Vec::new())
    }

    fn set_secret(
        &self,
        _label: &str,
        _secret: zeroize::Zeroizing<Vec<u8>>,
    ) -> Result<(), LoomError> {
        Err(LoomError::new(
            loom_shared::error_format::LoomErrorCode::VaultRejection,
            "benchmark vault: keychain not configured",
        ))
    }

    fn get_secret_direct(
        &self,
        _label: &str,
    ) -> Result<zeroize::Zeroizing<Vec<u8>>, LoomError> {
        Err(LoomError::new(
            loom_shared::error_format::LoomErrorCode::VaultUnknownLabel,
            "benchmark vault: keychain not configured",
        ))
    }

    fn delete_secret(&self, _label: &str) -> Result<(), LoomError> {
        Ok(())
    }

    fn list_labels(&self) -> Result<Vec<String>, LoomError> {
        Ok(Vec::new())
    }
}

/// In-memory ManifestWriter with configurable delays (for latency injection in tests).
pub struct MockManifestWriter {
    /// Simulated delay added to each `open_manifest()` call (default: 0ms).
    pub open_delay_ms: u64,
    /// Simulated delay added to each `append()` call (default: 0ms).
    pub append_delay_ms: u64,
    entries: Mutex<Vec<(String, ManifestEntry)>>,
}

impl MockManifestWriter {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            open_delay_ms: 0,
            append_delay_ms: 0,
            entries: Mutex::new(Vec::new()),
        })
    }

    /// Delay on `open_manifest()` — slows down `session_manager.create()`.
    pub fn with_open_delay(delay_ms: u64) -> Arc<Self> {
        Arc::new(Self {
            open_delay_ms: delay_ms,
            append_delay_ms: 0,
            entries: Mutex::new(Vec::new()),
        })
    }

    /// Delay on `append()` — slows down receipt writes in receipt_overhead bench.
    pub fn with_append_delay(delay_ms: u64) -> Arc<Self> {
        Arc::new(Self {
            open_delay_ms: 0,
            append_delay_ms: delay_ms,
            entries: Mutex::new(Vec::new()),
        })
    }

    pub fn entry_count(&self) -> usize {
        self.entries.lock().len()
    }
}

impl ManifestWriter for MockManifestWriter {
    fn open_manifest_with_started_at(
        &self,
        session: SessionId,
        _budgets: Option<crate::budget_enforcer::BudgetLimits>,
        _started_at_ms_override: Option<u64>,
        _capture_policy: Option<String>,
    ) -> Result<WriterHandle, LoomError> {
        if self.open_delay_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(self.open_delay_ms));
        }
        // WriterHandle.wal_path and checkpoint_path are pub(crate) within loom-core.
        Ok(WriterHandle {
            session_id: session,
            wal_path: PathBuf::from("/dev/null"),
            checkpoint_path: PathBuf::from("/dev/null"),
        })
    }

    fn append(&self, session: SessionId, entry: ManifestEntry) -> Result<(), LoomError> {
        if self.append_delay_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(self.append_delay_ms));
        }
        self.entries.lock().push((session.0, entry));
        Ok(())
    }

    fn append_audit(
        &self,
        _session: SessionId,
        _kind: AuditKind,
        _canonical_bytes: Vec<u8>,
    ) -> Result<(), LoomError> {
        Ok(())
    }

    fn validate(&self, _session: SessionId) -> Result<(), LoomError> {
        Ok(())
    }

    fn checkpoint(&self, _session: SessionId) -> Result<(), LoomError> {
        Ok(())
    }
}

/// Build an in-process LocalSessionManager with benchmark-grade mock dependencies.
///
/// Uses:
/// - BenchmarkContentStore (in-memory, no disk)
/// - caller-supplied manifest_writer (allows latency injection in tests)
/// - BenchmarkVault (no-op)
/// - MockBudgetEnforcer (no-op)
/// - DeterminismHarness (virtual clock, seed=42)
/// - Observability (logs to /dev/null, otel disabled)
pub fn build_session_manager(manifest_writer: Arc<dyn ManifestWriter>) -> Arc<LocalSessionManager> {
    let content_store = BenchmarkContentStore::new() as Arc<dyn ContentStore>;
    let vault = Arc::new(BenchmarkVault) as Arc<dyn Vault>;
    let budget_enforcer = Arc::new(MockBudgetEnforcer) as Arc<dyn BudgetEnforcer>;
    let determinism = Arc::new(DeterminismHarness::new(42, manifest_writer.clone()));
    let obs = Observability::new(PathBuf::from("/dev/null"), false);

    LocalSessionManager::new(
        content_store,
        manifest_writer,
        vault,
        budget_enforcer,
        determinism,
        obs,
        42, // default_seed for benchmarks
        PathBuf::from("/tmp/loom-bench/sessions"),
    )
}
