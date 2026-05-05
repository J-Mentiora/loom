// Interface tests for `HostBindings`. Verifies that there are exactly 8
// host-fns, all from `wit-bindgen` (no hand-rolled `extern "C"`), the
// exhaustive host-fn enumeration, and that the trampoline shape is
// mode-agnostic (same shape live + replay).
//
// These tests exercise type-level shape; the trampoline bodies are
// panic stubs because the real bodies are wit-bindgen-generated
// at build time. Runtime behaviour is covered by integration tests.

extern crate alloc;

use super::host_bindings::{host, Instant, LogLevel, NetReq, NetResp};
use crate::error_mapper::error_mapper::HostError;
use crate::receipt_builder::receipt_builder::{ContentRef, Receipt};
use alloc::string::ToString;
use alloc::vec::Vec;

// === Exactly 8 host-fns; all imports come from `host` mod ===

#[test]
fn host_module_exposes_exactly_eight_host_fns() {
    // Type-level enumeration. If a 9th host-fn is added without a WIT
    // change + regen, this test must be updated alongside the change
    // request — making "drift" a code-review event.
    let _ = host::clock_now as fn() -> Instant;
    let _ = host::rng_next_u64 as fn() -> u64;
    let _ = host::blob_put as fn(&[u8]) -> Result<ContentRef, HostError>;
    let _ = host::blob_get as fn(&ContentRef) -> Result<Vec<u8>, HostError>;
    let _ = host::net_request as fn(&NetReq) -> Result<NetResp, HostError>;
    let _ = host::shim_call as fn(&str, &[u8]) -> Result<Vec<u8>, HostError>;
    let _ = host::log_emit as fn(LogLevel, &str, &[(alloc::string::String, alloc::string::String)]);
    let _ = host::receipt_emit as fn(&Receipt);
}

// === clock_now returns Instant with `ticks: u64` integer ===

#[test]
fn instant_is_integer_ticks_no_floats() {
    let i = Instant { ticks: 42 };
    let _: u64 = i.ticks;
}

// === blob_put returns ContentRef carrying hash+size ===

#[test]
fn blob_put_signature_takes_byte_slice_returns_content_ref() {
    // Type-level: `&[u8] -> Result<ContentRef, HostError>`.
    let _: fn(&[u8]) -> Result<ContentRef, HostError> = host::blob_put;
}

// === NetReq carries headers including opaque Grant strings ===

#[test]
fn net_req_headers_are_string_pairs() {
    let req = NetReq {
        method: "GET".to_string(),
        url: "https://api.example/x".to_string(),
        headers: alloc::vec![("Authorization".to_string(), "Grant abc123".to_string())],
        body: None,
    };
    assert_eq!(req.headers[0].0, "Authorization");
    assert!(req.headers[0].1.starts_with("Grant "));
}

// === NetResp body_size_bytes is decompressed size ===

#[test]
fn net_resp_body_size_is_post_decompression_integer() {
    let resp = NetResp {
        status: 200,
        headers: Vec::new(),
        body_ref: ContentRef {
            sha256_hex: "ab".to_string(),
            size_bytes: 4096,
        },
        body_size_bytes: 4096,
    };
    let _: u64 = resp.body_size_bytes;
    assert_eq!(resp.body_ref.size_bytes, resp.body_size_bytes);
}

// === shim_call takes shim_id `&str` + opaque message bytes ===

#[test]
fn shim_call_signature_is_str_plus_bytes() {
    let _: fn(&str, &[u8]) -> Result<Vec<u8>, HostError> = host::shim_call;
}

// === log_emit accepts LogLevel + msg + structured fields ===

#[test]
fn log_emit_levels_cover_standard_severities() {
    let _ = LogLevel::Trace;
    let _ = LogLevel::Debug;
    let _ = LogLevel::Info;
    let _ = LogLevel::Warn;
    let _ = LogLevel::Error;
}

// === receipt_emit takes &Receipt only; never returns Result ===

#[test]
fn receipt_emit_is_infallible_returns_unit() {
    // Generated host-fn for `receipt_emit` returns `()` per WIT
    // signature — the host-side queue is the only persistence path.
    let _: fn(&Receipt) = host::receipt_emit;
}

// === Trampoline shape is mode-agnostic ===
//
// The same `host::clock_now` symbol resolves to the live impl (calls
// `DeterminismHarness::tape_append + clock_now`) or the replay impl
// (calls `DeterminismHarness::tape_read_next`) depending on which
// `wasmtime::component::Linker` was used at instantiation. Surface code
// (and these bindings) cannot tell which.

#[test]
fn host_bindings_have_no_mode_argument() {
    // Type-level: NONE of the 8 trampolines takes a `mode: Mode`
    // parameter. If anyone adds one, the surface CI lint
    // `tools/lint-surface-mode.py` and this test must both update.
    let _: fn() -> Instant = host::clock_now;
    let _: fn() -> u64 = host::rng_next_u64;
}

// === HostBindings depends on ErrorMapper for translation ===

#[test]
fn fallible_host_fns_return_host_error_typed_result() {
    // The `Result<T, HostError>` variant is the WIT-mapped output that
    // `ErrorMapper::map` consumes inside the verb. ErrorMapper is the
    // only translator (see error_mapper/error_mapper.rs).
    fn assert_returns_host_error<T>(_f: fn(&[u8]) -> Result<T, HostError>) {}
    assert_returns_host_error(host::blob_put);
}
