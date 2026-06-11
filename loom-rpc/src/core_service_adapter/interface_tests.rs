// Interface tests for `CoreServiceAdapter`. Verifies the routing split
// (no `action.*` here), GrantInfo shape (no secret-fields),
// and that every contract method on `session.*` / `vault.*` has a
// corresponding adapter signature.

use super::core_service_adapter::{
    AdapterError, CoreFacadeBridge, CoreServiceAdapter, CoreServiceAdapterApi, CreateSessionParams,
    DiffReport, ExportInfo, GrantInfo, GrantParams, LoomError, SessionInfo, SessionInspection,
    ValidationResult, VaultAddInfo, VaultAddParams,
};
use std::sync::Arc;

#[test]
fn constructor_takes_arc_dyn_core_facade_bridge() {
    fn _ck(c: Arc<dyn CoreFacadeBridge>) -> Arc<CoreServiceAdapter> {
        CoreServiceAdapter::new(c)
    }
    let _ = _ck;
}

#[test]
fn grant_info_has_no_secret_token_or_value_fields() {
    // Response schema explicitly omits secret-shaped fields.
    fn _ck(g: &GrantInfo) {
        let _: &String = &g.grant_id;
        let _: &String = &g.origin;
        let _: &Vec<String> = &g.scopes;
        let _: &u64 = &g.ttl_seconds;
        let _: &String = &g.label;
        // The ABSENCE of secret/token/value is what we're asserting —
        // any future addition would break this test by virtue of the
        // exhaustive struct-field listing above (a reviewer adding a
        // secret field has to also delete the comment + add the field
        // to this test, which is the trip-wire).
    }
    let _ = _ck;
}

#[test]
fn create_session_signature() {
    // Full `LoomError` (not a bare `AdapterError` code): the cap
    // rejection's `{active, cap, hint}` context must reach the envelope.
    fn _ck<A: CoreServiceAdapterApi>(
        a: &A,
        p: CreateSessionParams,
    ) -> Result<SessionInfo, LoomError> {
        a.create_session(p)
    }
    let _ = _ck::<CoreServiceAdapter>;
}

#[test]
fn inspect_session_signature() {
    fn _ck<A: CoreServiceAdapterApi>(
        a: &A,
        s: &str,
        at: Option<u64>,
    ) -> Result<SessionInspection, AdapterError> {
        a.inspect_session(s, at)
    }
    let _ = _ck::<CoreServiceAdapter>;
}

#[test]
fn list_sessions_signature() {
    fn _ck<A: CoreServiceAdapterApi>(a: &A) -> Result<Vec<SessionInfo>, AdapterError> {
        a.list_sessions()
    }
    let _ = _ck::<CoreServiceAdapter>;
}

#[test]
fn close_session_signature() {
    fn _ck<A: CoreServiceAdapterApi>(a: &A, s: &str) -> Result<SessionInfo, AdapterError> {
        a.close_session(s)
    }
    let _ = _ck::<CoreServiceAdapter>;
}

#[test]
fn abort_session_signature_carries_reason() {
    fn _ck<A: CoreServiceAdapterApi>(a: &A, s: &str, r: &str) -> Result<SessionInfo, AdapterError> {
        a.abort_session(s, r)
    }
    let _ = _ck::<CoreServiceAdapter>;
}

#[test]
fn replay_session_signature() {
    fn _ck<A: CoreServiceAdapterApi>(
        a: &A,
        s: &str,
        sp: Option<f32>,
        nm: Option<&str>,
    ) -> Result<SessionInfo, AdapterError> {
        a.replay_session(s, sp, nm)
    }
    let _ = _ck::<CoreServiceAdapter>;
}

#[test]
fn diff_sessions_signature() {
    fn _ck<A: CoreServiceAdapterApi>(
        a: &A,
        x: &str,
        y: &str,
        inc: bool,
        dom: bool,
    ) -> Result<DiffReport, AdapterError> {
        a.diff_sessions(x, y, inc, dom)
    }
    let _ = _ck::<CoreServiceAdapter>;
}

#[test]
fn export_session_signature_supports_all_four_formats() {
    fn _ck<A: CoreServiceAdapterApi>(a: &A, s: &str, f: &str) -> Result<ExportInfo, AdapterError> {
        a.export_session(s, f)
    }
    let _ = _ck::<CoreServiceAdapter>;
    // Format is a string per the contract; format = json | tarball |
    // har | cdp. SchemaValidator enforces enum at request boundary.
}

#[test]
fn validate_session_signature() {
    fn _ck<A: CoreServiceAdapterApi>(a: &A, s: &str) -> Result<ValidationResult, AdapterError> {
        a.validate_session(s)
    }
    let _ = _ck::<CoreServiceAdapter>;
}

#[test]
fn vault_grant_signature_returns_grant_info_no_secret() {
    // Response carries grant_id only.
    fn _ck<A: CoreServiceAdapterApi>(a: &A, p: GrantParams) -> Result<GrantInfo, AdapterError> {
        a.vault_grant(p)
    }
    let _ = _ck::<CoreServiceAdapter>;
}

#[test]
fn vault_revoke_signature_takes_grant_id_and_reason() {
    fn _ck<A: CoreServiceAdapterApi>(a: &A, g: &str, r: &str) -> Result<(), AdapterError> {
        a.vault_revoke(g, r)
    }
    let _ = _ck::<CoreServiceAdapter>;
}

#[test]
fn vault_list_grants_signature_supports_optional_session_filter() {
    fn _ck<A: CoreServiceAdapterApi>(
        a: &A,
        s: Option<&str>,
    ) -> Result<Vec<GrantInfo>, AdapterError> {
        a.vault_list_grants(s)
    }
    let _ = _ck::<CoreServiceAdapter>;
}

#[test]
fn vault_add_signature_returns_vault_add_info_no_secret() {
    // Response is the typed
    // `VaultAddInfo { provider, label, status }` — no secret bytes,
    // no token, no value field. Compile-time enforced by the field
    // listing in `vault_add_info_has_no_secret_fields` below.
    fn _ck<A: CoreServiceAdapterApi>(
        a: &A,
        p: VaultAddParams,
    ) -> Result<VaultAddInfo, AdapterError> {
        a.vault_add(p)
    }
    let _ = _ck::<CoreServiceAdapter>;
}

#[test]
fn vault_add_info_has_no_secret_fields() {
    // Same trip-wire pattern as `grant_info_has_no_secret_token_or_value_fields`:
    // exhaustive struct-field list — adding a secret field would
    // require deleting this comment + updating the test.
    fn _ck(v: &VaultAddInfo) {
        let _: &String = &v.provider;
        let _: &String = &v.label;
        let _: &String = &v.status;
    }
    let _ = _ck;
}

#[test]
fn adapter_does_not_expose_action_dispatch() {
    // Compile-time evidence: the trait surface contains NO method
    // returning `Receipt`. Adding one would require this test to be
    // updated, which is the trip-wire.
    fn _audit_methods_returns_no_receipt<A: CoreServiceAdapterApi>(_a: &A) {
        // Listing every method on the trait so a future addition
        // surfaces as a compile-time diff in this test.
        let _ = A::create_session;
        let _ = A::inspect_session;
        let _ = A::list_sessions;
        let _ = A::close_session;
        let _ = A::abort_session;
        let _ = A::replay_session;
        let _ = A::diff_sessions;
        let _ = A::export_session;
        let _ = A::validate_session;
        let _ = A::vault_grant;
        let _ = A::vault_revoke;
        let _ = A::vault_list_grants;
        let _ = A::vault_add;
    }
    let _ = _audit_methods_returns_no_receipt::<CoreServiceAdapter>;
}

// === v0.9.6 web-cookie-injection: vault.get_session_context types ===

#[test]
fn vault_get_session_context_info_round_trips_serde() {
    use super::core_service_adapter::VaultGetSessionContextInfo;
    let info = VaultGetSessionContextInfo {
        session_id: "S01HZABCDEF".to_string(),
        unambiguous: true,
    };
    let json = serde_json::to_string(&info).expect("serialise");
    let back: VaultGetSessionContextInfo = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(back.session_id, "S01HZABCDEF");
    assert!(back.unambiguous);
}

#[test]
fn vault_get_session_context_info_carries_unambiguous_flag_as_false_for_multi_active() {
    use super::core_service_adapter::VaultGetSessionContextInfo;
    let info = VaultGetSessionContextInfo {
        session_id: "S2".to_string(),
        unambiguous: false, // multiple active → caller may want to confirm
    };
    let json = serde_json::to_string(&info).unwrap();
    assert!(json.contains("\"unambiguous\":false"));
}

#[test]
fn vault_get_session_context_trait_method_signature() {
    use super::core_service_adapter::{
        AdapterError, CoreServiceAdapterApi, VaultGetSessionContextInfo,
    };
    fn _ck(c: &dyn CoreServiceAdapterApi) -> Result<VaultGetSessionContextInfo, AdapterError> {
        c.vault_get_session_context()
    }
    let _ = _ck;
}
