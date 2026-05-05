// error_types — stable receipt status/error discriminators.
//
// # Contract semantics
// - **Separate from LoomErrorCode (design decision Q1).** `ReceiptCode` is a
//   public-facing action outcome discriminator; `LoomErrorCode` is for
//   internal daemon-level infrastructure errors. They coexist without aliasing.
// - **Stable 22-code enum.** Adding a variant is SemVer-minor;
//   removing one is major. The wire string is snake_case (serde rename_all).
// - **`WebActionCompleted` is an OK-status code.** `ReceiptCode`
//   covers both success discriminators and error codes.
// - **`as_wire()` matches serde output (P1 fix).** Both use snake_case; tests
//   verify parity.

use serde::{Deserialize, Serialize};

/// Stable 22-code receipt status/error discriminator.
///
/// Emitted in the `code` field of every receipt. `WebActionCompleted` is the
/// sentinel used by OK receipts across all action tiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptCode {
    // ---- OK sentinel ----
    WebActionCompleted,

    // ---- Web surface errors ----
    WebNavigationFailed,
    WebSelectorNotFound,
    WebEvaluateThrew,
    WebActionTimeout,
    /// Selector resolved but the element provided no usable hit-test
    /// geometry (display:none / visibility:hidden / zero-area /
    /// off-tree). Emitted by Click/Hover/Scroll after `DOM.querySelector`
    /// succeeded. SemVer-minor addition (was 22 codes; now 23).
    WebHitTestFailed,

    // ---- Vault errors ----
    VaultGrantExpired,
    VaultOriginMismatch,
    VaultGrantRevoked,
    VaultSecretUnavailable,
    VaultThreatModelMissing,

    // ---- Budget errors ----
    BudgetExceededWalltime,
    BudgetExceededNetwork,
    BudgetExceededDomNodes,
    BudgetExceededJsHeap,

    // ---- Infrastructure errors ----
    SchemaViolation,
    SessionUnknown,
    SessionAborted,
    SessionClosed,
    SurfaceUnavailable,
    StoreIntegrityFailed,
    ManifestIntegrityFailed,
    RuntimeCrash,
}

impl ReceiptCode {
    /// Stable snake_case wire string. Matches `serde(rename_all = "snake_case")` output.
    pub fn as_wire(self) -> &'static str {
        match self {
            ReceiptCode::WebActionCompleted => "web_action_completed",
            ReceiptCode::WebNavigationFailed => "web_navigation_failed",
            ReceiptCode::WebSelectorNotFound => "web_selector_not_found",
            ReceiptCode::WebEvaluateThrew => "web_evaluate_threw",
            ReceiptCode::WebActionTimeout => "web_action_timeout",
            ReceiptCode::WebHitTestFailed => "web_hit_test_failed",
            ReceiptCode::VaultGrantExpired => "vault_grant_expired",
            ReceiptCode::VaultOriginMismatch => "vault_origin_mismatch",
            ReceiptCode::VaultGrantRevoked => "vault_grant_revoked",
            ReceiptCode::VaultSecretUnavailable => "vault_secret_unavailable",
            ReceiptCode::VaultThreatModelMissing => "vault_threat_model_missing",
            ReceiptCode::BudgetExceededWalltime => "budget_exceeded_walltime",
            ReceiptCode::BudgetExceededNetwork => "budget_exceeded_network",
            ReceiptCode::BudgetExceededDomNodes => "budget_exceeded_dom_nodes",
            ReceiptCode::BudgetExceededJsHeap => "budget_exceeded_js_heap",
            ReceiptCode::SchemaViolation => "schema_violation",
            ReceiptCode::SessionUnknown => "session_unknown",
            ReceiptCode::SessionAborted => "session_aborted",
            ReceiptCode::SessionClosed => "session_closed",
            ReceiptCode::SurfaceUnavailable => "surface_unavailable",
            ReceiptCode::StoreIntegrityFailed => "store_integrity_failed",
            ReceiptCode::ManifestIntegrityFailed => "manifest_integrity_failed",
            ReceiptCode::RuntimeCrash => "runtime_crash",
        }
    }

    /// True for OK-status receipt codes. Only `WebActionCompleted` for now.
    pub fn is_ok(self) -> bool {
        matches!(self, ReceiptCode::WebActionCompleted)
    }
}

/// Receipt surface discriminator.
/// One of: web, core, vault, budget, protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptSurface {
    Web,
    Core,
    Vault,
    Budget,
    Protocol,
}
