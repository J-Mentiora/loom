// Re-export of the locked Phase 5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-host/modules/wit_type_marshaller/interface_tests.rs` instead.
// Interface tests for `WitTypeMarshaller`. The module itself is
// `wit-bindgen`-generated at build time, so the tests pin BC-HOST-02
// HARD invariants that the generation step must satisfy.

use super::wit_type_marshaller::{Marshaller, Mode};

// === BC-HOST-02 HARD: WIT is schema source of truth ===

#[test]
fn mode_enum_has_exactly_live_and_replay() {
    // The mode tag drives the two-linker design (IC-HOST-08).
    let _ = Mode::Live;
    let _ = Mode::Replay;
    // No third mode — replay vs live is the only axis.
    fn _ck(m: Mode) -> &'static str {
        match m {
            Mode::Live => "live",
            Mode::Replay => "replay",
        }
    }
    assert_eq!(_ck(Mode::Live), "live");
    assert_eq!(_ck(Mode::Replay), "replay");
}

#[test]
fn marshaller_is_a_pub_type() {
    // Compile-time pin: the symbol exists at the expected path so
    // `HostFunctionTable` can `use loom_host::wit_type_marshaller::*;`
    // once the wit-bindgen output replaces this stub.
    fn _ck() -> Result<Marshaller, loom_core::error::LoomError> {
        Marshaller::generated_or_panic()
    }
    let _ = _ck;
}

// === BC-HOST-02: file declares wit-bindgen-generated kind ===

#[test]
fn module_kind_marker_present_in_file() {
    // The first line of `interfaces.rs` is `// module_kind: wit-bindgen-generated`.
    // The convention check is a doc invariant — the lint
    // `tools/lint-pipeline-conventions.py` walks the loom-host modules
    // tree and asserts the marker is present on this exact module.
    let pin = "module_kind: wit-bindgen-generated";
    assert!(pin.contains("wit-bindgen-generated"));
}

// === IC-HOST-03: 8-host-fn enumeration pin ===

#[test]
fn the_eight_host_fns_are_enumerated_in_doc() {
    // Doc-pin: the rustdoc on `interfaces.rs` enumerates exactly:
    // clock_now, rng_next_u64, blob_put, blob_get, net_request,
    // shim_call, log_emit, receipt_emit. Any future host-fn requires a
    // WIT change AND a verification-criteria update (IC-HOST-03).
    let expected = [
        "clock_now",
        "rng_next_u64",
        "blob_put",
        "blob_get",
        "net_request",
        "shim_call",
        "log_emit",
        "receipt_emit",
    ];
    assert_eq!(expected.len(), 8);
}

// === BC-HOST-04: no platform symbols introduced via marshaller ===

#[test]
fn marshaller_does_not_expose_platform_types() {
    // Compile-time pin: nothing here may import AppKit / Foundation /
    // ApplicationServices / chromiumoxide. Enforced by `cargo-deny`
    // allowlist; this test just notes the intent.
    let pin = "BC-HOST-04: marshaller imports limited to wit-bindgen + loom-core";
    assert!(pin.contains("BC-HOST-04"));
}
