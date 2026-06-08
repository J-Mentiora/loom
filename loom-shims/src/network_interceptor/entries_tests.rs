// Tests for the NEW full-capture network-entries path (xhr/fetch/subresource +
// document), distinct from the Document-only `parse_network_event` path that feeds
// the hashed receipt's status_code derivation. The accumulator correlates CDP
// events by `request_id` and produces a raw `Vec<LoomNetworkEntry>` for the
// observational (non-hashed) `network_entries` side-channel.
//
// These reference `NetworkEntryAccumulator` + `LoomNetworkEntry`, which Phase 3
// will introduce in `network_interceptor.rs`. Until then this file is RED
// (compile failure) — that is the intended TDD red signal.

use super::network_interceptor::{LoomNetworkEntry, NetworkEntryAccumulator};
use ciborium::value::Value;

fn request_will_be_sent(request_id: &str, url: &str, method: &str, rtype: &str) -> Value {
    Value::Map(vec![
        (
            Value::Text("requestId".into()),
            Value::Text(request_id.into()),
        ),
        (Value::Text("type".into()), Value::Text(rtype.into())),
        (
            Value::Text("wallTime".into()),
            Value::Float(1_700_000_000.0),
        ),
        (
            Value::Text("request".into()),
            Value::Map(vec![
                (Value::Text("url".into()), Value::Text(url.into())),
                (Value::Text("method".into()), Value::Text(method.into())),
            ]),
        ),
    ])
}

fn response_received(request_id: &str, rtype: &str, status: i64, from_disk_cache: bool) -> Value {
    Value::Map(vec![
        (
            Value::Text("requestId".into()),
            Value::Text(request_id.into()),
        ),
        (Value::Text("type".into()), Value::Text(rtype.into())),
        (
            Value::Text("response".into()),
            Value::Map(vec![
                (Value::Text("status".into()), Value::Integer(status.into())),
                (
                    Value::Text("fromDiskCache".into()),
                    Value::Bool(from_disk_cache),
                ),
            ]),
        ),
    ])
}

// === AC: a document + N API calls → one entry per request with method/status/type ===

#[test]
fn xhr_request_correlates_into_complete_entry() {
    let acc = NetworkEntryAccumulator::new();
    acc.observe(
        "Network.requestWillBeSent",
        &request_will_be_sent("R-7", "https://app.test/api/thing", "GET", "XHR"),
    );
    acc.observe(
        "Network.responseReceived",
        &response_received("R-7", "XHR", 200, false),
    );
    let entries = acc.snapshot();
    assert_eq!(entries.len(), 1, "one xhr request → one entry");
    let e: &LoomNetworkEntry = &entries[0];
    assert_eq!(e.url, "https://app.test/api/thing");
    assert_eq!(e.method, "GET");
    assert_eq!(e.status, 200);
    assert_eq!(e.resource_type, "XHR");
    assert_eq!(e.request_id, "R-7");
    assert!(!e.from_cache);
    assert!(e.ts_ms >= 1_700_000_000_000, "ts_ms is wallTime in ms");
}

// === The new path must NOT filter to type==Document (unlike parse_network_event) ===

#[test]
fn non_document_events_are_captured_not_dropped() {
    let acc = NetworkEntryAccumulator::new();
    acc.observe(
        "Network.requestWillBeSent",
        &request_will_be_sent("R-1", "https://app.test/style.css", "GET", "Stylesheet"),
    );
    acc.observe(
        "Network.requestWillBeSent",
        &request_will_be_sent("R-2", "https://app.test/api/data", "POST", "Fetch"),
    );
    let entries = acc.snapshot();
    assert_eq!(entries.len(), 2, "stylesheet AND fetch both captured");
    assert_eq!(entries[1].method, "POST");
    assert_eq!(entries[1].resource_type, "Fetch");
}

// === from_cache via requestServedFromCache (no responseReceived) ===

#[test]
fn request_served_from_cache_sets_from_cache_flag() {
    let acc = NetworkEntryAccumulator::new();
    acc.observe(
        "Network.requestWillBeSent",
        &request_will_be_sent("R-3", "https://app.test/logo.png", "GET", "Image"),
    );
    acc.observe(
        "Network.requestServedFromCache",
        &Value::Map(vec![(
            Value::Text("requestId".into()),
            Value::Text("R-3".into()),
        )]),
    );
    let entries = acc.snapshot();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].from_cache, "memory-cache hit → from_cache=true");
}

#[test]
fn from_disk_cache_response_sets_from_cache_flag() {
    let acc = NetworkEntryAccumulator::new();
    acc.observe(
        "Network.requestWillBeSent",
        &request_will_be_sent("R-4", "https://app.test/app.js", "GET", "Script"),
    );
    acc.observe(
        "Network.responseReceived",
        &response_received("R-4", "Script", 200, true),
    );
    assert!(acc.snapshot()[0].from_cache);
}

// === D-REDIR: per-hop entries; redirect hops share request_id ===

#[test]
fn redirect_emits_one_entry_per_hop_sharing_request_id() {
    let acc = NetworkEntryAccumulator::new();
    // CDP delivers each redirect as a fresh requestWillBeSent on the same requestId.
    acc.observe(
        "Network.requestWillBeSent",
        &request_will_be_sent("R-5", "https://app.test/api/thing", "GET", "XHR"),
    );
    acc.observe(
        "Network.requestWillBeSent",
        &request_will_be_sent("R-5", "https://app.test/api/thing/", "GET", "XHR"),
    );
    acc.observe(
        "Network.responseReceived",
        &response_received("R-5", "XHR", 200, false),
    );
    let entries = acc.snapshot();
    assert_eq!(entries.len(), 2, "two hops → two entries (D-REDIR)");
    assert!(
        entries.iter().all(|e| e.request_id == "R-5"),
        "hops share request_id"
    );
    assert_eq!(entries[0].url, "https://app.test/api/thing");
    assert_eq!(entries[1].url, "https://app.test/api/thing/");
}

// === loadingFailed keeps the entry with status 0 ===

#[test]
fn loading_failed_keeps_entry_with_zero_status() {
    let acc = NetworkEntryAccumulator::new();
    acc.observe(
        "Network.requestWillBeSent",
        &request_will_be_sent("R-6", "https://broken.test/x", "GET", "Fetch"),
    );
    acc.observe(
        "Network.loadingFailed",
        &Value::Map(vec![
            (Value::Text("requestId".into()), Value::Text("R-6".into())),
            (
                Value::Text("errorText".into()),
                Value::Text("net::ERR_NAME_NOT_RESOLVED".into()),
            ),
        ]),
    );
    let entries = acc.snapshot();
    assert_eq!(
        entries.len(),
        1,
        "failed request is still in the complete list"
    );
    assert_eq!(entries[0].status, 0);
}

// === Cardinality cap: keep first-N, flag truncation ===

#[test]
fn cap_truncates_to_first_n_and_flags() {
    let acc = NetworkEntryAccumulator::with_cap(2);
    for i in 0..5 {
        acc.observe(
            "Network.requestWillBeSent",
            &request_will_be_sent(
                &format!("R-{i}"),
                &format!("https://app.test/{i}"),
                "GET",
                "Fetch",
            ),
        );
    }
    let entries = acc.snapshot();
    assert_eq!(entries.len(), 2, "capped at 2");
    assert!(acc.truncated(), "truncation flagged");
    assert_eq!(
        entries[0].request_id, "R-0",
        "keep first-N (insertion order)"
    );
}

// === Ordering: first-observed order is preserved ===

#[test]
fn entries_preserve_first_observed_order() {
    let acc = NetworkEntryAccumulator::new();
    for id in ["A", "B", "C"] {
        acc.observe(
            "Network.requestWillBeSent",
            &request_will_be_sent(id, &format!("https://app.test/{id}"), "GET", "Fetch"),
        );
    }
    let ids: Vec<String> = acc.snapshot().into_iter().map(|e| e.request_id).collect();
    assert_eq!(ids, vec!["A", "B", "C"]);
}
