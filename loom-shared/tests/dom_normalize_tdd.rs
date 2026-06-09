//! TDD (Phase 2, Step 11) — RED tests for `loom_shared::dom_normalize::normalize_dom_cbor`.
//!
//! These assert the content-stable `dom_snapshot_hash` behavior: the ephemeral CDP
//! `frameId` must be stripped from the CBOR `DOM.getDocument` result before it is hashed
//! and stored, so two independent same-seed captures of byte-identical content produce the
//! SAME bytes (hence the same `dom_snapshot_hash`), while genuinely different DOM stays
//! different (no false collisions).
//!
//! This file fails to COMPILE until `loom-shared` exposes `dom_normalize::normalize_dom_cbor`
//! (RED). The library itself still builds — only this test target is red. Phase 3 adds the
//! module and the recursive `frameId` strip → GREEN.

use ciborium::value::Value as CborValue;

fn to_cbor(v: &CborValue) -> Vec<u8> {
    let mut b = Vec::new();
    ciborium::ser::into_writer(v, &mut b).expect("encode cbor");
    b
}

fn from_cbor(b: &[u8]) -> CborValue {
    ciborium::de::from_reader(b).expect("decode cbor")
}

fn text(s: &str) -> CborValue {
    CborValue::Text(s.to_string())
}

/// Normalize → raw bytes (unwraps the `NormalizedDomCbor` newtype).
fn norm(bytes: &[u8]) -> Vec<u8> {
    loom_shared::dom_normalize::normalize_dom_cbor(bytes).into_bytes()
}

/// Build a `{ "root": { document node } }` shape, optionally carrying a root `frameId`.
fn doc(frame_id: Option<&str>, extra_attr: &str) -> CborValue {
    let mut node: Vec<(CborValue, CborValue)> = vec![
        (text("nodeId"), CborValue::Integer(1.into())),
        (text("backendNodeId"), CborValue::Integer(1.into())),
        (text("nodeName"), text("#document")),
        (text("documentURL"), text("http://example.test/")),
        (text("baseURL"), text("http://example.test/")),
        (text("attrMarker"), text(extra_attr)),
    ];
    if let Some(fid) = frame_id {
        node.push((text("frameId"), text(fid)));
    }
    node.push((text("children"), CborValue::Array(vec![])));
    CborValue::Map(vec![(text("root"), CborValue::Map(node))])
}

/// Recursively assert NO `frameId` key remains anywhere in the value tree.
fn assert_no_frame_id(v: &CborValue) {
    match v {
        CborValue::Map(entries) => {
            for (k, val) in entries {
                assert_ne!(k, &text("frameId"), "frameId key survived normalization");
                assert_no_frame_id(val);
            }
        }
        CborValue::Array(items) => items.iter().for_each(assert_no_frame_id),
        _ => {}
    }
}

#[test]
fn strips_root_frame_id_so_two_captures_match() {
    // Two captures of byte-identical content differing ONLY in the ephemeral root frameId.
    let a = to_cbor(&doc(Some("F61402F5AAAA"), "same-content"));
    let b = to_cbor(&doc(Some("14B083E3BBBB"), "same-content"));
    assert_ne!(a, b, "precondition: raw bytes differ by frameId");

    let na = norm(&a);
    let nb = norm(&b);

    assert_eq!(
        na, nb,
        "normalized bytes must be identical once frameId is stripped"
    );
    assert_no_frame_id(&from_cbor(&na));
}

#[test]
fn different_dom_content_still_differs_no_false_collision() {
    let a = to_cbor(&doc(Some("F61402F5AAAA"), "content-A"));
    let b = to_cbor(&doc(Some("F61402F5AAAA"), "content-B"));
    let na = norm(&a);
    let nb = norm(&b);
    assert_ne!(
        na, nb,
        "genuinely different DOM must produce different normalized bytes"
    );
}

#[test]
fn strips_frame_id_recursively_in_nested_content_document() {
    // pierce:true inlines iframe contentDocument nodes, each with their OWN frameId.
    let nested = CborValue::Map(vec![
        (text("nodeName"), text("IFRAME")),
        (
            text("contentDocument"),
            CborValue::Map(vec![
                (text("nodeName"), text("#document")),
                (text("frameId"), text("CHILDFRAME-XYZ")),
                (text("children"), CborValue::Array(vec![])),
            ]),
        ),
    ]);
    let tree = CborValue::Map(vec![(
        text("root"),
        CborValue::Map(vec![
            (text("nodeName"), text("#document")),
            (text("frameId"), text("ROOTFRAME-123")),
            (text("children"), CborValue::Array(vec![nested])),
        ]),
    )]);
    let normalized = norm(&to_cbor(&tree));
    assert_no_frame_id(&from_cbor(&normalized));
}

#[test]
fn no_op_when_no_frame_id_present() {
    // fake-chromium-style fixture without frameId — bytes must round-trip unchanged in shape.
    let v = doc(None, "no-frame");
    let raw = to_cbor(&v);
    let normalized = norm(&raw);
    assert_eq!(
        from_cbor(&normalized),
        v,
        "absent frameId → content unchanged"
    );
}

#[test]
fn idempotent() {
    let raw = to_cbor(&doc(Some("F61402F5AAAA"), "x"));
    let once = norm(&raw);
    let twice = norm(&once);
    assert_eq!(once, twice, "normalization must be idempotent");
}

#[test]
fn preserves_non_frame_id_keys_and_order() {
    // Stripping must remove ONLY frameId; every content key (incl. documentURL/baseURL) stays.
    let raw = to_cbor(&doc(Some("F61402F5AAAA"), "y"));
    let normalized = from_cbor(&norm(&raw));
    if let CborValue::Map(top) = &normalized {
        let (_, root) = &top[0];
        if let CborValue::Map(node) = root {
            let keys: Vec<String> = node
                .iter()
                .filter_map(|(k, _)| match k {
                    CborValue::Text(s) => Some(s.clone()),
                    _ => None,
                })
                .collect();
            for must in [
                "nodeId",
                "backendNodeId",
                "documentURL",
                "baseURL",
                "attrMarker",
                "children",
            ] {
                assert!(
                    keys.contains(&must.to_string()),
                    "content key {must} must survive"
                );
            }
            assert!(
                !keys.contains(&"frameId".to_string()),
                "frameId must be gone"
            );
        } else {
            panic!("root node not a map");
        }
    } else {
        panic!("top not a map");
    }
}

#[test]
fn malformed_cbor_passes_through_unchanged() {
    // Never lose/corrupt data: undecodable bytes are returned verbatim (defensive).
    let garbage = vec![0xFFu8, 0x00, 0x13, 0x37, 0xAB];
    let out = norm(&garbage);
    assert_eq!(out, garbage, "malformed CBOR must pass through unchanged");
}
