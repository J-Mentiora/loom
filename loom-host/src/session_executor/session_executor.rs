// SessionExecutor — builds per-dispatch `wasmtime::Store<HostState>`
// and runs the surface invocation on the caller's tokio task.
//
// # Contract semantics
// - **No extra spawns inside dispatch.** The surface
//   invocation runs on the *caller's* `tokio::Handle` (cloned from
//   `SessionHandle::handle`). NO `tokio::spawn` per dispatch. NO
//   per-host-fn task spawns.
// - **Per-action `Store<HostState>`.** A fresh `Store` is constructed
//   per dispatch; `HostState` is moved in with `core` + `determinism`
//   + `mode` + receipt builder. The Store is dropped at action-complete.
// - **Trap boundary.** The wasmtime `Func::call_async`
//   result is matched: success → `ActionOutcome::Success`; trap →
//   `TrapHandler::handle_trap` → typed `LoomError::SurfaceTrap`.
// - **Receipt-marshaller spawn AFTER return.** This
//   module returns the `ActionOutcome` synchronously; the caller
//   (`WasmHost::dispatch`) hands it to `ReceiptMarshaller::queue`
//   on `session_handle.receipt_pool`.
// - **Abort propagation (loom-core).** `tokio::select!`
//   races the WASM call against `session_handle.abort_signal.notified()`.
//   On abort: the call is dropped, a typed `LoomError::SessionAborted`
//   receipt is returned. The store is armed via
//   `WasmRuntime::arm_store_preemption` BEFORE any guest code runs, so
//   `call_async` yields every epoch tick — the select stays preemptive
//   even for CPU-bound guests, and a runaway loop traps at the hard
//   guest-CPU deadline through the normal trap path.
// - **Acyclicity.** Depends on `WasmRuntime`, `ModuleLibrary`,
//   `HostFunctionTable`, `TrapHandler`, `HostObservability` — never
//   on `WasmHost` (downstream).

use crate::host_function_table::HostState;
use crate::host_observability::HostObservability;
use crate::module_library::{ModuleLibrary, SurfaceName};
use crate::receipt_marshaller::{ObservedCosts, ReceiptBuilder, ReceiptStatus};
use crate::trap_handler::TrapHandler;
use crate::wasm_runtime::WasmRuntime;
use crate::wit_type_marshaller::Mode;
use loom_core::budget_enforcer::KillReason;
use loom_core::error::LoomError;
use loom_core::manifest_writer::SessionId;
use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::runtime::Handle as TokioHandle;
use tokio::sync::Notify;
use wasmtime::component::Val;

/// One action ready to dispatch. WIT-derived; here only the fields
/// needed inside loom-host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub action_id: u64,
    pub surface: String,
    pub method: String,
    pub args_canonical_bytes: Vec<u8>,
}

/// Session handle threaded from `loom-core::SessionManager` through
/// `WasmHost::dispatch` into `SessionExecutor::run`.
#[derive(Clone)]
pub struct SessionHandle {
    pub session_id: SessionId,
    /// The session's tokio `Handle`. Surface invocation runs on this —
    /// no extra spawn.
    pub handle: TokioHandle,
    /// The session's receipt pool — separate `Handle` for the
    /// post-return receipt-marshaller spawn.
    pub receipt_pool: TokioHandle,
    /// Abort flag (loom-core::session_manager).
    pub abort_flag: Arc<AtomicBool>,
    /// Abort notify (raced against the WASM call).
    pub abort_signal: Arc<Notify>,
    /// Budget kill metadata (loom-core::session_manager). Populated by
    /// the BudgetEnforcer kill-callback BEFORE `abort_signal.notify_one()`
    /// fires. The executor's abort arm reads this on wake to disambiguate:
    ///   - `Some(BudgetExceeded { kind, observed, limit })` → emit
    ///     `ActionOutcome::Trapped { LoomError::BudgetExceeded }` so the
    ///     receipt stamps `error.kind = budget_exceeded` with
    ///     `detail.budget_kind` + `detail.elapsed_ms`.
    ///   - `None` → user-initiated abort → emit `ActionOutcome::Aborted`.
    pub kill_reason: Arc<parking_lot::Mutex<Option<KillReason>>>,
    /// Per-session determinism seed. Threaded into
    /// HostState at dispatch time and onto the shim wire via
    /// `ShimRequest::PageNavigate.seed`.
    pub seed: loom_shared::types::Seed,
    /// The session's OWN determinism harness (virtual clock + ChaCha20
    /// RNG seeded with the resolved `seed`), threaded from
    /// `Session.determinism` into `HostState.determinism` at dispatch
    /// time. Per-session — never the facade singleton — so concurrent
    /// sessions cannot interleave RNG draws or bleed clock state, and
    /// the same Header-recorded seed reproduces the same stream on
    /// record and replay.
    pub determinism: Arc<loom_core::determinism_harness::DeterminismHarness>,
    /// Per-session Unix epoch milliseconds. Substituted into the shim
    /// JS template's `Date.now` constant.
    pub epoch_ms: loom_shared::types::EpochMs,
    /// Operator's `--no-blocklist` opt-out.
    /// Threaded from `Session.no_blocklist` at dispatch time onto
    /// `HostState.no_blocklist`; consumed by `navigate_execute` to
    /// compute `blocklist_enabled = !no_blocklist` for each
    /// `ShimRequest::PageNavigate`.
    pub no_blocklist: bool,
    /// Operator's `--no-determinism` opt-out (settle-capture 4b). Threaded
    /// from `Session.no_determinism` onto `HostState.no_determinism`.
    pub no_determinism: bool,
    /// Operator's `--profile` choice (`"safe" | "standard" | "full"`).
    /// Threaded from `Session.profile` so HostState can inject
    /// `LOOM_SHIM_PROFILE` into per-session shim spawns
    /// (safe-profile path).
    pub profile: String,
    /// Session-scoped downloads directory under safe profile
    /// (`<sessions_root>/<ulid>/downloads/`). `None` for non-safe profiles.
    /// Used at shim-spawn time as `LOOM_SHIM_DOWNLOADS_DIR` so Chromium's
    /// `Browser.setDownloadBehavior(allowAndName, downloadPath=$DIR)`
    /// confines all downloads to this dir.
    pub downloads_dir: Option<std::path::PathBuf>,
}

/// What the executor returns synchronously to `WasmHost::dispatch`.
pub enum ActionOutcome {
    Success {
        builder: ReceiptBuilder,
        observed_costs: ObservedCosts,
    },
    Trapped {
        builder: ReceiptBuilder,
        loom_error: LoomError,
    },
    Aborted {
        builder: ReceiptBuilder,
    },
}

pub struct SessionExecutor {
    pub(crate) runtime: Arc<WasmRuntime>,
    pub(crate) library: Arc<ModuleLibrary>,
    pub(crate) trap_handler: Arc<TrapHandler>,
    pub(crate) obs: Arc<HostObservability>,
}

impl SessionExecutor {
    pub fn new(
        runtime: Arc<WasmRuntime>,
        library: Arc<ModuleLibrary>,
        trap_handler: Arc<TrapHandler>,
        obs: Arc<HostObservability>,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            library,
            trap_handler,
            obs,
        })
    }

    /// Run `action` on the surface from `library`. Builds a per-dispatch
    /// `Store<HostState>` and invokes the surface export. NO extra
    /// `tokio::spawn` inside this function.
    ///
    /// `linker` is the pre-built `live_linker` or `replay_linker` from
    /// `HostFunctionRegistry::linker_for(mode)`.
    pub async fn run(
        self: &Arc<Self>,
        action: Action,
        session: SessionHandle,
        mode: Mode,
        linker: &wasmtime::component::Linker<HostState>,
        host_state: HostState,
    ) -> Result<ActionOutcome, LoomError> {
        use loom_core::error::LoomErrorCode;
        let _ = mode;
        let component = self.library.get(&SurfaceName(action.surface.clone()))?;
        let engine = self.runtime.engine();
        // Clone the determinism harness handle before `host_state` moves into
        // the Store — needed to advance the virtual clock around dispatch
        // (timing invariants).
        let harness = host_state.determinism.clone();
        // Capture before host_state moves into the Store (decisions D9).
        let determinism_on = !host_state.no_determinism;
        let mut store = wasmtime::Store::new(engine, host_state);
        // Arm preemption BEFORE instantiate (component initializers run
        // guest code too): epoch yield per ticker tick keeps the abort
        // `select!` arm below live, the hard guest-CPU deadline traps
        // runaway loops, and the optional fuel budget is loaded.
        self.runtime.arm_store_preemption(&mut store)?;
        let instance = self
            .instantiate_surface(&mut store, &component, linker)
            .await?;

        let func_name = action.method.clone();
        let func = lookup_surface_export(&instance, &mut store, &action.surface, &func_name)
            .ok_or_else(|| {
                LoomError::new(
                    LoomErrorCode::Unsupported,
                    format!(
                        "surface '{}' export '{}' not found",
                        action.surface, func_name
                    ),
                )
            })?;

        // Receipt timestamps: under determinism (the default), a pure function of
        // the per-session `action_id` so two independent same-seed runs produce
        // byte-equal receipts (and a byte-equal manifest hash chain) instead of
        // folding in real wall-clock dispatch duration. With `--no-determinism`,
        // keep the real virtual clock. No shared mutable clock in the determinism
        // path → no cross-session bleed (decisions D9; council C2).
        const ACTION_DELTA_MS: u64 = loom_core::determinism_harness::DETERMINISTIC_ACTION_DELTA_MS;
        let started_ms = if determinism_on {
            action.action_id.saturating_mul(ACTION_DELTA_MS)
        } else {
            harness.clock_now()
        };
        // Wall-clock measurement of dispatch overhead — feeds begin_action in the
        // non-determinism path only.
        let dispatch_t0 = std::time::Instant::now();

        let mut builder = ReceiptBuilder {
            action_id: action.action_id,
            started_at_ms: started_ms,
            // Determinism (NFR-DET-01): keep real (volatile) response sizes in the
            // canonical receipt ONLY for a non-deterministic session; under
            // determinism they are zeroed so the replay hash chain stays
            // byte-stable. Fail-safe default is exclude — see
            // ReceiptBuilder::preserve_response_sizes.
            preserve_response_sizes: !determinism_on,
            ..Default::default()
        };

        // Typed input: WIT `record action { kind: string, payload: list<u8>, deadline-ms: u64 }`.
        let input_args: [Val; 1] = [build_action_val(&action)];

        // The function returns a single result: `result<receipt, host-error>`.
        // Wasmtime overwrites the slot, so the placeholder value here is irrelevant.
        let mut output_slot: [Val; 1] = [Val::Bool(false)];

        let abort_signal = session.abort_signal.clone();
        let kill_reason_slot = session.kill_reason.clone();
        let call_result = tokio::select! {
            result = func.call_async(&mut store, &input_args, &mut output_slot) => result,
            _ = abort_signal.notified() => {
                // Even on abort, set finished so timing_ticks remains
                // monotonically non-decreasing across actions.
                builder.finished_at_ms = if determinism_on {
                    action.action_id.saturating_add(1).saturating_mul(ACTION_DELTA_MS)
                } else {
                    let delta_ms = (dispatch_t0.elapsed().as_millis() as u64).max(1);
                    harness.begin_action(delta_ms);
                    harness.clock_now()
                };
                // Distinguish budget-kill from user-abort. Budget-kill
                // populates Session::kill_reason BEFORE notifying.
                // Budget-kill detection.
                let maybe_reason = kill_reason_slot.lock().clone();
                return match maybe_reason {
                    Some(KillReason::BudgetExceeded { kind, observed, limit }) => {
                        let kind_str = match kind {
                            loom_core::budget_enforcer::ResourceKind::Walltime => "wall_clock",
                            loom_core::budget_enforcer::ResourceKind::Network => "network",
                            loom_core::budget_enforcer::ResourceKind::DomNodes => "dom_nodes",
                            loom_core::budget_enforcer::ResourceKind::JsHeap => "js_heap",
                        };
                        let loom_error = LoomError::new(
                            LoomErrorCode::BudgetExceeded,
                            format!("budget exceeded: {kind_str} observed={observed} limit={limit}"),
                        )
                        .with_context(serde_json::json!({
                            "budget_kind": kind_str,
                            "elapsed_ms": observed,
                            "limit": limit,
                        }));
                        // Truthful receipt status: the queued builder is the
                        // action's single receipt, and ReceiptStatus defaults
                        // to Ok — without this stamp a budget-killed action
                        // would be recorded as a success.
                        builder.status = ReceiptStatus::Trapped;
                        builder.error_code = Some(format!("{:?}", loom_error.code));
                        builder.error_details = Some(loom_error.message.clone());
                        Ok(ActionOutcome::Trapped { builder, loom_error })
                    }
                    _ => {
                        // User-initiated abort: non-Ok status so the manifest
                        // receipt never reads as a completed action.
                        builder.status = ReceiptStatus::Error;
                        builder.error_code =
                            Some(format!("{:?}", LoomErrorCode::SessionAborted));
                        builder.error_details =
                            Some("session aborted before action completed".to_string());
                        Ok(ActionOutcome::Aborted { builder })
                    }
                };
            }
        };

        // Set finished: deterministic per-session value under determinism, else
        // advance the virtual clock by measured wall-clock (`.max(1)` keeps it
        // strictly-positive monotonic even for sub-ms dispatches).
        builder.finished_at_ms = if determinism_on {
            action
                .action_id
                .saturating_add(1)
                .saturating_mul(ACTION_DELTA_MS)
        } else {
            let delta_ms = (dispatch_t0.elapsed().as_millis() as u64).max(1);
            harness.begin_action(delta_ms);
            harness.clock_now()
        };

        match call_result {
            Ok(()) => match decode_typed_receipt(&output_slot[0], &mut builder) {
                Ok(()) => Ok(ActionOutcome::Success {
                    builder,
                    observed_costs: ObservedCosts::default(),
                }),
                Err(loom_error) => {
                    // Guest returned a host-error (or an undecodable shape).
                    // The queued builder is the action's single receipt —
                    // stamp the truthful status. decode_typed_receipt already
                    // staged the WIT variant name/detail on the error fields
                    // for variant errors; fill them from the LoomError only
                    // when it didn't.
                    builder.status = ReceiptStatus::Trapped;
                    if builder.error_code.is_none() {
                        builder.error_code = Some(format!("{:?}", loom_error.code));
                    }
                    if builder.error_details.is_none() {
                        builder.error_details = Some(loom_error.message.clone());
                    }
                    Ok(ActionOutcome::Trapped {
                        builder,
                        loom_error,
                    })
                }
            },
            Err(e) => {
                if let Some(&trap) = e.downcast_ref::<wasmtime::Trap>() {
                    let ctx = crate::trap_handler::TrapContext {
                        session_id: session.session_id,
                        action_id: action.action_id,
                        surface: action.surface,
                        dwp_path: None,
                    };
                    // handle_trap stamps status=Trapped + trap details on the
                    // builder; the SINGLE receipt is queued by
                    // WasmHost::dispatch after this returns (no handler-side
                    // append — exactly one ActionReceipt per action).
                    let loom_error = self.trap_handler.handle_trap(trap, ctx, &mut builder);
                    Ok(ActionOutcome::Trapped {
                        builder,
                        loom_error,
                    })
                } else {
                    Err(LoomError::new(LoomErrorCode::Internal, e.to_string()))
                }
            }
        }
    }

    /// Helper: instantiate the surface against the linker. Pure setup.
    ///
    /// Async because `wasm_runtime::WasmRuntime` builds the engine with
    /// `Config::async_support(true)` (loom-host/src/wasm_runtime/interfaces.rs).
    /// Wasmtime requires `Linker::instantiate_async` (NOT `instantiate`)
    /// when the engine is async-config'd; calling the sync variant raises
    /// `store configuration requires that *_async functions are used
    /// instead`. Same constraint applies to `Func::call_async` further down
    /// the dispatch path (already used in `run`).
    pub async fn instantiate_surface(
        &self,
        store: &mut wasmtime::Store<HostState>,
        component: &wasmtime::component::Component,
        linker: &wasmtime::component::Linker<HostState>,
    ) -> Result<wasmtime::component::Instance, LoomError> {
        linker
            .instantiate_async(store, component)
            .await
            .map_err(crate::error_mapper::wasmtime_error_to_loom_error)
    }

    /// Resolve the surface name for an action. Convenience for `WasmHost`.
    pub fn surface_for(&self, action: &Action) -> SurfaceName {
        SurfaceName(action.surface.clone())
    }
}

/// Build a `Val::Record` for the WIT `action` input
/// (`record action { kind: string, payload: list<u8>, deadline-ms: u64 }`).
/// Field order and field names MUST match the WIT exactly — wasmtime's
/// `Val::Record` lower checks both (see wasmtime-44 values.rs:443-456).
/// `deadline_ms` is hard-coded 0 today; call-deadline propagation is a
/// separate followup.
pub(crate) fn build_action_val(action: &Action) -> Val {
    let payload = Val::List(
        action
            .args_canonical_bytes
            .iter()
            .map(|b| Val::U8(*b))
            .collect(),
    );
    Val::Record(vec![
        ("kind".to_string(), Val::String(action.method.clone())),
        ("payload".to_string(), payload),
        ("deadline-ms".to_string(), Val::U64(0)),
    ])
}

/// Decode the typed `result<receipt, host-error>` returned by the WASM
/// guest into the receipt builder.
///
/// On `Ok` with a 3-field receipt: populates `builder.action_hash`,
/// `builder.outcome_hash`, `builder.emitted_at_ms` and returns
/// `Ok(())`. If any of the three expected fields is missing, returns
/// `Err(LoomError::Internal)` — the WIT contract requires all three,
/// so a missing field is a host-or-guest decoding bug, not a runtime
/// error.
///
/// On `Err(host-error)`: maps the WIT variant name to the matching
/// `LoomErrorCode` (`shim-failure → ShimFailure`,
/// `budget-exceeded → BudgetExceeded`, etc.) and stamps
/// `builder.error_code` / `builder.error_details` so the manifest
/// receipt records the failure with full fidelity.
///
/// On any other `Val` shape (`Val::Bool`, `Val::Result(Ok(None))`,
/// `Val::Result(Err(None))`, etc.): returns `Err(LoomError::Internal)`
/// — these are host-side decode bugs (wasmtime returned something we
/// don't expect), not legitimate guest-error returns. Distinguishing
/// the two is what lets operators tell "the guest is broken" from
/// "wasmtime is broken".
pub(crate) fn decode_typed_receipt(
    val: &Val,
    builder: &mut ReceiptBuilder,
) -> Result<(), LoomError> {
    use loom_core::error::LoomErrorCode;
    match val {
        Val::Result(Ok(Some(boxed))) => match boxed.as_ref() {
            Val::Record(fields) => {
                let mut have_action = false;
                let mut have_outcome = false;
                let mut have_emitted = false;
                for (name, v) in fields {
                    match (name.as_str(), v) {
                        ("action-hash", Val::String(s)) => {
                            builder.action_hash = s.clone();
                            have_action = true;
                        }
                        ("outcome-hash", Val::String(s)) => {
                            builder.outcome_hash = s.clone();
                            have_outcome = true;
                        }
                        ("emitted-at-ms", Val::U64(n)) => {
                            builder.emitted_at_ms = *n;
                            have_emitted = true;
                        }
                        // ---- Navigate tier-2 optional fields ----
                        ("url", Val::Option(opt)) => {
                            builder.navigate_url = extract_opt_string(opt);
                        }
                        ("final-url", Val::Option(opt)) => {
                            builder.navigate_final_url = extract_opt_string(opt);
                        }
                        ("title", Val::Option(opt)) => {
                            builder.navigate_title = extract_opt_string(opt);
                        }
                        ("status-code", Val::Option(opt)) => {
                            builder.navigate_status_code = extract_opt_u32(opt);
                        }
                        ("dom-snapshot-hash", Val::Option(opt)) => {
                            builder.navigate_dom_snapshot_hash = extract_opt_string(opt);
                        }
                        ("screenshot-after-hash", Val::Option(opt)) => {
                            builder.navigate_screenshot_after_hash = extract_opt_string(opt);
                        }
                        ("console-count", Val::Option(opt)) => {
                            builder.navigate_console_count = extract_opt_u64(opt);
                        }
                        ("network-count", Val::Option(opt)) => {
                            builder.navigate_network_count = extract_opt_u64(opt);
                        }
                        ("side-effects-json", Val::Option(opt)) => {
                            builder.navigate_side_effects_json = extract_opt_bytes(opt);
                        }
                        ("console-lines-json", Val::Option(opt)) => {
                            builder.navigate_console_lines_json = extract_opt_bytes(opt);
                        }
                        ("network-summary-json", Val::Option(opt)) => {
                            builder.navigate_network_summary_json = extract_opt_bytes(opt);
                        }
                        // ---- Observational network-entries side-channel ----
                        ("network-entries-json", Val::Option(opt)) => {
                            builder.navigate_network_entries_json = extract_opt_bytes(opt);
                        }
                        ("network-entries-blob-ref", Val::Option(opt)) => {
                            builder.navigate_network_entries_blob_ref =
                                extract_opt_content_ref(opt);
                        }
                        ("network-entries-truncated", Val::Option(opt)) => {
                            builder.navigate_network_entries_truncated = extract_opt_bool(opt);
                        }
                        // settle-capture: the two deterministic readiness fields
                        // are recorded on the canonical receipt (so they surface
                        // on the wire receipt and replay reproduces them). The
                        // settle-ms / network-count-at-settle diagnostics stay in
                        // the host NavigateResult only.
                        ("settle-until", Val::Option(opt)) => {
                            builder.navigate_settle_until = extract_opt_string(opt);
                        }
                        ("settle-outcome", Val::Option(opt)) => {
                            builder.navigate_settle_outcome = extract_opt_string(opt);
                        }
                        // ---- Evaluate tier optional fields ----
                        ("return-value-json", Val::Option(opt)) => {
                            builder.evaluate_return_value_json = extract_opt_string(opt);
                        }
                        ("return-value-blob-ref", Val::Option(opt)) => {
                            builder.evaluate_return_value_blob_ref = extract_opt_content_ref(opt);
                        }
                        // ---- v0.9.6 cookie tier optional fields ----
                        ("set-cookies-result", Val::Option(opt)) => {
                            builder.set_cookies_result = extract_opt_string(opt);
                        }
                        ("get-cookies-result", Val::Option(opt)) => {
                            builder.get_cookies_result = extract_opt_string(opt);
                        }
                        ("clear-cookies-result", Val::Option(opt)) => {
                            builder.clear_cookies_result = extract_opt_string(opt);
                        }
                        ("delete-cookies-result", Val::Option(opt)) => {
                            builder.delete_cookies_result = extract_opt_string(opt);
                        }
                        _ => {}
                    }
                }
                if !(have_action && have_outcome && have_emitted) {
                    return Err(LoomError::new(
                        LoomErrorCode::Internal,
                        format!(
                            "decode_typed_receipt: missing WIT receipt fields (action-hash={have_action}, outcome-hash={have_outcome}, emitted-at-ms={have_emitted})"
                        ),
                    ));
                }
                Ok(())
            }
            other => Err(LoomError::new(
                LoomErrorCode::Internal,
                format!("decode_typed_receipt: expected Val::Record for receipt, got {other:?}"),
            )),
        },
        Val::Result(Err(Some(boxed))) => match boxed.as_ref() {
            Val::Variant(name, inner) => {
                let detail = match inner.as_deref() {
                    Some(Val::String(s)) => s.clone(),
                    _ => String::new(),
                };
                builder.error_code = Some(name.clone());
                builder.error_details = Some(detail.clone());

                // A `shim-failure` whose detail is a
                // structured JSON object (currently `{kind, ...}`)
                // represents a TYPED guest-error receipt — not an RPC
                // failure. Surface it as `Ok(())` with `builder.status =
                // Error` so the marshaller emits a typed error receipt
                // rather than collapsing to an RPC-level error envelope.
                // Other variants (and unstructured shim-failure detail)
                // keep returning `Err` to preserve the existing contract
                // (decode_typed_receipt_maps_each_wit_host_error_variant).
                if name == "shim-failure" {
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&detail) {
                        if parsed.get("kind").and_then(|k| k.as_str()).is_some() {
                            builder.status = crate::receipt_marshaller::ReceiptStatus::Error;
                            if let Some(sc) = parsed.get("status_code").and_then(|n| n.as_u64()) {
                                builder.navigate_status_code = Some(sc as u32);
                            }

                            // P0: if host_impl embedded the
                            // captured network events under `_network_events`,
                            // hoist them onto navigate_side_effects_json so
                            // the marshaller's navigate path converts them
                            // into ReceiptPayload.network_events for HAR.
                            // Only re-serialize error_details when plumbing
                            // was present, to preserve byte-exact details for
                            // pre-P0 payloads (test:
                            // decode_typed_receipt_shim_failure_with_structured_detail_sets_status_error).
                            let has_plumbing = parsed
                                .as_object()
                                .map(|obj| obj.contains_key("_network_events"))
                                .unwrap_or(false);
                            if has_plumbing {
                                let mut cleaned = parsed.clone();
                                if let Some(obj) = cleaned.as_object_mut() {
                                    if let Some(events) = obj.remove("_network_events") {
                                        if let Ok(bytes) = serde_json::to_vec(&events) {
                                            builder.navigate_side_effects_json = Some(bytes);
                                        }
                                    }
                                }
                                builder.error_details = Some(
                                    serde_json::to_string(&cleaned)
                                        .unwrap_or_else(|_| detail.clone()),
                                );
                            }

                            return Ok(());
                        }
                    }
                }

                Err(LoomError::new(
                    map_host_error_variant(name),
                    format!("guest returned host-error::{name}({detail})"),
                ))
            }
            other => Err(LoomError::new(
                LoomErrorCode::Internal,
                format!(
                    "decode_typed_receipt: expected Val::Variant for host-error, got {other:?}"
                ),
            )),
        },
        Val::Result(Ok(None)) => Err(LoomError::new(
            LoomErrorCode::Internal,
            "decode_typed_receipt: Ok variant has no payload — wasmtime decode bug".to_string(),
        )),
        Val::Result(Err(None)) => Err(LoomError::new(
            LoomErrorCode::Internal,
            "decode_typed_receipt: Err variant has no payload — wasmtime decode bug".to_string(),
        )),
        other => Err(LoomError::new(
            LoomErrorCode::Internal,
            format!("decode_typed_receipt: expected Val::Result, got {other:?}"),
        )),
    }
}

/// Map a WIT `host-error` variant name (kebab-case, per
/// `wit/loom-surface.wit:30-36`) to the corresponding
/// `LoomErrorCode`. Unknown variants fall back to `SurfaceTrap` so the
/// JSON-RPC caller still gets a meaningful classification.
fn map_host_error_variant(variant: &str) -> loom_core::error::LoomErrorCode {
    use loom_core::error::LoomErrorCode;
    match variant {
        "budget-exceeded" => LoomErrorCode::BudgetExceeded,
        "vault-rejection" => LoomErrorCode::VaultRejection,
        "shim-failure" => LoomErrorCode::ShimFailure,
        "store-integrity-failed" => LoomErrorCode::StoreIntegrityFailed,
        "internal" => LoomErrorCode::Internal,
        _ => LoomErrorCode::SurfaceTrap,
    }
}

/// Extract `Option<String>` from a WIT `option<string>` Val::Option.
fn extract_opt_string(opt: &Option<Box<Val>>) -> Option<String> {
    match opt.as_deref() {
        Some(Val::String(s)) => Some(s.clone()),
        _ => None,
    }
}

/// Extract `Option<u32>` from a WIT `option<u32>` Val::Option.
fn extract_opt_u32(opt: &Option<Box<Val>>) -> Option<u32> {
    match opt.as_deref() {
        Some(Val::U32(n)) => Some(*n),
        _ => None,
    }
}

/// Extract `Option<u64>` from a WIT `option<u64>` Val::Option.
fn extract_opt_u64(opt: &Option<Box<Val>>) -> Option<u64> {
    match opt.as_deref() {
        Some(Val::U64(n)) => Some(*n),
        _ => None,
    }
}

fn extract_opt_bool(opt: &Option<Box<Val>>) -> Option<bool> {
    match opt.as_deref() {
        Some(Val::Bool(b)) => Some(*b),
        _ => None,
    }
}

/// Extract `Option<Vec<u8>>` from a WIT `option<list<u8>>` Val::Option.
fn extract_opt_bytes(opt: &Option<Box<Val>>) -> Option<Vec<u8>> {
    match opt.as_deref() {
        Some(Val::List(items)) => {
            let bytes: Vec<u8> = items
                .iter()
                .filter_map(|v| if let Val::U8(b) = v { Some(*b) } else { None })
                .collect();
            if bytes.is_empty() && items.is_empty() {
                None
            } else {
                Some(bytes)
            }
        }
        _ => None,
    }
}

/// Extract `Option<ContentRef>` from a WIT `option<content-ref>` Val::Option.
/// `content-ref` is a record `{ sha256: string, size: u64 }`; we map to
/// loom-core's host-side `ContentRef { sha256, size_bytes }`.
fn extract_opt_content_ref(opt: &Option<Box<Val>>) -> Option<loom_core::content_store::ContentRef> {
    match opt.as_deref() {
        Some(Val::Record(fields)) => {
            let mut sha = String::new();
            let mut size: u64 = 0;
            let mut have_sha = false;
            let mut have_size = false;
            for (name, v) in fields {
                match (name.as_str(), v) {
                    ("sha256", Val::String(s)) => {
                        sha = s.clone();
                        have_sha = true;
                    }
                    ("size", Val::U64(n)) => {
                        size = *n;
                        have_size = true;
                    }
                    _ => {}
                }
            }
            if have_sha && have_size {
                Some(loom_core::content_store::ContentRef {
                    sha256: sha,
                    size_bytes: size,
                })
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Look up an exported function on the surface component. The WIT
/// world is `world surface { import host; export web-surface; }`, so
/// guest exports live nested under the interface name. We try the
/// versioned form first (the canonical naming wit-bindgen emits when
/// the WIT package carries an `@version`), then unversioned, then a
/// flat top-level lookup as a last resort.
fn lookup_surface_export(
    instance: &wasmtime::component::Instance,
    store: &mut wasmtime::Store<HostState>,
    surface: &str,
    method: &str,
) -> Option<wasmtime::component::Func> {
    // Map the surface name (the WASM artifact's file stem, e.g.
    // `loom_surface_web`) to the WIT interface name the world
    // exports (e.g. `web-surface`). The loom v1 set is fixed; if a
    // future artifact name doesn't match any known surface, fall
    // through to a flat lookup so the failure path still fires.
    let iface_short = match surface {
        "loom_surface_web" => "web-surface",
        "loom_surface_shell" => "shell-surface",
        "loom_surface_fs" => "fs-surface",
        "loom_surface_api" => "api-surface",
        "loom_surface_native" => "native-surface",
        other => other,
    };

    let iface_candidates = [
        format!("loom:surface/{iface_short}@1.0.0"),
        format!("loom:surface/{iface_short}"),
    ];

    for iface_name in &iface_candidates {
        if let Some(iface_idx) = instance.get_export_index(&mut *store, None, iface_name) {
            if let Some(func_idx) = instance.get_export_index(&mut *store, Some(&iface_idx), method)
            {
                if let Some(func) = instance.get_func(&mut *store, func_idx) {
                    return Some(func);
                }
            }
        }
    }

    // Fallback for components that flatten interface exports to the
    // top level.
    instance.get_func(&mut *store, method)
}
