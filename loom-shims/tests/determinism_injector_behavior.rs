// Behavior tests for ChromiumDeterminismInjector — TDD Red phase.
//
// AC coverage:
//   test_inject_sends_add_script_method_with_run_immediately_true
//               test_inject_idempotent_second_call_is_noop
//               test_inject_empty_source_returns_empty_source_error
//               test_script_source_has_required_markers
//  .1: test_determinism_init_js_markers_for_animation_disabling
//  .1: test_supervisor_sets_lc_all_in_child_env (stub)

use ciborium::value::Value;
use loom_shared::types::{EpochMs, Seed};
use loom_shims::cdp_connection::cdp_connection::{
    CdpConnection, CdpError, EventFilter, EventHandler, EventRegistration,
};
use loom_shims::determinism_injector::determinism_injector::{
    build_inject_params, script_source_has_determinism_markers, ChromiumDeterminismInjector,
    DeterminismError, DeterminismInjector, ADD_SCRIPT_METHOD,
};
use loom_shims::ipc_endpoint::ipc_endpoint::{CdpMessage, TargetId};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A mock CdpConnection that records all commands sent (method + params).
struct RecordingCdp {
    calls: Mutex<Vec<(TargetId, String, Value)>>,
    response: Value,
}

impl RecordingCdp {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            response: Value::Map(vec![(
                Value::Text("identifier".into()),
                Value::Text("script-id-1".into()),
            )]),
        })
    }

    fn recorded_methods(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|(_, m, _)| m.clone())
            .collect()
    }

    /// Return all `source` strings sent via `Page.addScriptToEvaluateOnNewDocument`.
    fn recorded_inject_sources(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, m, _)| m == ADD_SCRIPT_METHOD)
            .filter_map(|(_, _, params)| {
                let Value::Map(m) = params else { return None };
                m.iter()
                    .find(|(k, _)| k == &Value::Text("source".into()))
                    .and_then(|(_, v)| {
                        if let Value::Text(s) = v {
                            Some(s.clone())
                        } else {
                            None
                        }
                    })
            })
            .collect()
    }
}

#[async_trait::async_trait]
impl CdpConnection for RecordingCdp {
    async fn connect(&self, _ws_url: &str) -> Result<(), CdpError> {
        Ok(())
    }

    async fn command(
        &self,
        target_id: TargetId,
        msg: CdpMessage,
        _timeout: Option<Duration>,
    ) -> Result<Value, CdpError> {
        self.calls
            .lock()
            .unwrap()
            .push((target_id, msg.method.clone(), msg.params));
        Ok(self.response.clone())
    }

    fn register_event_handler(
        &self,
        _filter: EventFilter,
        _handler: EventHandler,
    ) -> EventRegistration {
        EventRegistration::detached(0)
    }

    fn invalidate_session(&self) {}

    fn is_connected(&self) -> bool {
        true
    }
}

/// A mock CdpConnection that always returns CdpError on `command`.
struct FailingCdp;

#[async_trait::async_trait]
impl CdpConnection for FailingCdp {
    async fn connect(&self, _ws_url: &str) -> Result<(), CdpError> {
        Ok(())
    }
    async fn command(
        &self,
        _target_id: TargetId,
        _msg: CdpMessage,
        _t: Option<Duration>,
    ) -> Result<Value, CdpError> {
        Err(CdpError::Timeout { ms: 0 })
    }
    fn register_event_handler(
        &self,
        _filter: EventFilter,
        _handler: EventHandler,
    ) -> EventRegistration {
        EventRegistration::detached(0)
    }
    fn invalidate_session(&self) {}
    fn is_connected(&self) -> bool {
        true
    }
}

/// Test fixture: a minimal determinism template containing the canonical
/// markers AND the three substitution tokens. Mirrors the structure of
/// the real `assets/determinism_init.js` so `is_determinism_template`
/// returns true and `render_determinism_script` substitutes the tokens.
fn make_script() -> String {
    r#"(function() {
  var _epoch_ms = __LOOM_EPOCH_MS__;
  var _c = (__LOOM_SEED_LO__) | 0;
  var _d = (__LOOM_SEED_HI__) | 0;
  Date.now = function() { return _epoch_ms; };
  Math.random = function() { return 0; };
  window.requestAnimationFrame = function(cb) { cb(0); return 0; };
})();"#
        .to_string()
}

// === KILL: canonical CDP method + runImmediately ===

#[tokio::test]
async fn test_inject_sends_add_script_method_with_run_immediately_true() {
    let cdp = RecordingCdp::new();
    let injector = ChromiumDeterminismInjector::new(cdp.clone(), make_script());

    injector
        .inject(1, Seed(0), EpochMs(0))
        .await
        .expect("inject should succeed");

    let methods = cdp.recorded_methods();
    assert!(
        methods.iter().any(|m| m == ADD_SCRIPT_METHOD),
        "inject must send {ADD_SCRIPT_METHOD}, got: {methods:?}"
    );

    // Verify runImmediately: true was in the params
    let params = build_inject_params(&make_script());
    if let Value::Map(m) = params {
        let key = Value::Text("runImmediately".into());
        let v = m.iter().find(|(k, _)| k == &key).map(|(_, v)| v);
        assert_eq!(
            v,
            Some(&Value::Bool(true)),
            "runImmediately must be true (KILL)"
        );
    } else {
        panic!("expected Map params");
    }
}

#[tokio::test]
async fn test_inject_idempotent_second_call_is_noop() {
    let cdp = RecordingCdp::new();
    let injector = ChromiumDeterminismInjector::new(cdp.clone(), make_script());

    injector
        .inject(1, Seed(0), EpochMs(0))
        .await
        .expect("first inject");
    injector
        .inject(1, Seed(0), EpochMs(0))
        .await
        .expect("second inject — must be noop, not error");

    let methods = cdp.recorded_methods();
    assert_eq!(
        methods
            .iter()
            .filter(|m| m.as_str() == ADD_SCRIPT_METHOD)
            .count(),
        1,
        "second inject must be a no-op (only 1 CDP call, not 2)"
    );
}

#[tokio::test]
async fn test_inject_empty_source_returns_empty_source_error() {
    let cdp = RecordingCdp::new();
    let injector = ChromiumDeterminismInjector::new(cdp, String::new());

    let result = injector.inject(1, Seed(0), EpochMs(0)).await;
    assert!(matches!(result, Err(DeterminismError::EmptySource)));
}

#[test]
fn test_script_source_present_returns_false_for_empty() {
    let cdp = RecordingCdp::new();
    let injector = ChromiumDeterminismInjector::new(cdp, String::new());
    assert!(!injector.script_source_present());
}

#[test]
fn test_script_source_present_returns_true_for_valid_script() {
    let cdp = RecordingCdp::new();
    let injector = ChromiumDeterminismInjector::new(cdp, make_script());
    assert!(injector.script_source_present());
}

// === animation disabling markers ===

#[test]
fn test_script_source_has_required_markers() {
    let script = make_script();
    assert!(
        script_source_has_determinism_markers(&script),
        "script must contain Date.now, Math.random, requestAnimationFrame"
    );
}

#[test]
fn test_determinism_init_js_installs_rng_and_css_but_not_clock_overrides() {
    // Post faithful-entrance-animations: clock determinism moved to CDP virtual
    // time, so the asset installs ONLY the seeded RNG + CSS flattening and must
    // NOT freeze the clocks (a frozen clock stalls JS entrance animations).
    let script = include_str!("../assets/determinism_init.js");
    assert!(
        script.contains("Math.random"),
        "must install seeded Math.random"
    );
    assert!(
        script.contains("animation-duration"),
        "must inject CSS animation/transition flattening"
    );
    assert!(
        !script.contains("performance.now = function"),
        "must NOT override performance.now (drives JS animations via virtual time now)"
    );
    assert!(
        !script.contains("Date.now = function"),
        "must NOT override Date.now (driven by virtual time now)"
    );
    assert!(
        !script.contains("requestAnimationFrame = function"),
        "must NOT override requestAnimationFrame (native rAF runs on virtual time)"
    );
}

#[tokio::test]
async fn test_clear_target_removes_from_registry() {
    let cdp = RecordingCdp::new();
    let injector = ChromiumDeterminismInjector::new(cdp, make_script());

    injector
        .inject(1, Seed(0), EpochMs(0))
        .await
        .expect("inject");
    injector.clear_target(1);

    // After clear, the target is no longer registered (next inject should fire CDP again).
    // This is tested indirectly — re-inject after clear should send a new CDP command.
    let cdp2 = RecordingCdp::new();
    let injector2 = ChromiumDeterminismInjector::new(cdp2.clone(), make_script());
    injector2
        .inject(1, Seed(0), EpochMs(0))
        .await
        .expect("first inject");
    injector2.clear_target(1);
    injector2
        .inject(1, Seed(0), EpochMs(0))
        .await
        .expect("re-inject after clear");
    let methods = cdp2.recorded_methods();
    assert_eq!(
        methods
            .iter()
            .filter(|m| m.as_str() == ADD_SCRIPT_METHOD)
            .count(),
        2,
        "re-inject after clear must send 2 CDP calls total"
    );
}

// === per-session seed substitution on the wire ===

/// J.2: the seed value supplied to `inject` is rendered into the script
/// source actually sent via `Page.addScriptToEvaluateOnNewDocument`.
#[tokio::test]
async fn test_inject_substitutes_seed_into_wire_source() {
    let cdp = RecordingCdp::new();
    let injector = ChromiumDeterminismInjector::new(cdp.clone(), make_script());
    injector
        .inject(7, Seed(42), EpochMs(1700))
        .await
        .expect("inject");

    let sources = cdp.recorded_inject_sources();
    assert_eq!(sources.len(), 1, "exactly one inject command sent");
    let s = &sources[0];
    assert!(
        s.contains("0x0000002a"),
        "seed_lo=0x2a (42) must appear in rendered source"
    );
    assert!(
        s.contains("0x00000000"),
        "seed_hi=0 must appear (seed fits in u32)"
    );
    assert!(s.contains("1700"), "epoch_ms=1700 must appear");
    assert!(
        !s.contains("__LOOM_SEED_LO__"),
        "tokens must be substituted, not present"
    );
    assert!(!s.contains("__LOOM_SEED_HI__"));
    assert!(!s.contains("__LOOM_EPOCH_MS__"));
}

/// J.2: two injectors with different seeds produce different rendered sources.
#[tokio::test]
async fn test_different_seeds_produce_different_wire_sources() {
    let cdp_a = RecordingCdp::new();
    let cdp_b = RecordingCdp::new();
    let inj_a = ChromiumDeterminismInjector::new(cdp_a.clone(), make_script());
    let inj_b = ChromiumDeterminismInjector::new(cdp_b.clone(), make_script());

    inj_a.inject(1, Seed(0), EpochMs(0)).await.expect("a");
    inj_b.inject(1, Seed(42), EpochMs(0)).await.expect("b");

    let src_a = &cdp_a.recorded_inject_sources()[0];
    let src_b = &cdp_b.recorded_inject_sources()[0];
    assert_ne!(
        src_a, src_b,
        "different seeds must produce different rendered sources"
    );
}

/// J.2: when `inject` fails, `per_target_identifiers` is NOT populated, so a
/// retry runs (idempotency cache is not poisoned by failed attempts).
#[tokio::test]
async fn test_inject_failure_does_not_poison_idempotency_cache() {
    let cdp = Arc::new(FailingCdp);
    let injector = ChromiumDeterminismInjector::new(cdp, make_script());
    let r1 = injector.inject(1, Seed(42), EpochMs(0)).await;
    assert!(matches!(r1, Err(DeterminismError::CdpFailure(_))));

    // After failure, retry must also reach CDP (not short-circuit Ok).
    // We can't easily count CDP calls on FailingCdp, but we can assert
    // the second result is also an Err — if the cache had been poisoned
    // we would have got Ok(()) here.
    let r2 = injector.inject(1, Seed(42), EpochMs(0)).await;
    assert!(
        matches!(r2, Err(DeterminismError::CdpFailure(_))),
        "retry must reach CDP after failed first attempt"
    );
}
