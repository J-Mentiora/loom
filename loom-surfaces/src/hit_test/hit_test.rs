// hit_test — resolve a CSS selector to its element's bounding-box centre
// and emit `Input.dispatchMouseEvent` at integer CSS-pixel coordinates.
// Shared by Click, Hover, Scroll.
//
// # Why this module exists
//
// Prior to this fix, `ClickVerb`, `HoverVerb`, and `ScrollVerb` all
// dispatched mouse events at literal `x: 0, y: 0` regardless of the
// selector — their docstrings claimed "the shim resolves the selector
// and returns the centre", but no such resolution ever happened. The
// click landed at page origin (0,0) and React's synthetic-event system
// never fired the form's `onSubmit`.
//
// # Sequence (pure CDP)
//
//   1. `DOM.getDocument(depth=0, pierce=false)` → root nodeId
//   2. `DOM.querySelector(root, selector)` → element nodeId
//        - `nodeId == 0` → `WebSelectorNotFound` (existing semantic)
//        - missing/malformed nodeId → `WebHitTestFailed`
//   3. `DOM.scrollIntoViewIfNeeded(nodeId)` (BEST-EFFORT — errors are
//        tolerated; the subsequent `getBoxModel` is the authoritative
//        signal)
//   4. `DOM.getBoxModel(nodeId)` → content quad `[x1,y1,x2,y2,x3,y3,x4,y4]`
//        - shim error (Chromium `-32000 "Could not compute box model"`)
//          → `WebHitTestFailed`
//        - missing field / wrong arity → `WebHitTestFailed`
//        - collapsed quad / zero-area parallelogram → `WebHitTestFailed`
//   5. centre = `((quad[0]+quad[4])/2, (quad[1]+quad[5])/2)`,
//      rounded f64 → i64 (integer wire constraint).
//
// # Wire shape decoding
//
// Responses arrive as CBOR bytes from `host::shim_call`. The chromium
// shim peer transcodes JSON-RPC responses to CBOR before returning;
// field names match the CDP wire shape. We decode with `ciborium` into
// small typed response structs.

extern crate alloc;

use alloc::string::String;
use serde::Deserialize;

use crate::cdp_message_encoder::cdp_message_encoder::{
    CdpMessage, CdpMessageEncoder, DomGetBoxModel, DomGetDocument, DomQuerySelector,
    DomScrollIntoViewIfNeeded, PageGetLayoutMetrics,
};
use crate::error_mapper::error_mapper::{HostError, ShimFailureKind};
use crate::host_bindings::host_bindings::host;

// ── CBOR response types ───────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct DomGetDocumentResp {
    root: NodeRef,
}

#[derive(Debug, Deserialize)]
struct NodeRef {
    #[serde(rename = "nodeId")]
    node_id: u64,
}

#[derive(Debug, Deserialize)]
struct DomQuerySelectorResp {
    #[serde(rename = "nodeId")]
    node_id: u64,
}

#[derive(Debug, Deserialize)]
struct DomGetBoxModelResp {
    model: BoxModelRect,
}

#[derive(Debug, Deserialize)]
struct BoxModelRect {
    /// `[x1,y1, x2,y2, x3,y3, x4,y4]` — 8 f64s, clockwise from top-left
    /// per CDP spec.
    content: alloc::vec::Vec<f64>,
}

#[derive(Debug, Deserialize)]
struct PageGetLayoutMetricsResp {
    #[serde(rename = "cssLayoutViewport")]
    css_layout_viewport: ViewportRect,
}

#[derive(Debug, Deserialize)]
struct ViewportRect {
    #[serde(rename = "clientWidth")]
    client_width: u64,
    #[serde(rename = "clientHeight")]
    client_height: u64,
}

// ── Public API ────────────────────────────────────────────────────────

/// Resolve `selector` to the integer CSS-pixel centre of its
/// bounding-box. Errors map to typed `HostError` variants per the
/// header contract. Coordinates are rounded (not truncated) to honour
/// the integer wire constraint while staying as close as possible to
/// the f64 quad Chromium reports.
pub fn resolve_centre_for_selector(selector: &str) -> Result<(i64, i64), HostError> {
    // 1. Root document nodeId.
    let doc_bytes = host::shim_call(
        "chromium",
        &CdpMessageEncoder::encode(&CdpMessage::DomGetDocument(DomGetDocument {
            depth: 0,
            pierce: false,
        })),
    )?;
    let doc: DomGetDocumentResp =
        ciborium::de::from_reader(&doc_bytes[..]).map_err(|e| HostError::Internal {
            reason: alloc::format!("hit_test: malformed DOM.getDocument response: {e}"),
        })?;
    let root_node_id = doc.root.node_id;

    // 2. Selector → nodeId.
    let qs_bytes = host::shim_call(
        "chromium",
        &CdpMessageEncoder::encode(&CdpMessage::DomQuerySelector(DomQuerySelector {
            node_id: root_node_id,
            selector: alloc::string::String::from(selector),
        })),
    )?;
    // Distinguish three failure modes:
    //   - `nodeId: 0`           → `SelectorNotFound` (semantic preserved)
    //   - missing nodeId field  → `HitTestFailed`    (malformed CDP response)
    //   - deserialize error     → `Internal`         (shape unexpected)
    let node_id = match ciborium::de::from_reader::<DomQuerySelectorResp, _>(&qs_bytes[..]) {
        Ok(r) => r.node_id,
        Err(_) => {
            // Try a raw decode to distinguish "missing field" from
            // "totally malformed bytes".
            match ciborium::de::from_reader::<ciborium::value::Value, _>(&qs_bytes[..]) {
                Ok(_) => {
                    return Err(HostError::ShimFailure {
                        kind: ShimFailureKind::HitTestFailed,
                    })
                }
                Err(e) => {
                    return Err(HostError::Internal {
                        reason: alloc::format!("hit_test: undecodable querySelector response: {e}"),
                    })
                }
            }
        }
    };
    if node_id == 0 {
        return Err(HostError::ShimFailure {
            kind: ShimFailureKind::SelectorNotFound,
        });
    }

    // 3. Best-effort scroll into view. Result is intentionally discarded
    //    — the next getBoxModel is the authoritative signal.
    let _ = host::shim_call(
        "chromium",
        &CdpMessageEncoder::encode(&CdpMessage::DomScrollIntoViewIfNeeded(
            DomScrollIntoViewIfNeeded { node_id },
        )),
    );

    // 4. Box model. A Chromium-side -32000 "Could not compute box model"
    //    on a hidden / detached element is a hit-test miss, but a
    //    shim-transport class failure (Crashed, Timeout, etc.) is NOT —
    //    those should propagate as-is so the agent can distinguish a
    //    hidden element from a dead browser. Only generic / unknown
    //    shim failures collapse to HitTestFailed.
    let bm_bytes = match host::shim_call(
        "chromium",
        &CdpMessageEncoder::encode(&CdpMessage::DomGetBoxModel(DomGetBoxModel { node_id })),
    ) {
        Ok(b) => b,
        Err(
            e @ HostError::ShimFailure {
                kind: ShimFailureKind::Timeout,
            },
        )
        | Err(
            e @ HostError::ShimFailure {
                kind: ShimFailureKind::Crashed,
            },
        )
        | Err(
            e @ HostError::ShimFailure {
                kind: ShimFailureKind::PermissionDenied { .. },
            },
        )
        | Err(e @ HostError::Internal { .. })
        | Err(e @ HostError::BudgetExceeded { .. })
        | Err(e @ HostError::VaultRejection { .. })
        | Err(e @ HostError::StoreIntegrityFailed) => return Err(e),
        Err(_) => {
            return Err(HostError::ShimFailure {
                kind: ShimFailureKind::HitTestFailed,
            })
        }
    };
    let bm: DomGetBoxModelResp =
        ciborium::de::from_reader(&bm_bytes[..]).map_err(|_| HostError::ShimFailure {
            kind: ShimFailureKind::HitTestFailed,
        })?;
    let q = &bm.model.content;
    if q.len() != 8 {
        return Err(HostError::ShimFailure {
            kind: ShimFailureKind::HitTestFailed,
        });
    }

    // 5. Reject degenerate quads, then 6. compute centre.
    centre_from_content_quad(q).ok_or(HostError::ShimFailure {
        kind: ShimFailureKind::HitTestFailed,
    })
}

/// Compute the integer CSS-pixel centre from a CDP `content` quad.
/// Returns `None` for collapsed quads or zero-area parallelograms.
///
/// Public to the crate for unit-testability; the private contract:
/// - Input is the 8-element quad `[x1,y1, x2,y2, x3,y3, x4,y4]`
///   (clockwise from top-left per CDP spec).
/// - Centre is the midpoint of either diagonal — for a parallelogram
///   (real Chromium boxes are always parallelograms; rotation produces
///   non-axis-aligned but still-parallelogram quads), the diagonals
///   bisect each other.
/// - Rounding: `f64::round()` (round-half-away-from-zero), then `as i64`.
///   This honours the integer wire constraint while staying within
///   0.5 px of the f64 centre.
pub(crate) fn centre_from_content_quad(q: &[f64]) -> Option<(i64, i64)> {
    if q.len() != 8 {
        return None;
    }
    let collapsed = q[0] == q[2]
        && q[2] == q[4]
        && q[4] == q[6]
        && q[1] == q[3]
        && q[3] == q[5]
        && q[5] == q[7];
    if collapsed {
        return None;
    }
    // Shoelace area (twice the signed area of the parallelogram).
    let area_x2 = (q[0] * q[3] + q[2] * q[5] + q[4] * q[7] + q[6] * q[1])
        - (q[1] * q[2] + q[3] * q[4] + q[5] * q[6] + q[7] * q[0]);
    if area_x2.abs() < f64::EPSILON {
        return None;
    }
    let centre_x = ((q[0] + q[4]) * 0.5).round() as i64;
    let centre_y = ((q[1] + q[5]) * 0.5).round() as i64;
    // Regression guard: returning (0, 0) for a non-origin element would
    // silently re-introduce the original web.click bug (every click
    // landed at page origin). Compiled out in release; the per-verb
    // integration tests assert exact coords end-to-end.
    debug_assert!(
        (centre_x, centre_y) != (0, 0)
            || (q[0] == 0.0 && q[1] == 0.0 && q[4] == 0.0 && q[5] == 0.0),
        "centre_from_content_quad returned (0,0) for non-origin quad {q:?} — \
         would silently re-introduce the original web.click hit-test bug"
    );
    Some((centre_x, centre_y))
}

/// Viewport centre from `Page.getLayoutMetrics`. Used by `ScrollVerb`
/// when no selector is provided (`None` = scroll the root document); we
/// dispatch the wheel event at the viewport centre rather than at page
/// origin so the behaviour is consistent with selector-based scrolling.
pub fn resolve_viewport_centre() -> Result<(i64, i64), HostError> {
    let lm_bytes = host::shim_call(
        "chromium",
        &CdpMessageEncoder::encode(&CdpMessage::PageGetLayoutMetrics(PageGetLayoutMetrics {})),
    )?;
    let lm: PageGetLayoutMetricsResp =
        ciborium::de::from_reader(&lm_bytes[..]).map_err(|e| HostError::Internal {
            reason: alloc::format!("hit_test: malformed Page.getLayoutMetrics response: {e}"),
        })?;
    Ok((
        (lm.css_layout_viewport.client_width / 2) as i64,
        (lm.css_layout_viewport.client_height / 2) as i64,
    ))
}
