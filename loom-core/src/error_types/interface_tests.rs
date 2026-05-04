// Interface tests for error_types.
// Verifies: AC-CORE-05.2 stable code enum, as_wire() parity with serde.

#[cfg(test)]
mod tests {
    use crate::error_types::{ReceiptCode, ReceiptSurface};

    #[test]
    fn receipt_code_as_wire_matches_serde_for_all_variants() {
        let pairs = [
            (ReceiptCode::WebActionCompleted, "web_action_completed"),
            (ReceiptCode::WebNavigationFailed, "web_navigation_failed"),
            (ReceiptCode::WebSelectorNotFound, "web_selector_not_found"),
            (ReceiptCode::WebEvaluateThrew, "web_evaluate_threw"),
            (ReceiptCode::WebActionTimeout, "web_action_timeout"),
            (ReceiptCode::VaultGrantExpired, "vault_grant_expired"),
            (ReceiptCode::VaultOriginMismatch, "vault_origin_mismatch"),
            (ReceiptCode::VaultGrantRevoked, "vault_grant_revoked"),
            (ReceiptCode::VaultSecretUnavailable, "vault_secret_unavailable"),
            (ReceiptCode::VaultThreatModelMissing, "vault_threat_model_missing"),
            (ReceiptCode::BudgetExceededWalltime, "budget_exceeded_walltime"),
            (ReceiptCode::BudgetExceededNetwork, "budget_exceeded_network"),
            (ReceiptCode::BudgetExceededDomNodes, "budget_exceeded_dom_nodes"),
            (ReceiptCode::BudgetExceededJsHeap, "budget_exceeded_js_heap"),
            (ReceiptCode::SchemaViolation, "schema_violation"),
            (ReceiptCode::SessionUnknown, "session_unknown"),
            (ReceiptCode::SessionAborted, "session_aborted"),
            (ReceiptCode::SessionClosed, "session_closed"),
            (ReceiptCode::SurfaceUnavailable, "surface_unavailable"),
            (ReceiptCode::StoreIntegrityFailed, "store_integrity_failed"),
            (ReceiptCode::ManifestIntegrityFailed, "manifest_integrity_failed"),
            (ReceiptCode::RuntimeCrash, "runtime_crash"),
        ];
        assert_eq!(pairs.len(), 22, "must cover all 22 receipt codes");
        for (code, expected_wire) in &pairs {
            assert_eq!(code.as_wire(), *expected_wire);
            let serde_out = serde_json::to_string(code).unwrap();
            assert_eq!(serde_out, format!("\"{}\"", expected_wire),
                "serde output must match as_wire for {:?}", code);
        }
    }

    #[test]
    fn web_action_completed_is_ok_all_others_are_error() {
        assert!(ReceiptCode::WebActionCompleted.is_ok());
        assert!(!ReceiptCode::WebNavigationFailed.is_ok());
        assert!(!ReceiptCode::RuntimeCrash.is_ok());
    }

    #[test]
    fn receipt_surface_serde_roundtrip() {
        let surfaces = [
            ReceiptSurface::Web,
            ReceiptSurface::Core,
            ReceiptSurface::Vault,
            ReceiptSurface::Budget,
            ReceiptSurface::Protocol,
        ];
        for s in &surfaces {
            let json = serde_json::to_string(s).unwrap();
            let back: ReceiptSurface = serde_json::from_str(&json).unwrap();
            assert_eq!(*s, back);
        }
    }
}
