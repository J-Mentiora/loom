// Interface tests for `WitTypeMarshaller`. The real bindings are
// `wasmtime::component::bindgen!`-generated; these tests pin a couple
// of invariants on the wrapper module that survive bindgen regeneration.

use super::wit_type_marshaller::{Marshaller, Mode};

#[test]
fn mode_enum_has_exactly_live_and_replay() {
    // The mode tag drives the two-linker design.
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
    // `HostFunctionTable` can `use loom_host::wit_type_marshaller::*;`.
    fn _ck() -> Result<Marshaller, loom_core::error::LoomError> {
        Marshaller::generated_or_panic()
    }
    let _ = _ck;
}
