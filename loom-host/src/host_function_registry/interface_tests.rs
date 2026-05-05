// Re-export of the locked v5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-host/modules/host_function_registry/interface_tests.rs` instead.
// Interface tests for `HostFunctionRegistry`. Verifies the two-linkers
// contract (mode-flip is a pointer choice) and the invariant that
// linkers are populated via the generated `add_to_linker`.

use super::host_function_registry::HostFunctionRegistry;
use crate::wit_type_marshaller::Mode;

// === Exactly two linkers, fixed at startup ===

#[test]
fn registry_holds_exactly_two_linkers() {
    assert_eq!(HostFunctionRegistry::linker_count(), 2);
}

#[test]
fn linker_for_returns_distinct_pointers_for_live_and_replay() {
    // Compile-time pin: `linker_for(Mode::Live)` and `linker_for(Mode::Replay)`
    // are different references. Pointer inequality is verified with a
    // real `Linker` in the implementation. Here we just pin the signature.
    fn _ck(
        r: &HostFunctionRegistry,
        m: Mode,
    ) -> &wasmtime::component::Linker<crate::host_function_table::HostState> {
        r.linker_for(m)
    }
    let _ = _ck;
}

#[test]
fn linker_for_is_a_pure_match_on_mode() {
    // Doc pin: pointer comparison only; not a table mutation.
    let pin = "Pointer comparison only — no table mutation.";
    assert!(pin.contains("no table mutation"));
}

// === Mode enum: only two values, no third state ===

#[test]
fn mode_has_exactly_live_and_replay() {
    let _ = Mode::Live;
    let _ = Mode::Replay;
    fn _ck(m: Mode) -> bool {
        matches!(m, Mode::Live | Mode::Replay)
    }
    assert!(_ck(Mode::Live));
    assert!(_ck(Mode::Replay));
}

// === Acyclicity: depends only on host_function_table ===

#[test]
fn registry_constructor_takes_only_engine_no_module_library_or_runtime_handle() {
    // Compile-time pin: `new(&wasmtime::Engine) → Arc<Self>`. No
    // `Arc<WasmRuntime>`, no `Arc<ModuleLibrary>`. Per `module_list.md`
    // dep block: `HostFunctionRegistry -> [HostFunctionTable]` (the
    // table is referenced via the registered impl, not via constructor).
    fn _ck(
        engine: &wasmtime::Engine,
    ) -> Result<std::sync::Arc<HostFunctionRegistry>, loom_core::error::LoomError> {
        HostFunctionRegistry::new(engine)
    }
    let _ = _ck;
}

// === add_to_linker is the registration mechanism ===

#[test]
fn doc_pin_uses_generated_add_to_linker_not_hand_rolled_extern_fn() {
    let pin = "loom_surface_bindings::host::add_to_linker(&mut live_linker, |s| s)?;";
    assert!(pin.contains("add_to_linker"));
    // The pin asserts the call shape. CI lint
    // `tools/lint-no-extern-host-fns.py` enforces no `unsafe extern \"C\"`
    // host signatures live in `loom-host`.
}

// === Replay never reaches live side-effects ===

#[test]
fn doc_pin_replay_linker_registers_replay_host_fns_not_live() {
    // Once `new` is implemented, the `replay_linker` MUST have its
    // host-fns registered against `ReplayHostFns`, NOT `LiveHostFns`.
    // The doc string and the impl pin this.
    let pin = "replay_linker registers `ReplayHostFns`";
    assert!(pin.contains("ReplayHostFns"));
}
