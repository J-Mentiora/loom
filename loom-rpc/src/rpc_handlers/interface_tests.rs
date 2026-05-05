// Re-export of the locked Phase 5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-rpc/modules/rpc_handlers/interface_tests.rs` instead.
// Interface tests for `RpcHandlers`. Verifies every contract method
// has a handler signature, IC-RPC-09 routing split (action vs core),
// IC-RPC-10 vault response shape, IC-RPC-02 rpc.schemas in-memory.

use super::rpc_handlers::{HandlerResult, RpcHandlers};
use crate::core_service_adapter::core_service_adapter::{
    CoreServiceAdapterApi, CreateSessionParams, DiffReport, ExportInfo, GrantInfo, GrantParams,
    SessionInfo, SessionInspection, ValidationResult,
};
use crate::host_service_adapter::host_service_adapter::{Action, HostServiceAdapterApi, Receipt};
use crate::rpc_observability::rpc_observability::RpcObservabilityApi;
use crate::schema_provider::schema_provider::{SchemaProviderApi, SchemaRegistry};
use crate::schema_validator::schema_validator::SchemaValidatorApi;
use std::sync::Arc;

#[test]
fn constructor_takes_five_arc_handles() {
    fn _ck(
        c: Arc<dyn CoreServiceAdapterApi>,
        h: Arc<dyn HostServiceAdapterApi>,
        s: Arc<dyn SchemaProviderApi>,
        v: Arc<dyn SchemaValidatorApi>,
        o: Arc<dyn RpcObservabilityApi>,
    ) -> Arc<RpcHandlers> {
        RpcHandlers::new(c, h, s, v, o)
    }
    let _ = _ck;
}

// ===== session.* signatures =====

#[test]
fn session_create_signature() {
    fn _ck() {
        async fn _go(h: &RpcHandlers, p: CreateSessionParams) -> HandlerResult<SessionInfo> {
            h.session_create(p).await
        }
        let _ = _go;
    }
    let _ = _ck;
}

#[test]
fn session_inspect_signature_supports_optional_at_action() {
    fn _ck() {
        async fn _go(
            h: &RpcHandlers,
            s: String,
            at: Option<u64>,
        ) -> HandlerResult<SessionInspection> {
            h.session_inspect(s, at).await
        }
        let _ = _go;
    }
    let _ = _ck;
}

#[test]
fn session_list_signature() {
    fn _ck() {
        async fn _go(h: &RpcHandlers) -> HandlerResult<Vec<SessionInfo>> {
            h.session_list().await
        }
        let _ = _go;
    }
    let _ = _ck;
}

#[test]
fn session_close_signature() {
    fn _ck() {
        async fn _go(h: &RpcHandlers, s: String) -> HandlerResult<SessionInfo> {
            h.session_close(s).await
        }
        let _ = _go;
    }
    let _ = _ck;
}

#[test]
fn session_abort_signature_for_ac_core_08_1() {
    fn _ck() {
        async fn _go(h: &RpcHandlers, s: String, r: String) -> HandlerResult<SessionInfo> {
            h.session_abort(s, r).await
        }
        let _ = _go;
    }
    let _ = _ck;
}

#[test]
fn session_replay_signature() {
    fn _ck() {
        async fn _go(
            h: &RpcHandlers,
            s: String,
            sp: Option<f32>,
            nm: Option<String>,
        ) -> HandlerResult<SessionInfo> {
            h.session_replay(s, sp, nm).await
        }
        let _ = _go;
    }
    let _ = _ck;
}

#[test]
fn session_diff_signature() {
    fn _ck() {
        async fn _go(
            h: &RpcHandlers,
            a: String,
            b: String,
            i: bool,
            d: bool,
        ) -> HandlerResult<DiffReport> {
            h.session_diff(a, b, i, d).await
        }
        let _ = _go;
    }
    let _ = _ck;
}

#[test]
fn session_export_signature_for_four_formats() {
    fn _ck() {
        async fn _go(h: &RpcHandlers, s: String, f: String) -> HandlerResult<ExportInfo> {
            h.session_export(s, f).await
        }
        let _ = _go;
    }
    let _ = _ck;
}

#[test]
fn session_validate_signature() {
    fn _ck() {
        async fn _go(h: &RpcHandlers, s: String) -> HandlerResult<ValidationResult> {
            h.session_validate(s).await
        }
        let _ = _go;
    }
    let _ = _ck;
}

// ===== action.* — IC-RPC-09 routing =====

#[test]
fn action_dispatch_returns_receipt_via_host_adapter() {
    // IC-RPC-09: single host dispatch path; IC-RPC-07: typed Receipt only.
    fn _ck() {
        async fn _go(h: &RpcHandlers, a: Action) -> HandlerResult<Receipt> {
            h.action_dispatch(a).await
        }
        let _ = _go;
    }
    let _ = _ck;
}

// ===== vault.* — IC-RPC-10 =====

#[test]
fn vault_grant_returns_grant_info_no_secret_field() {
    // IC-RPC-10: response is GrantInfo (grant_id only).
    fn _ck() {
        async fn _go(h: &RpcHandlers, p: GrantParams) -> HandlerResult<GrantInfo> {
            h.vault_grant(p).await
        }
        let _ = _go;
    }
    let _ = _ck;
}

#[test]
fn vault_revoke_signature_takes_grant_id_and_reason() {
    fn _ck() {
        async fn _go(h: &RpcHandlers, g: String, r: String) -> HandlerResult<()> {
            h.vault_revoke(g, r).await
        }
        let _ = _go;
    }
    let _ = _ck;
}

#[test]
fn vault_list_grants_signature() {
    fn _ck() {
        async fn _go(h: &RpcHandlers, s: Option<String>) -> HandlerResult<Vec<GrantInfo>> {
            h.vault_list_grants(s).await
        }
        let _ = _go;
    }
    let _ = _ck;
}

// ===== rpc.* — IC-RPC-02 =====

#[test]
fn rpc_schemas_returns_in_memory_registry_snapshot() {
    // IC-RPC-02: never re-reads disk on request path.
    fn _ck() {
        async fn _go(h: &RpcHandlers) -> HandlerResult<SchemaRegistry> {
            h.rpc_schemas().await
        }
        let _ = _go;
    }
    let _ = _ck;
}

// ===== AC-CLI-01.1 canonical-JSON =====

#[test]
fn serialise_canonical_uses_jcs_helper_function() {
    // AC-CLI-01.1: all responses go through the canonical-JSON helper.
    fn _ck<T: serde::Serialize>(v: &T) -> Result<String, super::rpc_handlers::JsonRpcError> {
        RpcHandlers::serialise_canonical(v)
    }
    let _ = _ck::<u32>;
}
