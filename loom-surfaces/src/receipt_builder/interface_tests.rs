// Interface tests for `ReceiptBuilder`. Verifies tier discipline,
// integer-only fields, and the capture-mode boundary (surface emits at
// default tier; SessionExecutor post-processes).

extern crate alloc;

use super::receipt_builder::{
    ContentRef, NetworkEvent, Receipt, ReceiptBuilder, ReceiptInputs, ReceiptStatus, VerbKind,
};
use crate::error_mapper::error_mapper::LoomErrorCode;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

fn cref(hex: &str, size: u64) -> ContentRef {
    ContentRef {
        sha256_hex: hex.to_string(),
        size_bytes: size,
    }
}

fn fixture_inputs() -> ReceiptInputs {
    ReceiptInputs {
        action_id: "act_01".to_string(),
        timing_ticks: 1_500,
        dom_before_ref: Some(cref("aa", 100)),
        dom_after_ref: Some(cref("bb", 200)),
        screenshot_before_ref: Some(cref("cc", 50)),
        screenshot_after_ref: Some(cref("dd", 75)),
        network_events: Vec::new(),
        console_lines: Vec::new(),
        evaluate_return_value: None,
        tags: BTreeMap::new(),
    }
}

// === Full-blob tier (navigate, snapshot) ===

#[test]
fn navigate_uses_full_blob_tier_carrying_dom_and_screenshot() {
    let r = ReceiptBuilder::build_full_blob_receipt(VerbKind::Navigate, fixture_inputs());
    assert_eq!(r.verb, VerbKind::Navigate);
    assert_eq!(r.status, ReceiptStatus::Ok);
    assert!(
        r.dom_after_ref.is_some(),
        "navigate must carry dom_after_ref"
    );
    assert!(
        r.screenshot_after_ref.is_some(),
        "navigate must carry screenshot_after_ref"
    );
    assert_eq!(r.timing_ticks, 1_500);
}

#[test]
fn snapshot_uses_full_blob_tier() {
    let r = ReceiptBuilder::build_full_blob_receipt(VerbKind::Snapshot, fixture_inputs());
    assert_eq!(r.verb, VerbKind::Snapshot);
    assert!(r.dom_after_ref.is_some());
    assert!(r.screenshot_after_ref.is_some());
}

// === Hash-only tier (click/type/hover/select/scroll/wait) ===

#[test]
fn click_hash_only_tier_carries_refs_no_evaluate_return() {
    let r = ReceiptBuilder::build_hash_only_receipt(VerbKind::Click, fixture_inputs());
    assert_eq!(r.verb, VerbKind::Click);
    assert!(r.dom_after_ref.is_some(), "hash-only still carries the ref");
    assert!(r.evaluate_return_value.is_none());
}

#[test]
fn each_hash_only_verb_accepts_the_tier() {
    for v in [
        VerbKind::Click,
        VerbKind::TypeText,
        VerbKind::Hover,
        VerbKind::Select,
        VerbKind::Scroll,
        VerbKind::Wait,
    ] {
        let r = ReceiptBuilder::build_hash_only_receipt(v, fixture_inputs());
        assert_eq!(r.verb, v);
    }
}

// === Screenshot-only tier ===

#[test]
fn screenshot_tier_zeros_out_dom_and_network() {
    let r = ReceiptBuilder::build_screenshot_only_receipt(fixture_inputs());
    assert_eq!(r.verb, VerbKind::Screenshot);
    assert!(r.dom_before_ref.is_none(), "screenshot tier has no DOM");
    assert!(r.dom_after_ref.is_none(), "screenshot tier has no DOM");
    assert!(r.screenshot_after_ref.is_some());
    assert!(r.network_events.is_empty());
    assert!(r.console_lines.is_empty());
}

// === Console-only tier (evaluate) ===

#[test]
fn evaluate_tier_carries_console_and_return_value_only() {
    let mut inputs = fixture_inputs();
    inputs.evaluate_return_value = Some("42".to_string());
    inputs.console_lines = vec![super::receipt_builder::ConsoleLine {
        level: "log".to_string(),
        message: "hi".to_string(),
        timing_ticks: 100,
    }];
    let r = ReceiptBuilder::build_console_only_receipt(inputs);
    assert_eq!(r.verb, VerbKind::Evaluate);
    assert!(r.dom_after_ref.is_none());
    assert!(r.screenshot_after_ref.is_none());
    assert!(r.network_events.is_empty());
    assert_eq!(r.evaluate_return_value.as_deref(), Some("42"));
    assert_eq!(r.console_lines.len(), 1);
}

// === Error receipts ===

#[test]
fn error_receipt_has_status_error_and_error_code_set() {
    let r = ReceiptBuilder::build_error_receipt(
        VerbKind::Click,
        "act_02".to_string(),
        20,
        LoomErrorCode::WebActionTimeout,
        Some("css selector .foo timed out at 1000ms".to_string()),
        BTreeMap::new(),
    );
    assert_eq!(r.status, ReceiptStatus::Error);
    assert_eq!(r.error_code, Some(LoomErrorCode::WebActionTimeout));
    assert!(r.dom_after_ref.is_none());
    assert!(r.network_events.is_empty());
}

// === Integer-only fields (no f32/f64 anywhere) ===

#[test]
fn timing_ticks_field_is_u64_integer() {
    let r = ReceiptBuilder::build_full_blob_receipt(VerbKind::Navigate, fixture_inputs());
    let _: u64 = r.timing_ticks;
    let _: u64 = ContentRef {
        sha256_hex: "x".to_string(),
        size_bytes: 0,
    }
    .size_bytes;
}

#[test]
fn network_event_size_and_status_are_integers() {
    let n = NetworkEvent {
        method: "GET".to_string(),
        url: "https://x".to_string(),
        status_code: 200,
        response_body_sha256_hex: "ab".to_string(),
        response_body_size_bytes: 1024,
        response_body_ref: None,
        timing_ticks: 50,
    };
    let _: u32 = n.status_code;
    let _: u64 = n.response_body_size_bytes;
    let _: u64 = n.timing_ticks;
}

// === Tags use BTreeMap (no HashMap; canonical iteration order) ===

#[test]
fn tags_use_btreemap_for_canonical_iteration() {
    let mut tags: BTreeMap<String, String> = BTreeMap::new();
    tags.insert("zeta".to_string(), "z".to_string());
    tags.insert("alpha".to_string(), "a".to_string());
    let mut inputs = fixture_inputs();
    inputs.tags = tags;
    let r = ReceiptBuilder::build_hash_only_receipt(VerbKind::Click, inputs);
    let keys: Vec<&String> = r.tags.keys().collect();
    assert_eq!(keys[0], "alpha");
    assert_eq!(keys[1], "zeta");
}

// === Surface emits at default tier — no `--capture` knob ===

#[test]
fn receipt_builder_has_no_capture_mode_argument() {
    // Compilation evidence: the four `build_*` methods take only
    // (verb, ReceiptInputs) — no `capture_mode: CaptureMode` parameter.
    // If a future change added one, this test would fail to compile.
    let _r: Receipt = ReceiptBuilder::build_full_blob_receipt(VerbKind::Navigate, fixture_inputs());
    let _r: Receipt = ReceiptBuilder::build_hash_only_receipt(VerbKind::Click, fixture_inputs());
    let _r: Receipt = ReceiptBuilder::build_screenshot_only_receipt(fixture_inputs());
    let _r: Receipt = ReceiptBuilder::build_console_only_receipt(fixture_inputs());
}

// === ReceiptBuilder is stateless ===

#[test]
fn receipt_builder_is_zero_sized() {
    use core::mem::size_of;
    assert_eq!(size_of::<ReceiptBuilder>(), 0);
}
