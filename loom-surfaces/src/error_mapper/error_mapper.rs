// ErrorMapper — sole `host-error` → `LoomErrorCode` translator inside
// the WASM surface.
//
// # Contract semantics
// - **Surface-side translation only.** Maps the
//   typed `host-error` returned by host-fns to the `error_code` field
//   inside Receipts assembled by surface verbs. This is distinct from
//   `loom-host::ErrorMapper` which translates `LoomError` → `host-error`
//   at the WIT boundary.
// - **Pure (no panics, no allocations beyond the returned struct).**
//   Cannot fail. Adding a new `host-error` variant without an arm here
//   is a compile-time error (Rust exhaustiveness check).
// - **Single source of truth.** Verb modules MUST NOT inline their own
//   match arms; they call `ErrorMapper::map`. Enforced by CI lint
//   `tools/lint-surface-error-paths.py`.
//
// # Banned in this module
// - `std::time`, `std::net`, `std::fs::write`, `getrandom`,
//   `std::panic::catch_unwind`. Pure `match` over closed enums only.

extern crate alloc;

use alloc::string::String;

use crate::cookie_types::cookie_types::CookieValidationError;

// `host-error` and `LoomErrorCode` are normally `wit-bindgen`-generated.
// We declare matching shapes here so the public contract is reviewable
// from this single file. The real surface crate uses the generated
// types; the variant names are identical.

/// Reason discriminator inside `host-error::shim-failure`.
///
/// `HitTestFailed` is **guest-internal**: it is constructed inside the
/// WASM guest by `loom-surfaces::hit_test::resolve_centre_for_selector`
/// when the target element gives no box model (e.g. `display:none`,
/// `visibility:hidden`, zero-area, off-tree) or returns a malformed CDP
/// response. The host wire layer never produces this variant; it
/// originates entirely from the surface's CDP-response parsing. The
/// existing `SelectorNotFound` variant follows the same pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShimFailureKind {
    Timeout,
    SelectorNotFound,
    PermissionDenied {
        permission: String,
    },
    Crashed,
    NavigationFailed,
    /// Guest-internal: target exists but has no usable hit-test geometry.
    /// Emitted by `hit_test::resolve_centre_for_selector` for missing
    /// box models, malformed quads, or collapsed/zero-area parallelograms.
    HitTestFailed,
}

/// Reason discriminator inside `host-error::vault-rejection`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultRejectionReason {
    Expired,
    Scope,
    Origin,
    Revoked,
}

/// Resource kind inside `host-error::budget-exceeded`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetKind {
    WallClock,
    NetworkBytes,
    DomNodeCount,
    JsHeapBytes,
}

/// WIT `host-error` variant (mirrors `wit/loom-surface.wit::types.host-error`).
///
/// **Note:** `Eq` is intentionally NOT derived because the v0.9.6
/// `CookieValidationError` variant carries `InvalidExpires(f64)` per
/// the CDP cookie-expires spec, and `f64` is not `Eq`. `PartialEq`
/// remains, which covers all existing test assertions.
#[derive(Debug, Clone, PartialEq)]
pub enum HostError {
    BudgetExceeded { kind: BudgetKind },
    VaultRejection { reason: VaultRejectionReason },
    ShimFailure { kind: ShimFailureKind },
    StoreIntegrityFailed,
    Internal { reason: String },
    /// v0.9.6 web-cookie-injection. Surface-side validation rejection
    /// raised by `validate_cookie_params` in the `set_cookies` verb
    /// before any CDP call is made. Carries the typed `CookieValidationError`
    /// so the wire-level `error_code` discriminator and per-cookie
    /// `error_details` survive to the receipt.
    CookieValidationError(CookieValidationError),
}

/// Stable `LoomErrorCode` enum carried in the Receipt's `error_code` field.
/// Mirrors the variants defined in `errors.json` (the cross-system
/// stable enum).
///
/// **Note:** `Eq` is intentionally NOT derived (see `HostError`); the
/// `CookieValidationError(CookieValidationError)` variant transitively
/// carries `f64` through `CookieValidationError::InvalidExpires`.
#[derive(Debug, Clone, PartialEq)]
pub enum LoomErrorCode {
    BudgetWallClockExceeded,
    BudgetNetworkBytesExceeded,
    BudgetDomNodeCountExceeded,
    BudgetJsHeapBytesExceeded,
    VaultGrantExpired,
    VaultScopeViolation,
    VaultOriginViolation,
    VaultGrantRevoked,
    WebActionTimeout,
    WebSelectorNotFound,
    WebNavigationFailed,
    /// Selector resolved but the element provided no usable hit-test
    /// geometry — `display:none`, `visibility:hidden`, zero-area,
    /// off-tree, or otherwise occluded. Emitted by Click/Hover/Scroll
    /// when `hit_test::resolve_centre_for_selector` fails after
    /// `DOM.querySelector` succeeds. Wire string: `"web_hit_test_failed"`
    /// (snake_case at the receipt level).
    WebHitTestFailed,
    TccPermissionDenied {
        permission: String,
    },
    SurfaceShimFailed,
    StoreIntegrityFailed,
    HostInternalError {
        reason: String,
    },
    /// evaluate expression matched the safe-profile denylist.
    /// Wire string: "schema_violation" (matches AC spec).
    SchemaViolation,
    /// Download target path outside session-scoped downloads dir.
    /// Wire string: "safe_profile_download_blocked".
    SafeProfileDownloadBlocked,
    /// v0.9.6 web-cookie-injection. Surface-side validation rejection
    /// from `validate_cookie_params` in `set_cookies`. Wire string:
    /// `"cookie_validation_error"`. The inner `CookieValidationError`
    /// carries the per-cookie discriminator and detail
    /// (`name_empty` / `name_invalid` / `value_too_large` /
    /// `too_many_cookies` / `invalid_expires` / `invalid_same_site`).
    CookieValidationError(CookieValidationError),
}

/// `SurfaceContext` lets `ErrorMapper` choose between web-specific and
/// generic shim-failure translations (e.g., `WebActionTimeout` vs
/// `ShimTimeout`). v1 surface-web always passes `Web`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceContext {
    Web,
    Shell,
    Fs,
    Api,
    Native,
}

/// Stateless mapper. Only function: `map`.
pub struct ErrorMapper;

impl ErrorMapper {
    /// Translate a `host-error` to a stable `LoomErrorCode`. Pure;
    /// cannot fail.
    pub fn map(host_err: HostError, ctx: SurfaceContext) -> LoomErrorCode {
        match host_err {
            HostError::BudgetExceeded { kind } => match kind {
                BudgetKind::WallClock => LoomErrorCode::BudgetWallClockExceeded,
                BudgetKind::NetworkBytes => LoomErrorCode::BudgetNetworkBytesExceeded,
                BudgetKind::DomNodeCount => LoomErrorCode::BudgetDomNodeCountExceeded,
                BudgetKind::JsHeapBytes => LoomErrorCode::BudgetJsHeapBytesExceeded,
            },
            HostError::VaultRejection { reason } => match reason {
                VaultRejectionReason::Expired => LoomErrorCode::VaultGrantExpired,
                VaultRejectionReason::Scope => LoomErrorCode::VaultScopeViolation,
                VaultRejectionReason::Origin => LoomErrorCode::VaultOriginViolation,
                VaultRejectionReason::Revoked => LoomErrorCode::VaultGrantRevoked,
            },
            HostError::ShimFailure { kind } => match kind {
                ShimFailureKind::Timeout => match ctx {
                    SurfaceContext::Web => LoomErrorCode::WebActionTimeout,
                    _ => LoomErrorCode::SurfaceShimFailed,
                },
                ShimFailureKind::SelectorNotFound => LoomErrorCode::WebSelectorNotFound,
                ShimFailureKind::PermissionDenied { permission } => {
                    LoomErrorCode::TccPermissionDenied { permission }
                }
                ShimFailureKind::Crashed => LoomErrorCode::SurfaceShimFailed,
                ShimFailureKind::NavigationFailed => LoomErrorCode::WebNavigationFailed,
                ShimFailureKind::HitTestFailed => LoomErrorCode::WebHitTestFailed,
            },
            HostError::StoreIntegrityFailed => LoomErrorCode::StoreIntegrityFailed,
            HostError::Internal { reason } => LoomErrorCode::HostInternalError { reason },
            HostError::CookieValidationError(e) => LoomErrorCode::CookieValidationError(e),
        }
    }
}
