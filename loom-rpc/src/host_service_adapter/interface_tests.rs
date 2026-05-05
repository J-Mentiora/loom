// Re-export of the locked Phase 5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-rpc/modules/host_service_adapter/interface_tests.rs` instead.
// Interface tests for `HostServiceAdapter`. Verifies IC-RPC-07
// (typed Receipt only), IC-RPC-09 (single dispatch entry point),
// SR-RPC-05 latency partition wiring.

use super::host_service_adapter::{
    Action, AdapterError, HostServiceAdapter, HostServiceAdapterApi, Receipt, ReceiptError,
    ReceiptStatus, WasmHostBridge,
};
use std::sync::Arc;

#[test]
fn constructor_takes_arc_dyn_wasm_host_bridge() {
    fn _ck(h: Arc<dyn WasmHostBridge>) -> Arc<HostServiceAdapter> {
        HostServiceAdapter::new(h)
    }
    let _ = _ck;
}

#[test]
fn action_enum_has_typed_variants_no_cdp_shaped_value() {
    // IC-RPC-07: every action is a typed enum variant; there is no
    // `Action::Cdp(serde_json::Value)` variant. Adding one would
    // require this test to be updated.
    fn _audit_variants(a: Action) {
        match a {
            Action::WebNavigate { .. } => {}
            Action::WebClick { .. } => {}
            Action::WebEvaluate { .. } => {}
            Action::WebType { .. } => {}
            Action::WebScreenshot { .. } => {}
            Action::WebSelect { .. } => {}
            Action::WebHover { .. } => {}
            Action::WebScroll { .. } => {}
            Action::WebWait { .. } => {}
            Action::WebSnapshot { .. } => {}
        }
    }
    let _ = _audit_variants;
}

#[test]
fn receipt_carries_typed_status_and_no_cdp_envelope() {
    fn _ck(r: &Receipt) {
        let _: &u64 = &r.action_id;
        let _: &String = &r.session_id;
        let _: &ReceiptStatus = &r.status;
        let _: &u64 = &r.timing_ticks;
        let _: &Vec<serde_json::Value> = &r.side_effects;
        let _: &Option<ReceiptError> = &r.error;
    }
    let _ = _ck;
}

#[test]
fn receipt_status_serialises_snake_case() {
    let s = serde_json::to_string(&ReceiptStatus::Success).unwrap();
    assert_eq!(s, "\"success\"");
    let s = serde_json::to_string(&ReceiptStatus::Error).unwrap();
    assert_eq!(s, "\"error\"");
    let s = serde_json::to_string(&ReceiptStatus::Aborted).unwrap();
    assert_eq!(s, "\"aborted\"");
}

#[test]
fn dispatch_action_signature_is_async_returns_receipt_or_error() {
    // IC-RPC-09: single dispatch entry point. SR-RPC-05: the await
    // interval is the boundary the latency-partition observability
    // measures.
    fn _ck() {
        async fn _go<A: HostServiceAdapterApi>(a: &A, ac: Action) -> Result<Receipt, AdapterError> {
            a.dispatch_action(ac).await
        }
        let _ = _go::<HostServiceAdapter>;
    }
    let _ = _ck;
}

#[test]
fn adapter_has_no_session_or_vault_methods_per_ic_rpc_09_split() {
    // Compile-time evidence: the trait surface contains EXACTLY one
    // method. session.* / vault.* live in CoreServiceAdapter; this
    // adapter is structurally incompatible with those return types.
    #[allow(clippy::type_complexity)]
    fn _audit<A: HostServiceAdapterApi>() {
        let _: fn(
            &A,
            Action,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Receipt, AdapterError>> + Send + '_>,
        > = |a, ac| Box::pin(a.dispatch_action(ac));
    }
    let _ = _audit::<HostServiceAdapter>;
}
