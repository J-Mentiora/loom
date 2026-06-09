//! Normalize a CDP `DOM.getDocument` CBOR response into a content-stable form.
//!
//! Chromium's `DOM.getDocument` response embeds an ephemeral, per-navigation
//! `frameId` on every document node (the root document and, under `pierce:true`,
//! every inlined iframe `contentDocument`). That id is random per navigation, so
//! hashing the raw CBOR makes `dom_snapshot_hash` differ across two independent
//! captures of byte-identical content — even though the DOM structure, attributes
//! and text are identical (see `specs/2026-06-09-stable-dom-snapshot-hash`).
//!
//! This helper is the single place that strips the ephemeral identifier(s) so the
//! hash covers DOM structure + attributes + text + stable document fields
//! (`documentURL`/`baseURL`) only. It runs SHIM-side, at the CDP chokepoint every
//! DOM capture flows through (navigate STEP 5 + the generic `cdp_send` used by the
//! WASM-guest snapshot verb and the `loom-surfaces` guest verbs), so all paths get
//! content-stable bytes without touching the vendored WASM guest.
//!
//! Only `frameId` is stripped: a same-seed byte-diff showed it is the ONLY field
//! that varies across independent captures; `nodeId`/`backendNodeId` are
//! deterministic under the seed, so stripping them too would risk false collisions.
//! The denylist is a single extensible constant.

use ciborium::value::Value as CborValue;

/// CDP node keys that are ephemeral per-capture and must not enter the hash.
/// Extensible: add a key here if a future field is proven non-deterministic.
pub const EPHEMERAL_DOM_KEYS: &[&str] = &["frameId"];

/// Upper bound on the DOM CBOR we will decode. A real `DOM.getDocument` of a
/// large pierced page is a few MB; this generous ceiling stops a malformed /
/// hostile shim response from forcing an unbounded allocation (DoS guard,
/// mirrors `screenshot_decode::MAX_CDP_SCREENSHOT_BYTES`).
pub const MAX_DOM_CBOR_BYTES: usize = 256 * 1024 * 1024;

/// CBOR bytes that have passed through [`normalize_dom_cbor`]. The newtype
/// documents at the type level that ephemeral DOM identifiers have been stripped,
/// so a caller cannot accidentally hash/store un-normalized DOM bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedDomCbor(Vec<u8>);

impl NormalizedDomCbor {
    /// Borrow the normalized bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
    /// Consume and return the normalized bytes (for the content store / hash).
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

/// Recursively remove every [`EPHEMERAL_DOM_KEYS`] entry from a CBOR value.
///
/// `Vec::retain` preserves the order of the remaining map entries, which keeps
/// re-encoding byte-stable (ciborium serializes maps in insertion order).
fn strip_ephemeral_keys(value: &mut CborValue) {
    match value {
        CborValue::Map(entries) => {
            entries.retain(|(k, _)| match k {
                CborValue::Text(s) => !EPHEMERAL_DOM_KEYS.contains(&s.as_str()),
                _ => true,
            });
            for (_, v) in entries.iter_mut() {
                strip_ephemeral_keys(v);
            }
        }
        CborValue::Array(items) => {
            for item in items.iter_mut() {
                strip_ephemeral_keys(item);
            }
        }
        _ => {}
    }
}

/// Normalize a CBOR-encoded `DOM.getDocument` response into content-stable bytes
/// by recursively stripping ephemeral DOM identifiers (`frameId`).
///
/// On any anomaly — oversize input, undecodable CBOR, or a re-encode failure —
/// the input bytes are returned UNCHANGED (never corrupt or drop data) and a
/// `tracing::warn!` is emitted so the anomaly is observable rather than silent.
/// The warn is a side-channel and never affects the hash.
pub fn normalize_dom_cbor(bytes: &[u8]) -> NormalizedDomCbor {
    if bytes.len() > MAX_DOM_CBOR_BYTES {
        tracing::warn!(
            len = bytes.len(),
            max = MAX_DOM_CBOR_BYTES,
            "dom normalize: response exceeds size guard; passing through un-normalized"
        );
        return NormalizedDomCbor(bytes.to_vec());
    }
    let mut value: CborValue = match ciborium::de::from_reader(bytes) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "dom normalize: input was not valid CBOR; passing through un-normalized");
            return NormalizedDomCbor(bytes.to_vec());
        }
    };
    strip_ephemeral_keys(&mut value);
    let mut out = Vec::with_capacity(bytes.len());
    match ciborium::ser::into_writer(&value, &mut out) {
        Ok(()) => NormalizedDomCbor(out),
        Err(e) => {
            tracing::warn!(error = %e, "dom normalize: re-encode failed; passing through un-normalized");
            NormalizedDomCbor(bytes.to_vec())
        }
    }
}
