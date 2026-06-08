// loom-daemon vault bridge — translation between loom-rpc wire types
// (`GrantParams`/`VaultAddParams`/`GrantInfo`/…) and loom-core domain types
// (`GrantOpts`/`AddCredentialOpts`/`GrantSnapshot`/…) for the `vault.*` RPC
// methods. Extracted from `lib.rs` (large-file split).
//
// Each function takes `&CoreApiFacade` rather than `&CoreBridge` so the
// bridge's `core` field stays private — the `CoreFacadeBridge for CoreBridge`
// trait methods in `lib.rs` are thin delegations into here. Errors route
// through `map_loom_error` so the wire envelope carries the distinct
// `vault_grant_revoked`/`vault_grant_expired`/`vault_rejection` codes.

use crate::{map_loom_error, validate_label_or_rpc_err};
use loom_core::core_api_facade::CoreApiFacade;
use loom_core::error::LoomError;
use loom_rpc::core_service_adapter::core_service_adapter::{
    AdapterError, GrantInfo, GrantParams, VaultAddInfo, VaultAddParams, VaultDeleteSecretInfo,
    VaultDeleteSecretParams, VaultDiagnoseInfo, VaultDiagnoseInitStatus, VaultListLabelsInfo,
    VaultListLabelsParams, VaultSetSecretInfo, VaultSetSecretParams,
};

pub(crate) fn vault_grant(core: &CoreApiFacade, p: GrantParams) -> Result<GrantInfo, AdapterError> {
    use loom_core::manifest_writer::SessionId;
    use loom_core::vault::{CredentialType, GrantOpts};
    // v0.9.6: map the optional credential_type string to the typed
    // enum. Default OAuth preserves the v0.9.5 contract.
    let credential_type = match p.credential_type.as_deref() {
        None | Some("oauth") => CredentialType::OAuth,
        Some("cookie") => CredentialType::Cookie,
        Some(_other) => {
            return Err(
                loom_rpc::error_translator::error_translator::LoomErrorCode::SchemaViolation,
            );
        }
    };
    let opts = GrantOpts {
        credential_type,
        label: p.label.clone(),
        origin: p.origin.clone(),
        scopes: p.scopes.clone(),
        ttl_ms: p.ttl_seconds.saturating_mul(1000),
        // Safe under the §3.8(a) startup precondition: daemon refuses
        // to come up unless `security/vault_threat_model.md` exists
        // with all four required sections.
        threat_model_acknowledged: true,
    };
    let grant_id = core
        .vault
        .grant(SessionId(p.session_id), opts)
        .map_err(|e| map_loom_error(&e))?;
    Ok(GrantInfo {
        grant_id: grant_id.0,
        origin: p.origin,
        scopes: p.scopes,
        ttl_seconds: p.ttl_seconds,
        label: p.label,
    })
}

pub(crate) fn vault_revoke(
    core: &CoreApiFacade,
    grant_id: &str,
    reason: &str,
) -> Result<(), AdapterError> {
    use loom_core::vault::{GrantId, RevokeReason};
    core.vault
        .revoke(
            GrantId(grant_id.to_string()),
            RevokeReason {
                reason: reason.to_string(),
            },
        )
        .map_err(|e| map_loom_error(&e))
}

pub(crate) fn vault_list_grants(
    core: &CoreApiFacade,
    session_id: Option<&str>,
) -> Result<Vec<GrantInfo>, AdapterError> {
    use loom_core::manifest_writer::SessionId;
    let sid = session_id.map(|s| SessionId(s.to_string()));
    let snapshots = core
        .vault
        .list_grants(sid)
        .map_err(|e| map_loom_error(&e))?;
    // GrantSnapshot (loom-core) → GrantInfo (loom-rpc) field-level
    // translation. `session_id` is dropped at the bridge boundary
    // because contract `GrantInfo` has no such field (per F-A1).
    Ok(snapshots
        .into_iter()
        .map(|s| GrantInfo {
            grant_id: s.grant_id,
            origin: s.origin,
            scopes: s.scopes,
            ttl_seconds: s.ttl_seconds,
            label: s.label,
        })
        .collect())
}

pub(crate) fn vault_add(
    core: &CoreApiFacade,
    p: VaultAddParams,
) -> Result<VaultAddInfo, AdapterError> {
    use loom_core::vault::AddCredentialOpts;
    let receipt = core
        .vault
        .add_credential(AddCredentialOpts {
            provider: p.provider,
            label: p.label,
            yes: p.yes,
        })
        .map_err(|e| map_loom_error(&e))?;
    Ok(VaultAddInfo {
        provider: receipt.provider,
        label: receipt.label,
        status: receipt.status,
    })
}

// ── v0.9.4 W6 direct credential bridge methods ──────────────────

pub(crate) fn vault_set_secret(
    core: &CoreApiFacade,
    p: VaultSetSecretParams,
) -> Result<VaultSetSecretInfo, AdapterError> {
    use loom_core::manifest_writer::SessionId;
    use zeroize::Zeroizing;

    // D37 label validation at the wire boundary; the W5.10 manifest-
    // writer gate is the belt-and-suspenders below.
    validate_label_or_rpc_err(&p.label)?;

    let bytes = hex::decode(p.secret_hex.as_bytes()).map_err(|e| {
        map_loom_error(&LoomError::new(
            loom_core::error::LoomErrorCode::InvalidArgument,
            format!("vault.set_secret: secret_hex is not valid hex: {e}"),
        ))
    })?;
    if bytes.is_empty() {
        return Err(map_loom_error(&LoomError::new(
            loom_core::error::LoomErrorCode::InvalidArgument,
            "vault.set_secret: empty secret rejected",
        )));
    }
    const MAX_SECRET_BYTES: usize = 1 << 20; // 1 MiB (matches A-W6.2 / D22)
    if bytes.len() > MAX_SECRET_BYTES {
        return Err(map_loom_error(&LoomError::new(
            loom_core::error::LoomErrorCode::InvalidArgument,
            format!(
                "vault.set_secret: secret exceeds 1 MiB cap ({} bytes)",
                bytes.len()
            ),
        )));
    }
    // Size bucket via the loom-core single source of truth (D24) so this
    // receipt and the hash-chained audit payload can never disagree.
    let size_bucket = loom_core::vault::size_bucket(bytes.len()).as_str();

    // A-W6.1 overwrite contract: when overwrite=false and the label
    // already exists, reject before the keychain write so the audit
    // trail records a refusal, not a silent upsert.
    let session = p.session_id.as_deref().map(|s| SessionId(s.to_string()));
    let pre_existed = core
        .vault
        .get_secret_direct(session.as_ref(), &p.label)
        .is_ok();
    if pre_existed && !p.overwrite {
        return Err(map_loom_error(
            &LoomError::new(
                loom_core::error::LoomErrorCode::VaultRejection,
                format!(
                    "credential '{}' already exists; pass --overwrite to replace it",
                    p.label
                ),
            )
            .with_context(serde_json::json!({
                "code": "already_exists",
                "label": p.label,
            })),
        ));
    }

    core.vault
        .set_secret(session.as_ref(), &p.label, Zeroizing::new(bytes))
        .map_err(|e| map_loom_error(&e))?;

    Ok(VaultSetSecretInfo {
        label: p.label,
        replaced: pre_existed,
        size_bucket: size_bucket.to_string(),
    })
}

pub(crate) fn vault_delete_secret(
    core: &CoreApiFacade,
    p: VaultDeleteSecretParams,
) -> Result<VaultDeleteSecretInfo, AdapterError> {
    use loom_core::manifest_writer::SessionId;
    validate_label_or_rpc_err(&p.label)?;
    let session = p.session_id.as_deref().map(|s| SessionId(s.to_string()));
    let outcome = core
        .vault
        .delete_secret(session.as_ref(), &p.label, p.force)
        .map_err(|e| map_loom_error(&e))?;
    Ok(VaultDeleteSecretInfo {
        label: p.label,
        cascade_revoked_grants: outcome.cascade_revoked_grants,
    })
}

pub(crate) fn vault_list_labels(
    core: &CoreApiFacade,
    p: VaultListLabelsParams,
) -> Result<VaultListLabelsInfo, AdapterError> {
    use loom_core::manifest_writer::SessionId;
    let session = p.session_id.as_deref().map(|s| SessionId(s.to_string()));
    let labels = core
        .vault
        .list_labels(session.as_ref())
        .map_err(|e| map_loom_error(&e))?;
    let count = u32::try_from(labels.len()).unwrap_or(u32::MAX);
    Ok(VaultListLabelsInfo { labels, count })
}

pub(crate) fn vault_get_session_context(
    core: &CoreApiFacade,
) -> Result<
    loom_rpc::core_service_adapter::core_service_adapter::VaultGetSessionContextInfo,
    AdapterError,
> {
    use loom_rpc::core_service_adapter::core_service_adapter::VaultGetSessionContextInfo;
    use loom_rpc::error_translator::error_translator::LoomErrorCode;
    // Enumerate sessions; pick the most recently created Active one.
    // Returns SessionNotFound when no active sessions exist.
    let infos = core.list_sessions_info().map_err(|e| map_loom_error(&e))?;
    let mut active: Vec<&(String, String, u64)> = infos
        .iter()
        .filter(|(_id, status, _ts)| {
            // The status string from list_sessions_info is the
            // session's snake_case status; "active" indicates a
            // live session ready to accept actions.
            status == "active"
        })
        .collect();
    if active.is_empty() {
        return Err(LoomErrorCode::SessionNotFound);
    }
    active.sort_by_key(|(_id, _status, ts)| *ts);
    let unambiguous = active.len() == 1;
    let (session_id, _, _) = active.last().unwrap();
    Ok(VaultGetSessionContextInfo {
        session_id: session_id.clone(),
        unambiguous,
    })
}

pub(crate) fn vault_diagnose(core: &CoreApiFacade) -> Result<VaultDiagnoseInfo, AdapterError> {
    // v0.9.4 minimum-viable diagnose per A-W6.4. Probes the
    // keychain by attempting a list_labels call; success counts as
    // `init_status.ok`, failure surfaces the typed `KeychainErrorKind`
    // (snake_case) as the `last_keychain_error.kind`. The shape is
    // stable; richer state (cached last-error, backend identity from
    // KeychainConfig) lands in a follow-up that wires those signals
    // through `CoreApiFacade`.
    let (label_count, last_keychain_error, init_status) =
        match core.vault.list_labels(None) {
            Ok(labels) => (
                u32::try_from(labels.len()).unwrap_or(u32::MAX),
                None,
                VaultDiagnoseInitStatus::Ok,
            ),
            Err(e) => {
                // The LoomError code → KeychainErrorKind snake_case round-trip
                // (string match is fine — the set is closed at 6).
                let kind = match e.code {
                    loom_core::error::LoomErrorCode::VaultUnknownLabel => "not_found",
                    loom_core::error::LoomErrorCode::VaultPermissionDenied => "denied",
                    loom_core::error::LoomErrorCode::VaultBackendUnavailable => "unavailable",
                    loom_core::error::LoomErrorCode::VaultBackendTimeout => "timed_out",
                    loom_core::error::LoomErrorCode::VaultNonInteractivePrompt => {
                        "non_interactive_prompt"
                    }
                    loom_core::error::LoomErrorCode::VaultInternal => "internal",
                    _ => "internal",
                };
                let internal_hash = e
                    .context
                    .as_ref()
                    .and_then(|c| c.get("internal_hash"))
                    .and_then(|h| h.as_str())
                    .map(str::to_owned);
                let diagnosed_at_ts =
                    humantime::format_rfc3339_seconds(std::time::SystemTime::now()).to_string();
                (
                0,
                Some(loom_rpc::core_service_adapter::core_service_adapter::VaultDiagnoseLastError {
                    kind: kind.to_string(),
                    diagnosed_at_ts,
                    internal_hash,
                }),
                VaultDiagnoseInitStatus::Error {
                    reason: e.message.clone(),
                },
            )
            }
        };

    let backend = default_backend_name();
    Ok(VaultDiagnoseInfo {
        backend,
        init_status,
        // Hardcoded `"loom"` per D36 in v0.9.4.
        service_id: "loom".to_string(),
        label_count,
        last_keychain_error,
    })
}

/// Best-effort backend name for `vault.diagnose` per A-W6.4 schema. The
/// daemon today builds the `KeychainConfig` via env var + TTY at
/// `async_main`; this returns the platform default when no override is
/// detected. Refined when the follow-up wires the resolved `BackendChoice`
/// through `CoreApiFacade`.
fn default_backend_name() -> String {
    if let Ok(env) = std::env::var("LOOM_KEYCHAIN_BACKEND") {
        if !env.is_empty() {
            return env;
        }
    }
    if cfg!(target_os = "macos") {
        "macos".to_string()
    } else if cfg!(target_os = "linux") {
        "linux".to_string()
    } else {
        "stub".to_string()
    }
}
