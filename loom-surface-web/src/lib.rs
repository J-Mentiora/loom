// loom-surface-web — WASM component (cdylib) for the wasm32-wasip2 target.
//
// This crate produces `loom_surface_web.wasm` via:
//   cargo build --target wasm32-wasip2 -p loom-surface-web
//
// Current dispatch: navigate uses the typed `navigate-execute` host
// function; all other verbs forward via `shim_call`. Full typed dispatch
// for the remaining verbs is followup work.
//
// Note: WIT-binding items are gated on `target_arch = "wasm32"` because
// `wit_bindgen::generate!()` and the `export!()` macro emit symbols that
// only link on wasm32. The `hex_sha256` helper and its unit tests are
// target-agnostic so `cargo test -p loom-surface-web` exercises them on
// the host target — SHA-256 output must match Python's hashlib.sha256
// byte-for-byte, be 64 lowercase hex chars, and be deterministic across
// calls.

#[cfg(target_arch = "wasm32")]
wit_bindgen::generate!({
    world: "surface",
    path: "../wit/loom-surface.wit",
});

#[cfg(target_arch = "wasm32")]
struct SurfaceWebImpl;

#[cfg(target_arch = "wasm32")]
use exports::loom::surface::web_surface::{Action, HostError, Receipt};
#[cfg(target_arch = "wasm32")]
use loom::surface::host;

/// Navigate verb: calls the typed `navigate-execute` host function and
/// populates all navigate tier-2 receipt fields.
/// The action payload bytes are interpreted as a UTF-8 URL string.
#[cfg(target_arch = "wasm32")]
fn navigate_verb(a: Action) -> Result<Receipt, HostError> {
    // Decode the payload. settle-capture encodes navigate payloads as
    // `<until>\n<url>` (the daemon guarantees the prefix; URLs never contain a
    // newline). Split once: the first line is the readiness mode, the rest is
    // the URL. A payload with no newline is treated as a bare URL (settled).
    let payload_str = String::from_utf8(a.payload.clone())
        .map_err(|e| HostError::Internal(format!("navigate: payload not valid UTF-8: {e}")))?;
    let (until, url) = match payload_str.split_once('\n') {
        Some((mode, url)) => (mode.to_string(), url.to_string()),
        None => ("settled".to_string(), payload_str),
    };

    // action_id == action_hash at this layer (Q5 plumbing). The host
    // uses it for receipt correlation; the shim never sees it.
    let action_hash = hex_sha256(&a.payload);

    // Host's `navigate_execute` is now the sole HTTP 4xx/5xx + transport-
    // error gate. It returns Err(HostError::ShimFailure) for those cases
    // — which we propagate via `?`. The guest never sees a
    // `result.status_code >= 400` value: the host short-circuits first.
    let result = host::navigate_execute(&action_hash, &url, &until, a.deadline_ms)?;

    // Deterministic outcome hash: digest of dom + screenshot hashes concatenated.
    // settle-capture: the settle fields (until/outcome/ms/network-count) are
    // DELIBERATELY excluded from outcome_hash — they are virtual-time-derived
    // diagnostics, like the screenshot hash is excluded from the replay chain.
    let outcome_input = format!(
        "{}{}",
        result.dom_snapshot_hash, result.screenshot_after_hash
    );
    let outcome_hash = hex_sha256(outcome_input.as_bytes());

    Ok(Receipt {
        action_hash,
        outcome_hash,
        emitted_at_ms: result.emitted_at_ms,
        url: Some(result.url),
        final_url: Some(result.final_url),
        title: Some(result.title),
        status_code: Some(result.status_code),
        dom_snapshot_hash: Some(result.dom_snapshot_hash),
        screenshot_after_hash: Some(result.screenshot_after_hash),
        console_count: Some(result.console_count),
        network_count: Some(result.network_count),
        side_effects_json: Some(result.side_effects_json),
        console_lines_json: Some(result.console_lines_json),
        network_summary_json: Some(result.network_summary_json),
        return_value_json: None,
        return_value_blob_ref: None,
        set_cookies_result: None,
        get_cookies_result: None,
        clear_cookies_result: None,
        delete_cookies_result: None,
        settle_until: Some(result.settle_until),
        settle_outcome: Some(result.settle_outcome),
        settle_ms: Some(result.settle_ms),
        network_count_at_settle: Some(result.network_count_at_settle),
    })
}

/// Evaluate verb: calls the typed `evaluate-execute` host function and
/// populates the evaluate-tier receipt fields.
/// The action payload bytes are interpreted as a UTF-8 JS expression.
#[cfg(target_arch = "wasm32")]
fn evaluate_verb(a: Action) -> Result<Receipt, HostError> {
    let expression = String::from_utf8(a.payload.clone()).map_err(|e| {
        HostError::Internal(format!("evaluate: payload not valid UTF-8 expression: {e}"))
    })?;

    // action_id == action_hash at this layer (Q5).
    let action_hash = hex_sha256(&a.payload);

    // Host's `evaluate_execute` handles CDP Runtime.evaluate dispatch,
    // exceptionDetails (→ HostError::ShimFailure with kind=js_throw),
    // CBOR→canonical-JSON conversion, and 64KB content-store offload.
    let result = host::evaluate_execute(&action_hash, &expression, a.deadline_ms)?;

    // Q7: byte-stable outcome_hash with domain separator.
    //   E:    inline canonical-JSON path
    //   E:T:  truncated path (hash of blob ref's sha256)
    let outcome_hash = if let Some(json) = &result.return_value_json {
        let mut buf = Vec::with_capacity(2 + json.len());
        buf.extend_from_slice(b"E:");
        buf.extend_from_slice(json.as_bytes());
        hex_sha256(&buf)
    } else if let Some(blob_ref) = &result.return_value_blob_ref {
        let mut buf = Vec::with_capacity(4 + blob_ref.sha256.len());
        buf.extend_from_slice(b"E:T:");
        buf.extend_from_slice(blob_ref.sha256.as_bytes());
        hex_sha256(&buf)
    } else {
        return Err(HostError::Internal(
            "evaluate: result has neither json nor blob_ref".into(),
        ));
    };

    Ok(Receipt {
        action_hash,
        outcome_hash,
        emitted_at_ms: result.emitted_at_ms,
        url: None,
        final_url: None,
        title: None,
        status_code: None,
        dom_snapshot_hash: None,
        screenshot_after_hash: None,
        console_count: None,
        network_count: None,
        side_effects_json: None,
        console_lines_json: None,
        network_summary_json: None,
        return_value_json: result.return_value_json,
        return_value_blob_ref: result.return_value_blob_ref,
        set_cookies_result: None,
        get_cookies_result: None,
        clear_cookies_result: None,
        delete_cookies_result: None,
        settle_until: None,
        settle_outcome: None,
        settle_ms: None,
        network_count_at_settle: None,
    })
}

/// Set-input-files verb: calls the typed `set_input_files_execute` host
/// function (mirrors navigate/evaluate). The guest forwards `a.payload` (the
/// daemon-built canonical JSON `{selector,paths}`) verbatim — it has no JSON
/// parser; the host parses + runs the CDP sequence + already-validated paths.
/// No screenshot is captured (matches click/type — D-SCREEN-1); the tamper
/// chain rides on action_hash + a file-count-derived outcome_hash.
#[cfg(target_arch = "wasm32")]
fn set_input_files_verb(a: Action) -> Result<Receipt, HostError> {
    // action_id == action_hash at this layer (Q5); covers selector + canonical paths.
    let action_hash = hex_sha256(&a.payload);
    let result = host::set_input_files_execute(&action_hash, &a.payload, a.deadline_ms)?;

    // Deterministic outcome hash from the file count + action hash.
    let outcome_hash = hex_sha256(format!("S:{}:{}", result.file_count, action_hash).as_bytes());
    let now = host::clock_now();

    Ok(Receipt {
        action_hash,
        outcome_hash,
        emitted_at_ms: now.epoch_ms,
        url: None,
        final_url: None,
        title: None,
        status_code: None,
        dom_snapshot_hash: None,
        screenshot_after_hash: None,
        console_count: None,
        network_count: None,
        side_effects_json: None,
        console_lines_json: None,
        network_summary_json: None,
        return_value_json: None,
        return_value_blob_ref: None,
        set_cookies_result: None,
        get_cookies_result: None,
        clear_cookies_result: None,
        delete_cookies_result: None,
        settle_until: None,
        settle_outcome: None,
        settle_ms: None,
        network_count_at_settle: None,
    })
}

/// Wait-for verb (settle-capture slice 2): a standalone readiness wait on the
/// current page. Calls the typed `wait-for-execute` host function and builds a
/// receipt carrying ONLY the settle verdict (no DOM / screenshot — wait_for
/// captures nothing). The action payload bytes are the readiness mode string
/// (`load|networkidle|settled`); an empty payload defaults to `settled`.
#[cfg(target_arch = "wasm32")]
fn wait_for_verb(a: Action) -> Result<Receipt, HostError> {
    let until = String::from_utf8(a.payload.clone())
        .map_err(|e| HostError::Internal(format!("wait_for: payload not valid UTF-8: {e}")))?;
    let until = if until.is_empty() {
        "settled".to_string()
    } else {
        until
    };

    // action_id == action_hash at this layer (Q5 plumbing).
    let action_hash = hex_sha256(&a.payload);

    let result = host::wait_for_execute(&action_hash, &until, a.deadline_ms)?;

    // Replay-stable outcome hash: derived from the settle VERDICT only
    // (settle_until + settle_outcome are pure functions of the recorded
    // observation sequence). settle_ms / network_count_at_settle are
    // machine-variable diagnostics and are DELIBERATELY excluded — exactly
    // as navigate excludes them and as the screenshot hash is excluded from
    // the replay chain.
    let outcome_input = format!("W:{}:{}", result.settle_until, result.settle_outcome);
    let outcome_hash = hex_sha256(outcome_input.as_bytes());

    Ok(Receipt {
        action_hash,
        outcome_hash,
        emitted_at_ms: result.emitted_at_ms,
        url: None,
        final_url: None,
        title: None,
        status_code: None,
        dom_snapshot_hash: None,
        screenshot_after_hash: None,
        console_count: None,
        network_count: None,
        side_effects_json: None,
        console_lines_json: None,
        network_summary_json: None,
        return_value_json: None,
        return_value_blob_ref: None,
        set_cookies_result: None,
        get_cookies_result: None,
        clear_cookies_result: None,
        delete_cookies_result: None,
        settle_until: Some(result.settle_until),
        settle_outcome: Some(result.settle_outcome),
        settle_ms: Some(result.settle_ms),
        network_count_at_settle: Some(result.network_count_at_settle),
    })
}

/// Generic verb dispatcher for non-navigate, non-evaluate verbs. Forwards the action
/// payload to the chromium shim and builds a minimal receipt from the response.
#[cfg(target_arch = "wasm32")]
fn dispatch(verb: &str, a: Action) -> Result<Receipt, HostError> {
    host::log_emit(
        host::LogLevel::Info,
        &format!("web.{} dispatching to chromium shim", verb),
        &[],
    );
    let response = host::shim_call("chromium", &a.payload)?;

    // Selector-based verbs (click/type_text/select/hover/scroll/wait)
    // are encoded as Runtime.evaluate CBOR envelopes by the daemon's
    // build_chromium_args. When the page-side expression throws (e.g.
    // querySelector('#missing').click() raises TypeError on null),
    // CDP returns `exceptionDetails` in the response. Without checking
    // for it the receipt would falsely report status="success" — the
    // agent thinks the click landed when it didn't.
    //
    // The check is byte-pattern based to avoid pulling a CBOR decoder
    // into the wasm guest. CBOR text strings are length-prefixed; the
    // exact bytes for the "exceptionDetails" key (text-string of
    // length 16) are 0x70 followed by the UTF-8 bytes. A false positive
    // would require a payload with the literal string in some other
    // role (extremely unlikely for a CDP response). Screenshot /
    // snapshot CDP responses don't include this key.
    let is_selector_verb = matches!(
        verb,
        "click" | "type_text" | "select" | "hover" | "scroll" | "wait"
    );
    if is_selector_verb && contains_cbor_key(&response, "exceptionDetails") {
        return Err(HostError::ShimFailure(format!(
            "{{\"kind\":\"js_throw\",\"verb\":\"{verb}\",\"reason\":\"selector miss or page exception\"}}"
        )));
    }
    // wait predicate is a boolean Runtime.evaluate ("querySelector(sel) !== null").
    // CDP wraps the result as `{ result: { type: "boolean", value: <bool> } }`,
    // so a missing selector yields `value: false` without exceptionDetails.
    // Without this check, a wait for a nonexistent selector returns success
    // immediately (the shim response IS different — outcome_hash diverges —
    // but the receipt's status field never reflects the predicate verdict).
    // The guest can't perform real polling here (no host clock_now binding
    // in this dispatch path), so map a `value: false` reply to a typed
    // shim-failure that the daemon's error-mapper will surface as a
    // selector-miss / wait-timeout to the CLI.
    if verb == "wait" && contains_value_false(&response) {
        return Err(HostError::ShimFailure(
            "{\"kind\":\"wait_predicate_false\",\"verb\":\"wait\",\"reason\":\"selector did not appear before timeout\"}".into()
        ));
    }

    let now = host::clock_now();
    let response_hash = hex_sha256(&response);
    // For `screenshot`, persist the decoded PNG host-side via the typed
    // `record-screenshot` host import and surface the CONTENT-STORE hash of the
    // raw PNG as screenshot_after_hash, so an MCP/CLI consumer can resolve it to
    // a renderable `image/png`. The guest ships without a CBOR/base64 decoder by
    // design, so the host owns the unwrap+store. On a record failure we fall
    // back to the response hash so the verb still returns a receipt (graceful);
    // both live and replay take the same branch for identical bytes, so the
    // receipt stays deterministic.
    //
    // `snapshot` keeps the response-hash identifier (no content-store offload).
    let (screenshot_after_hash, dom_snapshot_hash) = match verb {
        "screenshot" => {
            // On a record failure, leave screenshot_after_hash absent rather
            // than emitting the response hash — a hash that resolves to nothing
            // in the content store is worse than no hash (the client would fetch
            // a 404). The verb still returns a successful receipt.
            let hash = match host::record_screenshot(&response) {
                Ok(r) => Some(r.sha256),
                Err(e) => {
                    host::log_emit(
                        host::LogLevel::Warn,
                        &format!("record_screenshot failed; screenshot not stored: {e:?}"),
                        &[],
                    );
                    None
                }
            };
            (hash, None)
        }
        "snapshot" => (None, Some(response_hash.clone())),
        _ => (None, None),
    };
    Ok(Receipt {
        action_hash: hex_sha256(&a.payload),
        outcome_hash: response_hash,
        emitted_at_ms: now.epoch_ms,
        url: None,
        final_url: None,
        title: None,
        status_code: None,
        dom_snapshot_hash,
        screenshot_after_hash,
        console_count: None,
        network_count: None,
        side_effects_json: None,
        console_lines_json: None,
        network_summary_json: None,
        return_value_json: None,
        return_value_blob_ref: None,
        set_cookies_result: None,
        get_cookies_result: None,
        clear_cookies_result: None,
        delete_cookies_result: None,
        settle_until: None,
        settle_outcome: None,
        settle_ms: None,
        network_count_at_settle: None,
    })
}

/// Byte-pattern scan: does `bytes` contain the CBOR-encoded text-string
/// for `key`? CBOR text-strings ≤ 23 bytes long are encoded as a single
/// type-major byte (0x60..=0x77) followed by the UTF-8. Used to detect
/// `exceptionDetails` in CDP Runtime.evaluate responses without pulling
/// a full CBOR decoder into the wasm guest.
///
/// False-positive resistance: the byte signature must include both the
/// CBOR length prefix AND the exact UTF-8 of the key, so a coincidental
/// substring in `error.message` text wouldn't match the prefix-byte
/// requirement. Tests on host-target verify this.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn contains_cbor_key(bytes: &[u8], key: &str) -> bool {
    let key_bytes = key.as_bytes();
    let len = key_bytes.len();
    if len == 0 || len > 23 {
        // Encoding for longer strings differs (multi-byte length); not
        // supported here. The CDP keys we care about are all ≤ 23 chars.
        return false;
    }
    let prefix = 0x60u8 | (len as u8);
    if bytes.len() < len + 1 {
        return false;
    }
    for i in 0..=bytes.len() - (len + 1) {
        if bytes[i] == prefix && &bytes[i + 1..i + 1 + len] == key_bytes {
            return true;
        }
    }
    false
}

/// Byte-pattern scan: does `bytes` contain a CBOR map entry mapping the
/// text-string key "value" to the boolean `false`? Equivalent to looking
/// for the 7-byte signature `[0x65, 'v', 'a', 'l', 'u', 'e', 0xf4]`.
///
/// Used by the `wait` verb to detect a `Runtime.evaluate` response of
/// `{ result: { value: false } }` — i.e. the selector did not appear —
/// without dragging a CBOR decoder into the wasm guest. Canonical CBOR
/// encodes `value` as `0x65` (text-string-len-5) + UTF-8, and `false` as
/// the simple-value byte `0xf4`. The `result` outer key is always present
/// in CDP Runtime.evaluate replies, so the inner `value: false` only
/// appears when the predicate evaluated false (or when an
/// `exceptionDetails` field is present, which the prior check already
/// short-circuits on).
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn contains_value_false(bytes: &[u8]) -> bool {
    const SIG: &[u8] = &[0x65, b'v', b'a', b'l', b'u', b'e', 0xf4];
    bytes.windows(SIG.len()).any(|w| w == SIG)
}

/// SHA-256 (lowercase hex, 64 chars) of the input bytes — produces the
/// `action_hash` and `outcome_hash` strings carried in `Receipt`.
///
/// Matches the host-side convention (`sha256_hex` /
/// `sha256_hex_of_cbor`) and is byte-equal to Python's
/// `hashlib.sha256(bytes).hexdigest()` over the same input. The daemon
/// produces canonical CBOR for every web verb's `Action.payload` via
/// `build_chromium_args` (loom-daemon/src/main.rs), so SHA-256 over
/// `a.payload` IS SHA-256 of the canonical CBOR action payload that
/// the host conventions require.
//
// dead_code on host target because navigate_verb / dispatch are
// wasm32-gated; the helper is exercised on host via the `tests` module.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn hex_sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest.iter() {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(target_arch = "wasm32")]
impl exports::loom::surface::web_surface::Guest for SurfaceWebImpl {
    fn navigate(a: Action) -> Result<Receipt, HostError> {
        navigate_verb(a)
    }
    fn click(a: Action) -> Result<Receipt, HostError> {
        dispatch("click", a)
    }
    fn type_text(a: Action) -> Result<Receipt, HostError> {
        dispatch("type_text", a)
    }
    fn select(a: Action) -> Result<Receipt, HostError> {
        dispatch("select", a)
    }
    fn hover(a: Action) -> Result<Receipt, HostError> {
        dispatch("hover", a)
    }
    fn scroll(a: Action) -> Result<Receipt, HostError> {
        dispatch("scroll", a)
    }
    fn wait(a: Action) -> Result<Receipt, HostError> {
        dispatch("wait", a)
    }
    fn wait_for(a: Action) -> Result<Receipt, HostError> {
        wait_for_verb(a)
    }
    fn evaluate(a: Action) -> Result<Receipt, HostError> {
        evaluate_verb(a)
    }
    fn set_input_files(a: Action) -> Result<Receipt, HostError> {
        set_input_files_verb(a)
    }
    fn screenshot(a: Action) -> Result<Receipt, HostError> {
        dispatch("screenshot", a)
    }
    fn snapshot(a: Action) -> Result<Receipt, HostError> {
        dispatch("snapshot", a)
    }
    // v0.9.6 web-cookie-injection. Routed through the same `dispatch`
    // path as click/type/etc. — the WIT-side action payload carries the
    // JCS-encoded Action enum, and the host's verb-dispatch table runs
    // the appropriate verb's `execute()`. Cookie verbs do not have
    // surface-side custom routing (no `navigate_verb`-style helper)
    // because they live entirely in loom-surfaces with no host-side
    // typed-helper variant.
    fn set_cookies(a: Action) -> Result<Receipt, HostError> {
        dispatch("set_cookies", a)
    }
    fn get_cookies(a: Action) -> Result<Receipt, HostError> {
        dispatch("get_cookies", a)
    }
    fn clear_cookies(a: Action) -> Result<Receipt, HostError> {
        dispatch("clear_cookies", a)
    }
    fn delete_cookies(a: Action) -> Result<Receipt, HostError> {
        dispatch("delete_cookies", a)
    }
}

#[cfg(target_arch = "wasm32")]
export!(SurfaceWebImpl);

#[cfg(test)]
mod tests {
    use super::hex_sha256;

    /// Equality with Python `hashlib.sha256`.
    /// Vector from FIPS 180-4: SHA-256("abc") =
    ///   ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
    /// Identical to `hashlib.sha256(b"abc").hexdigest()`.
    #[test]
    fn sha256_known_vector_matches_python_hashlib() {
        assert_eq!(
            hex_sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// Empty input vector.
    /// SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855.
    /// Same as `hashlib.sha256(b"").hexdigest()`.
    #[test]
    fn sha256_empty_input_matches_python_hashlib() {
        assert_eq!(
            hex_sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    /// Output is 64 lowercase hex chars across input sizes.
    #[test]
    fn sha256_output_is_64_lowercase_hex_chars() {
        for bytes in [
            b"".as_slice(),
            b"x".as_slice(),
            &[0u8; 1024],
            b"the quick brown fox jumps over the lazy dog".as_slice(),
        ] {
            let h = hex_sha256(bytes);
            assert_eq!(
                h.len(),
                64,
                "expected 64 hex chars, got {} for input len {}",
                h.len(),
                bytes.len()
            );
            assert!(
                h.chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "expected lowercase hex only, got {h}"
            );
        }
    }

    /// Deterministic across calls.
    #[test]
    fn sha256_is_deterministic_across_calls() {
        let input = b"loom-surface-web payload bytes";
        assert_eq!(hex_sha256(input), hex_sha256(input));
    }

    /// Anti-regression — distinct inputs produce distinct outputs.
    #[test]
    fn sha256_distinguishes_distinct_inputs() {
        assert_ne!(hex_sha256(b"a"), hex_sha256(b"b"));
        assert_ne!(hex_sha256(b"abc"), hex_sha256(b"abcd"));
    }

    use super::contains_cbor_key;

    /// CBOR text-string of length 16 ("exceptionDetails") starts with
    /// 0x70 (0x60 | 16). Verify the byte-pattern detector finds it.
    #[test]
    fn contains_cbor_key_finds_exception_details() {
        let mut buf = Vec::new();
        // Some leading noise to confirm full-buffer scan.
        buf.extend_from_slice(&[0x01, 0x02, 0x03]);
        buf.push(0x70); // text-string len 16
        buf.extend_from_slice(b"exceptionDetails");
        buf.extend_from_slice(b"trailing");
        assert!(contains_cbor_key(&buf, "exceptionDetails"));
    }

    /// A response that doesn't contain the key returns false. Just the
    /// UTF-8 substring without the length prefix isn't enough.
    #[test]
    fn contains_cbor_key_skips_unprefixed_substrings() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"This response mentions exceptionDetails in prose only");
        // 0x60 | 16 = 0x70 doesn't appear before "exceptionDetails" here
        // (because the leading bytes are ASCII text, not CBOR len prefix).
        assert!(
            !contains_cbor_key(&buf, "exceptionDetails"),
            "naked substring without CBOR length prefix must NOT match"
        );
    }

    /// Empty input + empty key both return false.
    #[test]
    fn contains_cbor_key_edge_cases() {
        assert!(!contains_cbor_key(&[], "exceptionDetails"));
        assert!(!contains_cbor_key(b"\x70exceptionDetails", ""));
    }

    /// Long keys (> 23 chars) aren't supported by the simple short-form
    /// encoder check; the function returns false rather than misclassify.
    #[test]
    fn contains_cbor_key_long_keys_unsupported() {
        let long = "a".repeat(30);
        let mut buf = vec![0x78, long.len() as u8];
        buf.extend_from_slice(long.as_bytes());
        assert!(!contains_cbor_key(&buf, &long));
    }

    use super::contains_value_false;

    /// Real-shape CDP Runtime.evaluate response with predicate=false:
    /// `{ result: { type: "boolean", value: false } }` encoded canonically.
    /// The `value: false` mapping appears as the 7-byte signature
    /// `[0x65, 'v', 'a', 'l', 'u', 'e', 0xf4]` and must match.
    #[test]
    fn contains_value_false_finds_predicate_false() {
        // Hand-built canonical CBOR for {"result":{"type":"boolean","value":false}}
        // a1 = map(1)
        //   66 "result"        text-string-len-6 "result"
        //   a2 = map(2)
        //     64 "type"        text-string-len-4 "type"
        //     67 "boolean"     text-string-len-7 "boolean"
        //     65 "value"       text-string-len-5 "value"
        //     f4              false
        let buf: &[u8] = &[
            0xa1, 0x66, b'r', b'e', b's', b'u', b'l', b't', 0xa2, 0x64, b't', b'y', b'p', b'e',
            0x67, b'b', b'o', b'o', b'l', b'e', b'a', b'n', 0x65, b'v', b'a', b'l', b'u', b'e',
            0xf4,
        ];
        assert!(contains_value_false(buf));
    }

    /// Same shape but with predicate=true must NOT match — that's the
    /// success path and we want to return the receipt as-is.
    #[test]
    fn contains_value_false_skips_predicate_true() {
        let buf: &[u8] = &[
            0xa1, 0x66, b'r', b'e', b's', b'u', b'l', b't', 0xa2, 0x64, b't', b'y', b'p', b'e',
            0x67, b'b', b'o', b'o', b'l', b'e', b'a', b'n', 0x65, b'v', b'a', b'l', b'u', b'e',
            0xf5, // true
        ];
        assert!(!contains_value_false(buf));
    }

    /// Empty buffer never matches.
    #[test]
    fn contains_value_false_empty() {
        assert!(!contains_value_false(&[]));
    }

    /// `value` key followed by a number, not a boolean — must not match.
    /// Common in non-wait CDP responses (e.g. element node IDs).
    #[test]
    fn contains_value_false_skips_value_with_non_bool() {
        let buf: &[u8] = &[
            0x65, b'v', b'a', b'l', b'u', b'e', 0x18, 0x42, // unsigned 8-bit int 0x42
        ];
        assert!(!contains_value_false(buf));
    }
}
