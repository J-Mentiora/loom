// WitTypeMarshaller — `wit-bindgen`-generated bidirectional WIT ↔ Rust
// type conversion. WIT is the schema source of truth.
//
// # The actual generated file
//
// At build time, `loom-host/build.rs` invokes
// ```text
// wit-bindgen rust ../../wit/loom-surface.wit --out-dir src/wit_type_marshaller/
// ```
// which emits a single `bindings.rs` file containing:
//
// ```ignore
// pub mod loom_surface_bindings {
//     pub mod host {
//         /// The host-import trait that `HostFunctionTable` implements.
//         /// Generated from `wit/loom-surface.wit::interface host`.
//         pub trait Host {
//             fn clock_now(&mut self) -> wasmtime::Result<u64>;
//             fn rng_next_u64(&mut self) -> wasmtime::Result<u64>;
//             fn blob_put(&mut self, bytes: Vec<u8>) -> wasmtime::Result<Result<ContentRef, HostError>>;
//             fn blob_get(&mut self, r: ContentRef) -> wasmtime::Result<Result<Vec<u8>, HostError>>;
//             fn net_request(&mut self, req: NetReq) -> wasmtime::Result<Result<NetResp, HostError>>;
//             fn shim_call(&mut self, shim_id: String, msg: Vec<u8>)
//                 -> wasmtime::Result<Result<Vec<u8>, HostError>>;
//             fn log_emit(&mut self, level: LogLevel, msg: String,
//                         fields: Vec<(String, String)>) -> wasmtime::Result<()>;
//             fn receipt_emit(&mut self, r: Receipt) -> wasmtime::Result<()>;
//         }
//
//         /// Generated linker entry point. Two pre-built `Linker<HostState>`
//         /// instances are built by `HostFunctionRegistry` calling this.
//         pub fn add_to_linker<T, U>(
//             linker: &mut wasmtime::component::Linker<T>,
//             get: impl Fn(&mut T) -> &mut U + Send + Sync + Copy + 'static,
//         ) -> wasmtime::Result<()>
//         where U: Host;
//     }
//
//     // WIT records → Rust structs (all integer-only per HARD #3).
//     pub struct ContentRef { pub sha256: String, pub size_bytes: u64 }
//     pub struct NetReq {
//         pub url: String,
//         pub method: String,
//         pub headers: Vec<(String, String)>,
//         pub body: Vec<u8>,
//         pub grant_id: String,
//     }
//     pub struct NetResp {
//         pub status: u16,
//         pub headers: Vec<(String, String)>,
//         pub body_ref: ContentRef,
//     }
//     pub struct Receipt {
//         pub action_id: u64,
//         pub status: ReceiptStatus,
//         pub emitted_at_ms: u64,
//         pub payload_canonical_bytes: Vec<u8>,
//     }
//     pub enum ReceiptStatus { Ok, Error }
//     pub enum LogLevel { Trace, Debug, Info, Warn, Error }
//
//     // Discriminated host-error from `wit/loom-surface.wit`.
//     pub enum HostError { /* see error_mapper::HostError mirror */ }
// }
// ```
//
// # What this stub asserts
//
// 1. The trait `host::Host` is the SOLE WASM↔core bridge.
// 2. The 8 host-fn signatures in the trait are byte-identical to the
//    `wit/loom-surface.wit::interface host` declaration; any drift fails
//    the build via `wit-bindgen --check`.
// 3. No hand-rolled `extern "C"` or `unsafe` host signatures are
//    permitted in the `loom-host` crate. CI lint
//    `tools/lint-no-extern-host-fns.py` walks the crate and rejects.
// 4. `add_to_linker<T, U: Host>` is the ONLY way to register host-fns
//    against a `wasmtime::component::Linker` — the two-linker mode-flip
//    in `HostFunctionRegistry` calls this twice (once with live state,
//    once with replay state).
//
// # Runtime exports surfaced to other loom-host modules
//
// The placeholder `Marshaller` struct below exists so the rest of the
// crate compiles at the interface stage. It is later replaced wholesale
// by the `wit-bindgen` output module.

use loom_core::error::LoomError;
use serde::{Deserialize, Serialize};

/// Mode tag (live vs replay). Used by `HostFunctionRegistry` to select
/// which `Linker<HostState>` to instantiate against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Live,
    Replay,
}

/// Placeholder marshaller surface. Replaced with the generated
/// `loom_surface_bindings` module.
pub struct Marshaller;

impl Marshaller {
    /// Returns a placeholder marshaller. Scaffold replaced with
    /// wit-bindgen generated output.
    pub fn generated_or_panic() -> Result<Self, LoomError> {
        Ok(Self)
    }
}
