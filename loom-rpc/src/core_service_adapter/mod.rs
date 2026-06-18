//! `core_service_adapter` — see crate root.
pub mod core_service_adapter;
pub use core_service_adapter::*;

#[cfg(test)]
mod interface_tests;

impl CoreServiceAdapterApi for CoreServiceAdapter {
    fn create_session(&self, params: CreateSessionParams) -> Result<SessionInfo, LoomError> {
        let (session_id, created_at_ms) = self.core.create_session_raw(
            &params.profile,
            &params.network_mode,
            params.capture_policy.as_deref(),
            params.seed,
            params.budget.clone(),
            params.no_blocklist,
            params.no_determinism,
            params.clock_anchor,
            params.record_screencast,
        )?;
        Ok(SessionInfo {
            session_id,
            status: "active".to_string(),
            created_at_ms,
            reason: None,
        })
    }

    fn inspect_session(
        &self,
        session_id: &str,
        at_action: Option<u64>,
    ) -> Result<SessionInspection, AdapterError> {
        let summary = self.core.inspect_session_json(session_id, at_action)?;
        Ok(SessionInspection {
            session_id: session_id.to_string(),
            at_action,
            manifest_summary: summary,
        })
    }

    fn list_sessions(&self) -> Result<Vec<SessionInfo>, AdapterError> {
        let infos = self.core.list_sessions_info()?;
        Ok(infos
            .into_iter()
            .map(|(session_id, status, created_at_ms)| {
                // Surface reason for aborted sessions.
                // The bridge encodes the reason inside the status string for
                // backward-compat with the (status, created_at_ms) tuple
                // shape: "aborted:<reason>" → status="aborted", reason=Some(<reason>).
                let (status, reason) = if let Some(rest) = status.strip_prefix("aborted:") {
                    ("aborted".to_string(), Some(rest.to_string()))
                } else {
                    (status, None)
                };
                SessionInfo {
                    session_id,
                    status,
                    created_at_ms,
                    reason,
                }
            })
            .collect())
    }

    fn close_session(&self, session_id: &str) -> Result<SessionInfo, AdapterError> {
        self.core.close_session_raw(session_id)?;
        Ok(SessionInfo {
            session_id: session_id.to_string(),
            status: "closed".to_string(),
            // Look up the original creation time so the response is
            // self-describing (caller doesn't need a separate inspect).
            created_at_ms: self.created_at_ms_for(session_id),
            reason: None,
        })
    }

    fn abort_session(&self, session_id: &str, reason: &str) -> Result<SessionInfo, AdapterError> {
        self.core.abort_session_raw(session_id, reason)?;
        Ok(SessionInfo {
            session_id: session_id.to_string(),
            status: "aborted".to_string(),
            created_at_ms: self.created_at_ms_for(session_id),
            reason: Some(reason.to_string()),
        })
    }

    fn replay_session(
        &self,
        session_id: &str,
        _speed: Option<f32>,
        // Tolerated-but-ignored: released SDKs (≤0.10.x) unconditionally
        // sent `network_mode: "replay"` on session.replay, so rejecting
        // would break every deployed replay() call. Replay re-executes
        // from the manifest; there is no page-network mode to choose.
        // Current SDKs no longer send the field.
        _network_mode: Option<&str>,
    ) -> Result<SessionInfo, LoomError> {
        let new_id = self.core.replay_session_to_id(session_id)?;
        Ok(SessionInfo {
            // The replay session is the NEW one created by the engine;
            // its created_at_ms is the source's started_at_ms (replay
            // copies the header for hash-chain bit-equality).
            // Look it up the same way close/abort do.
            created_at_ms: self.created_at_ms_for(&new_id),
            session_id: new_id,
            status: "replay_complete".to_string(),
            reason: None,
        })
    }

    fn diff_sessions(
        &self,
        a: &str,
        b: &str,
        include_screenshots: bool,
        _show_dom_diffs: bool,
    ) -> Result<DiffReport, AdapterError> {
        let diff = self.core.diff_sessions_json(a, b, include_screenshots)?;
        Ok(DiffReport {
            a: a.to_string(),
            b: b.to_string(),
            diff,
        })
    }

    fn export_session(&self, session_id: &str, format: &str) -> Result<ExportInfo, AdapterError> {
        self.core.export_session_to_cas(session_id, format)
    }

    fn content_get(&self, artifact_ref: &str) -> Result<ContentData, AdapterError> {
        let bytes = self.core.get_export_bytes(artifact_ref)?;
        Ok(ContentData {
            artifact_ref: artifact_ref.to_string(),
            data_hex: hex::encode(&bytes),
            size_bytes: bytes.len() as u64,
        })
    }

    fn validate_session(&self, session_id: &str) -> Result<ValidationResult, AdapterError> {
        self.core.validate_session_result(session_id)
    }

    fn import_playwright(&self, trace_bytes: &[u8]) -> Result<PlaywrightImportInfo, AdapterError> {
        self.core.import_playwright_from_bytes(trace_bytes)
    }

    // Vault methods delegate one-to-one to `CoreFacadeBridge`. The bridge
    // (loom-daemon::CoreBridge) holds the loom-core handle and translates
    // between wire types and `loom_core::vault` types.
    fn vault_grant(&self, params: GrantParams) -> Result<GrantInfo, AdapterError> {
        self.core.vault_grant(params)
    }

    fn vault_revoke(&self, grant_id: &str, reason: &str) -> Result<(), AdapterError> {
        self.core.vault_revoke(grant_id, reason)
    }

    fn vault_list_grants(&self, session_id: Option<&str>) -> Result<Vec<GrantInfo>, AdapterError> {
        self.core.vault_list_grants(session_id)
    }

    fn vault_add(&self, params: VaultAddParams) -> Result<VaultAddInfo, AdapterError> {
        self.core.vault_add(params)
    }

    fn vault_set_secret(
        &self,
        params: VaultSetSecretParams,
    ) -> Result<VaultSetSecretInfo, AdapterError> {
        self.core.vault_set_secret(params)
    }

    fn vault_delete_secret(
        &self,
        params: VaultDeleteSecretParams,
    ) -> Result<VaultDeleteSecretInfo, AdapterError> {
        self.core.vault_delete_secret(params)
    }

    fn vault_list_labels(
        &self,
        params: VaultListLabelsParams,
    ) -> Result<VaultListLabelsInfo, AdapterError> {
        self.core.vault_list_labels(params)
    }

    fn vault_diagnose(&self) -> Result<VaultDiagnoseInfo, AdapterError> {
        self.core.vault_diagnose()
    }

    fn vault_get_session_context(&self) -> Result<VaultGetSessionContextInfo, AdapterError> {
        self.core.vault_get_session_context()
    }

    fn gc_run(
        &self,
        ttl_days: Option<u64>,
        store_max_bytes: Option<u64>,
    ) -> Result<GcRunReport, AdapterError> {
        self.core.gc_run(ttl_days, store_max_bytes)
    }

    fn session_reap(&self, dry_run: bool) -> Result<ReapReport, AdapterError> {
        self.core.session_reap(dry_run)
    }
}

impl CoreServiceAdapter {
    /// Return the manifest's started_at_ms for `session_id` so close /
    /// abort / replay responses carry the original creation time
    /// instead of a hardcoded 0. Falls back to 0 on any lookup error
    /// (the field is metadata, not load-bearing for receipt
    /// correctness).
    fn created_at_ms_for(&self, session_id: &str) -> u64 {
        match self.core.list_sessions_info() {
            Ok(infos) => infos
                .into_iter()
                .find(|(id, _, _)| id == session_id)
                .map(|(_, _, ts)| ts)
                .unwrap_or(0),
            Err(_) => 0,
        }
    }
}
