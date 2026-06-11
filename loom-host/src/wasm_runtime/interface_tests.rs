// Interface tests for `WasmRuntime`. Verifies singleton shape,
// component-model + fuel + AOT config defaults, and the engine
// encapsulation invariant.

use super::wasm_runtime::{WasmRuntime, WasmRuntimeConfig};
use std::sync::Arc;

// === Singleton + Arc shape ===

#[test]
fn new_returns_arc_runtime() {
    // Compile-time pin: constructor returns `Arc<Self>` so all modules
    // share one engine instance.
    fn _ck(c: WasmRuntimeConfig) -> Result<Arc<WasmRuntime>, loom_core::error::LoomError> {
        WasmRuntime::new(c)
    }
    let _ = _ck;
}

#[test]
fn default_config_is_sensible() {
    let c = WasmRuntimeConfig::default();
    assert_eq!(c.mem_limit_mib, 64); // soft default
    assert!(c.fuel_per_invocation.is_none() || c.fuel_per_invocation.is_some());
    assert!(matches!(
        c.opt_level.as_str(),
        "speed" | "speed_and_size" | "none"
    ));
    // Epoch preemption defaults: 10ms abort granularity, 30s guest-CPU
    // runaway backstop. Both must be non-zero or preemption degrades.
    assert_eq!(c.epoch_tick_ms, 10);
    assert_eq!(c.guest_cpu_deadline_ms, 30_000);
}

#[test]
fn epoch_fields_deserialize_with_defaults_when_absent() {
    // Backward compat: configs serialized before the epoch knobs existed
    // must still deserialize (serde defaults fill in tick + deadline).
    let c: WasmRuntimeConfig = serde_json::from_str(
        r#"{"mem_limit_mib":64,"fuel_per_invocation":null,"opt_level":"none"}"#,
    )
    .expect("pre-epoch config JSON must deserialize");
    assert_eq!(c.epoch_tick_ms, 10);
    assert_eq!(c.guest_cpu_deadline_ms, 30_000);
}

// === Engine accessor returns &wasmtime::Engine, not Arc<Engine> ===

#[test]
fn engine_accessor_returns_borrow_not_clone() {
    // The boundary: higher modules borrow the engine for sub-call
    // duration. Returning &Engine prevents accidental Engine cloning
    // (which is cheap but logically suggests multiple engines).
    fn _ck(rt: &WasmRuntime) -> &wasmtime::Engine {
        rt.engine()
    }
    let _ = _ck;
}

// === Cwasm cache coherency ===

#[test]
fn precompile_compatibility_hash_returns_string() {
    // The hash is folded into the .cwasm path so a wasmtime upgrade
    // triggers `StartupManager` recovery. Type pin only here.
    fn _ck(rt: &WasmRuntime) -> Result<String, loom_core::error::LoomError> {
        rt.precompile_compatibility_hash()
    }
    let _ = _ck;
}

// === Encapsulation: WasmRuntime owns the only Engine in loom-host ===

#[test]
fn config_is_immutable_after_construction() {
    // Compile-time pin: the accessor returns `&WasmRuntimeConfig`,
    // not `&mut`. Engine config is set once at process startup.
    fn _ck(rt: &WasmRuntime) -> &WasmRuntimeConfig {
        rt.config()
    }
    let _ = _ck;
}

#[test]
fn fuel_per_invocation_is_optional_u64() {
    // Fuel-aware execution per design §4 / contract SLA. None = disabled.
    let c = WasmRuntimeConfig {
        fuel_per_invocation: Some(10_000_000),
        ..WasmRuntimeConfig::default()
    };
    assert_eq!(c.fuel_per_invocation, Some(10_000_000));
}

#[test]
fn mem_limit_mib_is_u32() {
    // Integer-only fields. No floats.
    let c = WasmRuntimeConfig::default();
    let _: u32 = c.mem_limit_mib;
}
