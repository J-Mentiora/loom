// CoreApiFacade — single coherent entry point.
//
// Implements all `loom-core_contract.md` traits and provides accessors
// for the `DeterminismHarness` struct. This is the ONLY type exposed
// outside the `loom-core` crate; `loom-host` and `loom-rpc` interact
// exclusively through this facade.
//
// # Contract semantics
// - **Single entry point.** No bypass to internal modules permitted.
// - **Constructed once at daemon startup.** Holds `Arc`s of the nine
//   internal modules.
// - **`startup_health_check`** drives `StartupManager::perform_recovery_sweep`
//   before any RPC traffic is accepted.

use loom_core::budget_enforcer::BudgetEnforcer;
use loom_core::content_store::ContentStore;
use loom_core::determinism_harness::DeterminismHarness;
use loom_core::error::LoomError;
use loom_core::manifest_writer::ManifestWriter;
use loom_core::observability::Observability;
use loom_core::replay_engine::ReplayEngine;
use loom_core::session_manager::LocalSessionManager;
use loom_core::startup_manager::{RecoveryReport, StartupManager};
use loom_core::vault::Vault;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// Thin export info returned by `export_session_to_cas`. Mirrors the
/// loom-rpc `ExportInfo` wire type; kept here to avoid a circular dep.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportInfo {
    pub session_id: String,
    pub format: String,
    /// SHA-256 hex of the export artifact stored in CAS.
    pub artifact_ref: String,
}

/// Result returned by `import_playwright_from_bytes`. Mirrors the
/// loom-rpc `import.playwright` response wire type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaywrightImportResult {
    pub session_id: String,
    pub action_count: u64,
}

/// Facade configuration. Loaded from the `loom` binary's config layer
/// (BC-CORE-08 precedence: CLI > env > config file > defaults). Validated
/// once at startup against the JSON Schema shipped in the binary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreConfig {
    pub data_root: PathBuf,
    pub log_path: PathBuf,
    pub otel_enabled: bool,
    pub default_seed: u64,
    pub checkpoint_every_n: u64,
}

/// The single facade type. `loom-host` and `loom-rpc` hold an
/// `Arc<CoreApiFacade>` and call its methods.
pub struct CoreApiFacade {
    pub session_manager: Arc<LocalSessionManager>,
    pub content_store: Arc<dyn ContentStore>,
    pub manifest_writer: Arc<dyn ManifestWriter>,
    pub vault: Arc<dyn Vault>,
    pub budget_enforcer: Arc<dyn BudgetEnforcer>,
    pub determinism: Arc<DeterminismHarness>,
    pub replay_engine: Arc<dyn ReplayEngine>,
    pub startup_manager: Arc<StartupManager>,
    pub observability: Arc<Observability>,
    /// Root of the sessions directory — needed by `Exporter`.
    pub sessions_root: PathBuf,
}

impl CoreApiFacade {
    /// Construct the facade by wiring the nine internal modules.
    /// Called once at daemon startup.
    pub fn new(config: CoreConfig) -> Result<Arc<Self>, LoomError> {
        let sessions_root = config.data_root.join("sessions");
        let cas_root = config.data_root.join("cas");

        let obs = Observability::new(config.log_path, config.otel_enabled);

        let content_store: Arc<dyn loom_core::content_store::ContentStore> = Arc::new(
            loom_core::content_store::LocalContentStore::new(cas_root.clone(), Arc::clone(&obs)),
        );
        let manifest_writer: Arc<dyn ManifestWriter> = Arc::new(
            loom_core::manifest_writer::LocalManifestWriter::new(
                sessions_root.clone(),
                Arc::clone(&obs),
            ),
        );

        // Scaffold keychain — Phase 6 wires the real platform backend.
        struct NullKeychain;
        impl loom_core::vault::KeychainAccess for NullKeychain {
            fn get_secret(
                &self,
                label: &str,
            ) -> Result<zeroize::Zeroizing<Vec<u8>>, LoomError> {
                Err(LoomError::new(
                    loom_core::error::LoomErrorCode::VaultUnknownLabel,
                    format!("keychain not configured for label={label}"),
                ))
            }
        }
        let keychain: Arc<dyn loom_core::vault::KeychainAccess> = Arc::new(NullKeychain);
        let vault: Arc<dyn Vault> = Arc::new(loom_core::vault::LocalVault::new(
            keychain,
            Arc::clone(&manifest_writer),
            Arc::clone(&obs),
        ));
        let budget_enforcer: Arc<dyn BudgetEnforcer> = Arc::new(
            loom_core::budget_enforcer::LocalBudgetEnforcer::new(Arc::clone(&obs)),
        );
        let determinism = Arc::new(DeterminismHarness::new(
            config.default_seed,
            Arc::clone(&manifest_writer),
        ));
        let session_manager = LocalSessionManager::new(
            Arc::clone(&content_store),
            Arc::clone(&manifest_writer),
            Arc::clone(&vault),
            Arc::clone(&budget_enforcer),
            Arc::clone(&determinism),
            Arc::clone(&obs),
            config.default_seed,
            sessions_root.clone(),
        );
        let replay_engine: Arc<dyn ReplayEngine> = Arc::new(
            loom_core::replay_engine::LocalReplayEngine::new(
                Arc::clone(&content_store),
                Arc::clone(&manifest_writer),
                Arc::clone(&determinism),
                Arc::clone(&obs),
                Arc::clone(&session_manager),
                sessions_root.clone(),
            ),
        );
        let startup_manager = Arc::new(StartupManager::new(
            sessions_root.clone(),
            cas_root,
            Arc::clone(&content_store),
            Arc::clone(&manifest_writer),
            Arc::clone(&obs),
        ));

        Ok(Arc::new(Self {
            session_manager,
            content_store,
            manifest_writer,
            vault,
            budget_enforcer,
            determinism,
            replay_engine,
            startup_manager,
            observability: obs,
            sessions_root,
        }))
    }

    /// Run crash recovery + integrity checks. Must complete before the
    /// daemon accepts any RPC traffic.
    pub fn startup_health_check(&self) -> Result<RecoveryReport, LoomError> {
        self.startup_manager.perform_recovery_sweep()
    }

    /// Accessor for `loom-host` to install determinism host-fns.
    pub fn determinism(&self) -> Arc<DeterminismHarness> {
        Arc::clone(&self.determinism)
    }

    /// Accessor for `loom-host` to dispatch session-management RPCs.
    pub fn session_manager(&self) -> Arc<LocalSessionManager> {
        Arc::clone(&self.session_manager)
    }

    /// Accessor for `loom-host` to fulfill net_request host-fn calls
    /// (substitute path is the ONLY caller into Vault from loom-host).
    pub fn vault(&self) -> Arc<dyn Vault> {
        Arc::clone(&self.vault)
    }

    /// Accessor for `loom-rpc` replay endpoints.
    pub fn replay_engine(&self) -> Arc<dyn ReplayEngine> {
        Arc::clone(&self.replay_engine)
    }

    /// Accessor for `loom-host` budget enforcement.
    pub fn budget_enforcer(&self) -> Arc<dyn BudgetEnforcer> {
        Arc::clone(&self.budget_enforcer)
    }

    /// Accessor for blob CAS reads/writes.
    pub fn content_store(&self) -> Arc<dyn ContentStore> {
        Arc::clone(&self.content_store)
    }

    /// Accessor for manifest appends (action receipts only — audit
    /// entries go through `Vault`).
    pub fn manifest_writer(&self) -> Arc<dyn ManifestWriter> {
        Arc::clone(&self.manifest_writer)
    }

    /// Accessor for tracing/metrics emission.
    pub fn observability(&self) -> Arc<Observability> {
        Arc::clone(&self.observability)
    }

    /// Disk-based session listing. Returns (session_id, status_str, created_at_ms).
    /// Used by loom-rpc's CoreServiceAdapter to build SessionInfo responses.
    /// Walks sessions_root on disk so crashed sessions (not in memory) are included.
    pub fn list_sessions_info(&self) -> Result<Vec<(String, String, u64)>, LoomError> {
        use crate::core_api_facade::list_sessions_info_from_dir;
        list_sessions_info_from_dir(&self.sessions_root)
    }

    /// Export a closed session to the requested format, store the bytes in
    /// CAS, and return an `ExportInfo` carrying the SHA-256 artifact reference.
    /// Called by `CoreServiceAdapter` via `CoreFacadeBridge`.
    pub fn export_session_to_cas(
        &self,
        session_id: &str,
        format: &str,
    ) -> Result<ExportInfo, LoomError> {
        use crate::exporters::exporters::Exporter;
        // Path-traversal gate.
        if !loom_shared::types::is_valid_session_id(session_id) {
            return Err(LoomError::new(
                loom_core::error::LoomErrorCode::SessionNotFound,
                format!("session {session_id} does not exist"),
            ));
        }
        // Pre-flight: surface SessionNotFound for missing sessions
        // instead of leaking a generic Io error from the exporter's
        // internal fs::read.
        let wal = self.sessions_root.join(session_id).join("manifest.wal");
        if !wal.exists() {
            return Err(LoomError::new(
                loom_core::error::LoomErrorCode::SessionNotFound,
                format!("session {session_id} does not exist"),
            ));
        }
        let exporter =
            Exporter::new(self.sessions_root.clone(), Arc::clone(&self.content_store));
        let bytes = match format {
            "json" => exporter.export_json(session_id)?,
            "tarball" => exporter.export_tarball(session_id)?,
            "har" => exporter.export_har(session_id)?,
            other => {
                return Err(LoomError::invalid_argument(format!(
                    "unsupported export format: {other}"
                )));
            }
        };
        let content_ref = self.content_store.put(&bytes)?;
        Ok(ExportInfo {
            session_id: session_id.to_string(),
            format: format.to_string(),
            artifact_ref: content_ref.sha256,
        })
    }

    /// Fetch raw export artifact bytes from CAS by SHA-256 reference.
    /// Called by `CoreServiceAdapter` via `CoreFacadeBridge`.
    pub fn get_export_bytes(&self, artifact_ref: &str) -> Result<Vec<u8>, LoomError> {
        use crate::content_store::ContentRef;
        self.content_store.get(&ContentRef {
            sha256: artifact_ref.to_string(),
            size_bytes: 0,
        })
    }

    /// Import a Playwright trace.zip from raw bytes; create a non-replayable session.
    ///
    /// Called by the `import.playwright` RPC method in `loom-rpc` (Phase 7).
    /// Returns `PlaywrightImportResult { session_id, action_count }`.
    pub fn import_playwright_from_bytes(
        &self,
        trace_bytes: &[u8],
    ) -> Result<PlaywrightImportResult, LoomError> {
        use crate::importers::PlaywrightImporter;
        let importer = PlaywrightImporter::new(self.sessions_root.clone());
        let result = importer.import(trace_bytes)?;
        Ok(PlaywrightImportResult { session_id: result.session_id, action_count: result.action_count })
    }
}
