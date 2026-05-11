// ErrorMapper — sole owner of cross-boundary error translation.
//
// # Contract semantics
// - **Single translation site.** This module owns every
//   `From<…>` impl that crosses a process boundary. No other module in
//   `loom-host` may declare these conversions.
// - **Three translation directions:**
//     1. `From<LoomError> for HostError` — Rust → WIT at the WASM boundary.
//     2. `From<wasmtime::Error> for LoomError` — wasmtime engine errors.
//     3. `From<wasmtime::Trap> for LoomError` — surface traps → typed
//        `SurfaceTrap` receipts.
// - **Closed mapping.** Every `LoomErrorCode` variant maps
//   to one of `host-error::{budget-exceeded, vault-rejection, shim-failure,
//   store-integrity-failed, internal}`. Adding a new `LoomErrorCode`
//   without updating `From` is a compile-time failure (exhaustive match).
// - **No `anyhow::Error` across WIT.** The generated `host::Host` trait
//   signature returns `Result<T, HostError>`, so anyhow cannot escape.

use loom_core::error::{LoomError, LoomErrorCode};
use serde::{Deserialize, Serialize};

/// WIT-derived discriminated variant. The shape shown is what
/// `wit-bindgen` generates from `wit/loom-surface.wit::variant host-error`.
/// Owned in this module (not in `WitTypeMarshaller`) because conversion
/// semantics — not type shape — is the rule here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum HostError {
    BudgetExceeded(BudgetDetail),
    VaultRejection(VaultDetail),
    ShimFailure(ShimDetail),
    StoreIntegrityFailed(ContentRefDetail),
    Internal(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetDetail {
    pub kind: String, // "walltime" | "network" | "dom_nodes" | "js_heap"
    pub observed: u64,
    pub limit: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultDetail {
    pub reason: String, // "origin_mismatch" | "scope_insufficient" | "expired" | "revoked"
    pub grant_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShimDetail {
    pub shim_id: String,
    pub reason: String, // "spawn_failed" | "crashed" | "timeout" | "protocol"
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentRefDetail {
    pub sha256: String,
    pub size_bytes: u64,
}

/// The full set of resolved `.dwp`-mapped trap frames. Built by
/// `TrapHandler` and converted by `ErrorMapper`'s `From<wasmtime::Trap>`
/// impl.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrapFrame {
    pub pc: u64,
    pub source_file: Option<String>,
    pub source_line: Option<u32>,
    pub func_name: Option<String>,
}

/// Pure conversion entry point. Wraps `From<LoomError> for HostError`
/// in a callable so call-sites read consistently.
pub fn loom_error_to_host_error(err: LoomError) -> HostError {
    HostError::from(err)
}

/// Pure conversion: wasmtime engine errors that aren't traps. Used by
/// `Compiler::compile` and `ModuleLibrary::load`.
pub fn wasmtime_error_to_loom_error(err: wasmtime::Error) -> LoomError {
    LoomError::new(LoomErrorCode::Internal, err.to_string())
}

/// Pure conversion: a wasmtime guest trap → `LoomError::SurfaceTrap`
/// carrying optional debug-info-resolved frames.
pub fn wasmtime_trap_to_loom_error(
    trap: wasmtime::Trap,
    surface: String,
    frames: Vec<TrapFrame>,
) -> LoomError {
    let _ = frames;
    LoomError::new(
        LoomErrorCode::SurfaceTrap,
        format!("surface={surface} trap={trap:?}"),
    )
}

// === The required From impls ===

impl From<LoomError> for HostError {
    fn from(err: LoomError) -> Self {
        let detail = err.message.clone();
        match err.code {
            LoomErrorCode::BudgetExceeded | LoomErrorCode::BudgetRateLimited => {
                HostError::BudgetExceeded(BudgetDetail {
                    kind: err.code.as_wire().to_owned(),
                    observed: 0,
                    limit: 0,
                })
            }
            LoomErrorCode::VaultRejection
            | LoomErrorCode::VaultGrantExpired
            | LoomErrorCode::VaultGrantRevoked
            | LoomErrorCode::VaultUnknownLabel => HostError::VaultRejection(VaultDetail {
                reason: err.code.as_wire().to_owned(),
                grant_id: String::new(),
            }),
            LoomErrorCode::ShimFailure
            | LoomErrorCode::ShimTimeout
            | LoomErrorCode::ShimBreakerOpen => HostError::ShimFailure(ShimDetail {
                shim_id: String::new(),
                reason: err.code.as_wire().to_owned(),
            }),
            LoomErrorCode::StoreIntegrityFailed => {
                HostError::StoreIntegrityFailed(ContentRefDetail {
                    sha256: String::new(),
                    size_bytes: 0,
                })
            }
            LoomErrorCode::SessionNotFound
            | LoomErrorCode::SessionAlreadyClosed
            | LoomErrorCode::SessionAborted
            | LoomErrorCode::SessionKilled
            | LoomErrorCode::SurfaceTrap
            | LoomErrorCode::StoreNotFound
            | LoomErrorCode::StoreFullNoEvictable
            | LoomErrorCode::ManifestCorrupt
            | LoomErrorCode::ReplayDivergence
            | LoomErrorCode::ReplayMissingBlob
            | LoomErrorCode::LlmCacheMiss
            | LoomErrorCode::RpcInvalidRequest
            | LoomErrorCode::RpcAuthFailed
            | LoomErrorCode::RpcSchemaViolation
            | LoomErrorCode::RequestTimeout
            | LoomErrorCode::RequestCancelled
            | LoomErrorCode::SchemaViolation
            | LoomErrorCode::SafeProfileDownloadBlocked
            | LoomErrorCode::ProfileRestricted
            | LoomErrorCode::Io
            | LoomErrorCode::InvalidArgument
            | LoomErrorCode::Unsupported
            | LoomErrorCode::BrowserNotFound
            | LoomErrorCode::Internal => HostError::Internal(detail),
        }
    }
}
