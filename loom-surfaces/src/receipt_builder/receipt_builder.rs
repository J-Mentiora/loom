// ReceiptBuilder — tier-aware Receipt assembly for surface verbs.
//
// # Contract semantics
// - **Per-tier method.** One method per Receipt tier from
//   the verb-tier table (design.md §6 / contract §Receipt Tier Discipline):
//     * `build_full_blob_receipt`   → navigate, snapshot
//     * `build_hash_only_receipt`   → click, type-text, hover, select, scroll, wait
//     * `build_screenshot_only_receipt` → screenshot
//     * `build_console_only_receipt` → evaluate
// - **Integer-only fields (hard binding 3).** No `f32`/`f64`
//   anywhere in the Receipt struct. Timing is `timing_ticks: u64`.
// - **Canonical-JSON-ready.** No `HashMap`; ordered
//   `BTreeMap`/`Vec<(K,V)>` only. `serde_jcs::to_string` happens
//   host-side in `loom-host::ReceiptMarshaller`.
// - **No `--capture` awareness.** Builders always emit at
//   the verb-default tier; SessionExecutor post-processes per
//   `--capture full|minimal`.
// - **Never fails.** Pure data transformation; returns `Receipt` directly
//   (no `Result`).
// - **Never holds module-level state.** Stateless free functions on a
//   ZST struct.
//
// # Banned in this module
// - `std::time`, `std::net`, `std::fs::write`, `getrandom`, `f32`, `f64`,
//   `HashMap`, `serde_json` (all denied at crate level).

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::error_mapper::error_mapper::LoomErrorCode;

/// Content store reference (mirrors `wit/loom-surface.wit::types.content-ref`).
/// Integer-only fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentRef {
    /// Lowercase hex SHA-256, exactly 64 chars.
    pub sha256_hex: String,
    pub size_bytes: u64,
}

/// Network event recorded during a verb invocation. Hashes only — never
/// raw bodies; bodies live in `ContentStore` via blob refs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkEvent {
    pub method: String,
    pub url: String,
    pub status_code: u32,
    /// Hash of the response body (post-decompression).
    pub response_body_sha256_hex: String,
    pub response_body_size_bytes: u64,
    /// Optional blob ref for `--capture full` / navigate full-blob tier.
    pub response_body_ref: Option<ContentRef>,
    pub timing_ticks: u64,
}

/// Single console output line captured from `Runtime.consoleAPICalled`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleLine {
    /// One of: log, info, warn, error, debug.
    pub level: String,
    pub message: String,
    /// Virtual clock tick at emission time.
    pub timing_ticks: u64,
}

/// Receipt status. Either `Ok` (emitted at verb-default tier) or `Error`
/// (emitted with `error_code` populated; same shape).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiptStatus {
    Ok,
    Error,
}

/// Verb kind discriminator; mirrors the `web-surface` WIT method names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerbKind {
    Navigate,
    Click,
    TypeText,
    Select,
    Hover,
    Scroll,
    Wait,
    Evaluate,
    Screenshot,
    Snapshot,
}

/// The Receipt struct emitted via `host::receipt_emit`. Mirrors WIT
/// `types.receipt`. Integer fields only. Field order is
/// canonical-JSON-ready when serialized via `serde_jcs` host-side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub verb: VerbKind,
    pub action_id: String,
    pub status: ReceiptStatus,
    /// Populated only when `status = Error`.
    pub error_code: Option<LoomErrorCode>,
    pub error_details: Option<String>,
    /// `t_end - t_start` from two `host::clock_now` reads.
    pub timing_ticks: u64,
    /// Full DOM blob (navigate, snapshot tiers); hash-only for
    /// click/type/hover/select/scroll/wait when present.
    pub dom_before_ref: Option<ContentRef>,
    pub dom_after_ref: Option<ContentRef>,
    /// Full screenshot (navigate, snapshot, screenshot tiers); hash-only
    /// for click/type/hover/select/scroll/wait when present.
    pub screenshot_before_ref: Option<ContentRef>,
    pub screenshot_after_ref: Option<ContentRef>,
    /// Network events captured during the verb's host-fn calls.
    pub network_events: Vec<NetworkEvent>,
    /// Console lines captured from the page during this verb. Always
    /// present (even if empty) for verbs whose tier table marks
    /// "console: yes".
    pub console_lines: Vec<ConsoleLine>,
    /// Evaluate-only: serialized return value (canonical-JSON shape).
    pub evaluate_return_value: Option<String>,
    /// Action-level tags for the manifest (key/value strings only).
    pub tags: BTreeMap<String, String>,
}

/// Inputs assembled by a verb before the build call.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReceiptInputs {
    pub action_id: String,
    pub timing_ticks: u64,
    pub dom_before_ref: Option<ContentRef>,
    pub dom_after_ref: Option<ContentRef>,
    pub screenshot_before_ref: Option<ContentRef>,
    pub screenshot_after_ref: Option<ContentRef>,
    pub network_events: Vec<NetworkEvent>,
    pub console_lines: Vec<ConsoleLine>,
    pub evaluate_return_value: Option<String>,
    pub tags: BTreeMap<String, String>,
}

/// Stateless ZST builder.
pub struct ReceiptBuilder;

impl ReceiptBuilder {
    /// Tier: full DOM + full screenshot + full network events.
    /// Verbs: `navigate`, `snapshot`.
    pub fn build_full_blob_receipt(verb: VerbKind, i: ReceiptInputs) -> Receipt {
        debug_assert!(matches!(verb, VerbKind::Navigate | VerbKind::Snapshot));
        Receipt {
            verb,
            action_id: i.action_id,
            status: ReceiptStatus::Ok,
            error_code: None,
            error_details: None,
            timing_ticks: i.timing_ticks,
            dom_before_ref: i.dom_before_ref,
            dom_after_ref: i.dom_after_ref,
            screenshot_before_ref: i.screenshot_before_ref,
            screenshot_after_ref: i.screenshot_after_ref,
            network_events: i.network_events,
            console_lines: i.console_lines,
            evaluate_return_value: None,
            tags: i.tags,
        }
    }

    /// Tier: hash-only DOM + hash-only screenshot + action network events.
    /// Verbs: `click`, `type-text`, `hover`, `select`, `scroll`, `wait`.
    /// Hash-only means each `ContentRef.size_bytes > 0` and the bytes
    /// were `host::blob_put` to CAS, but only the ref (not full blob)
    /// is included; SessionExecutor may upgrade later under `--capture full`.
    pub fn build_hash_only_receipt(verb: VerbKind, i: ReceiptInputs) -> Receipt {
        debug_assert!(matches!(
            verb,
            VerbKind::Click
                | VerbKind::TypeText
                | VerbKind::Hover
                | VerbKind::Select
                | VerbKind::Scroll
                | VerbKind::Wait
        ));
        Receipt {
            verb,
            action_id: i.action_id,
            status: ReceiptStatus::Ok,
            error_code: None,
            error_details: None,
            timing_ticks: i.timing_ticks,
            dom_before_ref: i.dom_before_ref,
            dom_after_ref: i.dom_after_ref,
            screenshot_before_ref: i.screenshot_before_ref,
            screenshot_after_ref: i.screenshot_after_ref,
            network_events: i.network_events,
            console_lines: i.console_lines,
            evaluate_return_value: None,
            tags: i.tags,
        }
    }

    /// Tier: full screenshot only (no DOM, no network).
    /// Verb: `screenshot`.
    pub fn build_screenshot_only_receipt(i: ReceiptInputs) -> Receipt {
        Receipt {
            verb: VerbKind::Screenshot,
            action_id: i.action_id,
            status: ReceiptStatus::Ok,
            error_code: None,
            error_details: None,
            timing_ticks: i.timing_ticks,
            dom_before_ref: None,
            dom_after_ref: None,
            screenshot_before_ref: None,
            screenshot_after_ref: i.screenshot_after_ref,
            network_events: Vec::new(),
            console_lines: Vec::new(),
            evaluate_return_value: None,
            tags: i.tags,
        }
    }

    /// Tier: console + return value only (no DOM, no screenshot, no
    /// network). Verb: `evaluate`.
    pub fn build_console_only_receipt(i: ReceiptInputs) -> Receipt {
        Receipt {
            verb: VerbKind::Evaluate,
            action_id: i.action_id,
            status: ReceiptStatus::Ok,
            error_code: None,
            error_details: None,
            timing_ticks: i.timing_ticks,
            dom_before_ref: None,
            dom_after_ref: None,
            screenshot_before_ref: None,
            screenshot_after_ref: None,
            network_events: Vec::new(),
            console_lines: i.console_lines,
            evaluate_return_value: i.evaluate_return_value,
            tags: i.tags,
        }
    }

    /// Build an error Receipt. Same struct shape; status = Error;
    /// error_code populated. Used by every verb on host-fn `Result::Err`.
    /// All blob/network fields stay `None`/`empty` because the verb
    /// failed before completing observation.
    pub fn build_error_receipt(
        verb: VerbKind,
        action_id: String,
        timing_ticks: u64,
        error_code: LoomErrorCode,
        error_details: Option<String>,
        tags: BTreeMap<String, String>,
    ) -> Receipt {
        Receipt {
            verb,
            action_id,
            status: ReceiptStatus::Error,
            error_code: Some(error_code),
            error_details,
            timing_ticks,
            dom_before_ref: None,
            dom_after_ref: None,
            screenshot_before_ref: None,
            screenshot_after_ref: None,
            network_events: Vec::new(),
            console_lines: Vec::new(),
            evaluate_return_value: None,
            tags,
        }
    }
}
