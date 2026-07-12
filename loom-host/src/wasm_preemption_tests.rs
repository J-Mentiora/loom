// Regression tests for epoch-based guest preemption (audit 2026-06-10:
// "Guest WASM CPU loops are unpreemptable"). A busy-loop component —
// `(loop (br 0))` behind the WIT action signature — is dispatched
// through the REAL `SessionExecutor::run` path and must be
// interruptible three ways:
//   (a) `abort_signal` preempts the `select!` mid-loop (epoch yields),
//   (b) the hard guest-CPU epoch deadline traps a runaway invocation
//       into the normal `ActionOutcome::Trapped` outcome,
//   (c) the `fuel_per_invocation` knob, when Some, actually loads fuel
//       into the store (it was unwired before this fix) and exhaustion
//       traps with `OutOfFuel`.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tokio::sync::Notify;

use crate::compiler::Compiler;
use crate::host_function_registry::HostFunctionRegistry;
use crate::host_observability::HostObservability;
use crate::module_library::ModuleLibrary;
use crate::receipt_marshaller::ReceiptMarshaller;
use crate::session_executor::{Action, ActionOutcome, SessionExecutor, SessionHandle};
use crate::shim_manager::ShimManager;
use crate::trap_handler::TrapHandler;
use crate::wasm_runtime::{WasmRuntime, WasmRuntimeConfig};
use crate::wit_type_marshaller::Mode;
use loom_core::core_api_facade::{CoreApiFacade, CoreConfig};
use loom_core::error::LoomErrorCode;
use loom_core::manifest_writer::SessionId;
use loom_shared::types::{EpochMs, Seed};

/// A component whose exported `busy` function matches the WIT action
/// signature (`record action` in → `result<receipt, host-error>` out;
/// see `build_action_val` / wit/loom-surface.wit) but never returns:
/// the core body is `(loop (br 0))`. Found by `lookup_surface_export`'s
/// flat top-level fallback. Without epoch interruption this loop is
/// unpreemptable and pins a tokio worker forever.
const BUSY_LOOP_COMPONENT_WAT: &str = r#"
(component
  (core module $m
    (memory (export "memory") 1)
    (func (export "realloc") (param i32 i32 i32 i32) (result i32) i32.const 1024)
    (func (export "busy") (param i32 i32 i32 i32 i64) (result i32)
      (loop (br 0))
      unreachable))
  (core instance $i (instantiate $m))
  (alias core export $i "memory" (core memory $mem))
  (alias core export $i "realloc" (core func $realloc))
  (type $action (record
    (field "kind" string)
    (field "payload" (list u8))
    (field "deadline-ms" u64)))
  (type $receipt (record
    (field "action-hash" string)
    (field "outcome-hash" string)
    (field "emitted-at-ms" u64)))
  (type $host-error (variant (case "internal" string)))
  (export $action_e "action" (type $action))
  (export $receipt_e "receipt" (type $receipt))
  (export $host_error_e "host-error" (type $host-error))
  (func $busy (param "action" $action_e) (result (result $receipt_e (error $host_error_e)))
    (canon lift (core func $i "busy") (memory $mem) (realloc $realloc) string-encoding=utf8))
  (export "busy" (func $busy))
)
"#;

const SURFACE: &str = "busy_surface";

struct Harness {
    executor: Arc<SessionExecutor>,
    registry: Arc<HostFunctionRegistry>,
    core: Arc<CoreApiFacade>,
    obs: Arc<HostObservability>,
    _dir: TempDir,
}

/// Build the full executor stack (real core facade, real library, real
/// trap handler) around the busy-loop component, with the given runtime
/// config. Mirrors `WasmHost::new` wiring.
fn make_harness(config: WasmRuntimeConfig) -> Harness {
    let dir = TempDir::new().expect("tempdir");
    let core = CoreApiFacade::new(
        CoreConfig {
            data_root: dir.path().join("data"),
            log_path: dir.path().join("daemon.log"),
            otel_enabled: false,
            default_seed: 42,
            checkpoint_every_n: 100,
        },
        Arc::new(loom_keychain::StubKeychain),
    )
    .expect("CoreApiFacade::new");

    let runtime = WasmRuntime::new(config).expect("WasmRuntime::new");
    let surfaces = dir.path().join("surfaces");
    std::fs::create_dir_all(&surfaces).expect("mkdir surfaces");
    let wat_src = surfaces.join(format!("{SURFACE}.wat"));
    std::fs::write(&wat_src, BUSY_LOOP_COMPONENT_WAT).expect("write wat");
    Compiler::new(runtime.clone())
        .compile_module(&wat_src, &surfaces.join(format!("{SURFACE}.cwasm")))
        .expect("compile busy-loop component");

    let library = ModuleLibrary::new(runtime.clone(), surfaces);
    let failures = library.load_all().expect("load_all");
    assert!(failures.is_empty(), "load failures: {failures:?}");

    let registry = HostFunctionRegistry::new(runtime.engine()).expect("registry");
    let obs = HostObservability::new(true);
    let trap_handler = TrapHandler::new(obs.clone());
    let executor = SessionExecutor::new(runtime, library, trap_handler, obs.clone());
    Harness {
        executor,
        registry,
        core,
        obs,
        _dir: dir,
    }
}

fn make_session(h: &Harness, abort_signal: Arc<Notify>) -> SessionHandle {
    SessionHandle {
        session_id: SessionId("01PREEMPTTEST".into()),
        handle: tokio::runtime::Handle::current(),
        receipt_pool: tokio::runtime::Handle::current(),
        abort_flag: Arc::new(AtomicBool::new(false)),
        abort_signal,
        kill_reason: Arc::new(parking_lot::Mutex::new(None)),
        seed: Seed(42),
        epoch_ms: EpochMs(0),
        no_blocklist: false,
        no_determinism: false,
        audio: false,
        profile: "safe".into(),
        downloads_dir: None,
        capture_dom_after: false,
        determinism: Arc::new(loom_core::determinism_harness::DeterminismHarness::new(
            42,
            h.core.manifest_writer(),
        )),
    }
}

fn make_host_state(h: &Harness) -> crate::host_function_table::HostState {
    let determinism = h.core.determinism();
    let tape_writer = determinism.new_tape_writer();
    crate::host_function_table::HostState {
        core: h.core.clone(),
        determinism,
        budget: h.core.budget_enforcer(),
        session_id: SessionId("01PREEMPTTEST".into()),
        action_id: 1,
        mode: Mode::Live,
        receipt_builder: crate::receipt_marshaller::ReceiptBuilder {
            action_id: 1,
            ..Default::default()
        },
        side_effects: Default::default(),
        host_call_metrics: Vec::new(),
        shim_manager: ShimManager::new(h.obs.clone()),
        obs: h.obs.clone(),
        tape_writer,
        replay_table: None,
        wasi_ctx: crate::host_function_table::build_sandboxed_wasi_ctx(),
        wasi_table: wasmtime::component::ResourceTable::new(),
        seed: Seed(42),
        epoch_ms: EpochMs(0),
        no_blocklist: false,
        no_determinism: false,
        audio: false,
        profile: "safe".into(),
        downloads_dir: None,
        capture_dom_after: false,
        cookie_capture: std::sync::Arc::new(parking_lot::Mutex::new(None)),
    }
}

fn busy_action() -> Action {
    Action {
        action_id: 1,
        surface: SURFACE.into(),
        method: "busy".into(),
        args_canonical_bytes: vec![],
        deadline_ms: None,
    }
}

async fn run_busy(h: &Harness, abort_signal: Arc<Notify>) -> ActionOutcome {
    run_busy_action(h, abort_signal, busy_action()).await
}

async fn run_busy_action(h: &Harness, abort_signal: Arc<Notify>, action: Action) -> ActionOutcome {
    let linker = h.registry.linker_for(Mode::Live);
    let host_state = make_host_state(h);
    let session = make_session(h, abort_signal);
    tokio::time::timeout(
        Duration::from_secs(30),
        h.executor
            .run(action, session, Mode::Live, linker, host_state),
    )
    .await
    .expect("busy-loop guest must be preemptible — run() hung past 30s")
    .expect("run must return Ok(ActionOutcome), not Err")
}

/// (a) A user abort fires mid-busy-loop: the epoch yield re-polls the
/// `select!` so the `abort_signal` arm wins within the tick cadence.
/// Pre-fix this test hangs (the call future never yields).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abort_signal_preempts_cpu_bound_guest_loop() {
    let h = make_harness(WasmRuntimeConfig {
        opt_level: "none".into(),
        epoch_tick_ms: 5,
        guest_cpu_deadline_ms: 60_000, // far away: abort, not the deadline, must win
        ..WasmRuntimeConfig::default()
    });
    let abort_signal = Arc::new(Notify::new());
    let notifier = abort_signal.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        notifier.notify_one();
    });
    let outcome = run_busy(&h, abort_signal).await;
    assert!(
        matches!(outcome, ActionOutcome::Aborted { .. }),
        "abort during a CPU-bound guest loop must yield ActionOutcome::Aborted"
    );
}

/// (b) With no abort, the hard guest-CPU epoch deadline traps the
/// runaway loop and it surfaces as the NORMAL trapped outcome
/// (SurfaceTrap via TrapHandler), not a hang or an internal error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn epoch_deadline_traps_runaway_guest_loop() {
    let h = make_harness(WasmRuntimeConfig {
        opt_level: "none".into(),
        epoch_tick_ms: 5,
        guest_cpu_deadline_ms: 100,
        ..WasmRuntimeConfig::default()
    });
    let outcome = run_busy(&h, Arc::new(Notify::new())).await;
    match outcome {
        ActionOutcome::Trapped { loom_error, .. } => {
            assert_eq!(loom_error.code, LoomErrorCode::SurfaceTrap);
            assert!(
                loom_error.message.contains("Interrupt"),
                "deadline trap must be Trap::Interrupt; got: {}",
                loom_error.message
            );
        }
        ActionOutcome::Success { .. } => panic!("busy loop cannot succeed"),
        ActionOutcome::Aborted { .. } => panic!("no abort was signalled"),
    }
}

/// (c) `fuel_per_invocation` is wired: when Some, the store is fueled
/// per dispatch and exhaustion traps with OutOfFuel. (Pre-fix the knob
/// enabled `consume_fuel` without any `set_fuel`, so even legitimate
/// guests would trap instantly — and with it unset, fuel did nothing.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fuel_per_invocation_knob_traps_busy_loop_out_of_fuel() {
    let h = make_harness(WasmRuntimeConfig {
        opt_level: "none".into(),
        fuel_per_invocation: Some(1_000_000),
        guest_cpu_deadline_ms: 60_000, // fuel, not the epoch deadline, must fire
        ..WasmRuntimeConfig::default()
    });
    let outcome = run_busy(&h, Arc::new(Notify::new())).await;
    match outcome {
        ActionOutcome::Trapped { loom_error, .. } => {
            assert_eq!(loom_error.code, LoomErrorCode::SurfaceTrap);
            assert!(
                loom_error.message.contains("OutOfFuel"),
                "fuel exhaustion must be Trap::OutOfFuel; got: {}",
                loom_error.message
            );
        }
        other => panic!(
            "fueled busy loop must trap OutOfFuel, got {}",
            match other {
                ActionOutcome::Success { .. } => "Success",
                ActionOutcome::Aborted { .. } => "Aborted",
                ActionOutcome::Trapped { .. } => unreachable!(),
            }
        ),
    }
}

/// (d) A per-action `deadline_ms` kills a slow/hanging guest DAEMON-side with a
/// typed `request_timeout` trapped outcome — distinct from both the abort path
/// (`Aborted`) and the hard guest-CPU epoch deadline (`SurfaceTrap`). The
/// guest-CPU deadline is parked far away so the per-action deadline arm — NOT the
/// epoch trap — is what fires. This is the unit-level proof of the enforcement
/// half: the busy-loop guest stands in for any hanging action (navigate, etc.).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn per_action_deadline_traps_request_timeout() {
    let h = make_harness(WasmRuntimeConfig {
        opt_level: "none".into(),
        epoch_tick_ms: 5, // guest yields so the select! deadline arm can win
        guest_cpu_deadline_ms: 60_000, // far away: the per-action deadline must win
        ..WasmRuntimeConfig::default()
    });
    let action = Action {
        deadline_ms: Some(100),
        ..busy_action()
    };
    let started = std::time::Instant::now();
    let outcome = run_busy_action(&h, Arc::new(Notify::new()), action).await;
    match outcome {
        ActionOutcome::Trapped { loom_error, .. } => {
            assert_eq!(
                loom_error.code,
                LoomErrorCode::RequestTimeout,
                "a per-action deadline kill must be RequestTimeout, got {:?} ({})",
                loom_error.code,
                loom_error.message
            );
            assert_eq!(loom_error.code.as_wire(), "request_timeout");
            assert!(
                loom_error.message.contains("deadline_ms"),
                "timeout message must attribute the action deadline; got: {}",
                loom_error.message
            );
            assert_ne!(
                loom_error.code,
                LoomErrorCode::SurfaceTrap,
                "a deadline kill must NOT be conflated with the epoch-deadline SurfaceTrap"
            );
        }
        other => panic!(
            "deadlined busy loop must trap RequestTimeout, got {}",
            match other {
                ActionOutcome::Success { .. } => "Success",
                ActionOutcome::Aborted { .. } => "Aborted",
                ActionOutcome::Trapped { .. } => unreachable!(),
            }
        ),
    }
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "deadline_ms=100 must fire long before the 60s guest-CPU deadline; took {:?}",
        started.elapsed()
    );
}

/// (e) Control: with NO `deadline_ms`, the deadline arm is INERT — a user abort
/// still wins mid-loop (proving the new `select!` arm never spuriously fires for
/// the default no-deadline dispatch, i.e. byte-for-byte unchanged behavior).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_deadline_keeps_arm_inert() {
    let h = make_harness(WasmRuntimeConfig {
        opt_level: "none".into(),
        epoch_tick_ms: 5,
        guest_cpu_deadline_ms: 60_000,
        ..WasmRuntimeConfig::default()
    });
    let abort_signal = Arc::new(Notify::new());
    let notifier = abort_signal.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        notifier.notify_one();
    });
    // busy_action() has deadline_ms: None.
    let outcome = run_busy(&h, abort_signal).await;
    assert!(
        matches!(outcome, ActionOutcome::Aborted { .. }),
        "with no deadline_ms the abort must still win — the deadline arm is inert"
    );
}

// === capture-policy=fingerprint host fn (capture_dom_after_hash) gating ===
// The host fn is the opt-in cost gate + best-effort capture. These exercise the
// branches that need NO live shim round-trip (tier-off / replay early-returns)
// and the best-effort failure path (tier-on but the shim isn't registered →
// `send` errors fast → Ok(None), never failing the interaction).
use crate::wit_type_marshaller::loom_surface_bindings::loom::surface::host::Host as _LoomHost;

#[tokio::test]
async fn capture_dom_after_hash_returns_none_when_tier_off() {
    let h = make_harness(WasmRuntimeConfig::default());
    let mut hs = make_host_state(&h); // make_host_state sets capture_dom_after = false
    assert!(!hs.capture_dom_after);
    let out = hs.capture_dom_after_hash().await.expect("host fn ok");
    assert!(
        out.is_none(),
        "non-fingerprint tier must not capture (no round-trip)"
    );
}

#[tokio::test]
async fn capture_dom_after_hash_returns_none_in_replay() {
    let h = make_harness(WasmRuntimeConfig::default());
    let mut hs = make_host_state(&h);
    hs.capture_dom_after = true;
    hs.mode = Mode::Replay;
    let out = hs.capture_dom_after_hash().await.expect("host fn ok");
    assert!(out.is_none(), "replay never drives the shim → None");
}

#[tokio::test]
async fn capture_dom_after_hash_best_effort_none_on_send_failure() {
    // Tier ON + Live, but no "chromium:<session>" shim registered → shim_manager
    // .send errors immediately. Best-effort (D6): the host fn swallows it and
    // returns Ok(None) rather than failing the interaction.
    let h = make_harness(WasmRuntimeConfig::default());
    let mut hs = make_host_state(&h);
    hs.capture_dom_after = true;
    assert_eq!(hs.mode, Mode::Live);
    let out = hs
        .capture_dom_after_hash()
        .await
        .expect("best-effort: capture failure must NOT error the interaction");
    assert!(
        out.is_none(),
        "send failure → dom_after_hash omitted (best-effort)"
    );
}
