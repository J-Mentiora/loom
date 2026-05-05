// Interface tests for `hit_test`. The public function
// `resolve_centre_for_selector` performs four host-fn calls in sequence
// and is exercised end-to-end by the per-verb interface tests at
// `loom-surfaces/src/{click,hover,scroll}_verb/interface_tests.rs`,
// which install a fixture box via `mock_host::install_hit_test_box`,
// run the verb, and assert the dispatched `Input.dispatchMouseEvent`
// coordinates landed at the box centre.
//
// The pure centre-computation helper `centre_from_content_quad` is unit-
// testable in isolation; this file pins its semantics around CDP quad
// shape, rounding, and degenerate-rejection.

use super::hit_test::centre_from_content_quad;

// === Happy path: axis-aligned box at non-origin ===

#[test]
fn centre_of_axis_aligned_box_is_midpoint() {
    // Box from (300, 200) to (420, 240). Quad is clockwise from top-left.
    let quad = [
        300.0, 200.0, // top-left
        420.0, 200.0, // top-right
        420.0, 240.0, // bottom-right
        300.0, 240.0, // bottom-left
    ];
    assert_eq!(centre_from_content_quad(&quad), Some((360, 220)));
}

#[test]
fn centre_of_box_at_origin_is_origin() {
    // Boxes that ARE at (0,0) legitimately — the debug_assert in
    // resolve_centre_for_selector permits (0,0) only for this case.
    let quad = [0.0, 0.0, 10.0, 0.0, 10.0, 10.0, 0.0, 10.0];
    assert_eq!(centre_from_content_quad(&quad), Some((5, 5)));
}

#[test]
fn centre_rounds_half_away_from_zero() {
    // Box from (0, 0) to (3, 5). f64 midpoint is (1.5, 2.5).
    // f64::round() rounds half away from zero, so → (2, 3).
    let quad = [0.0, 0.0, 3.0, 0.0, 3.0, 5.0, 0.0, 5.0];
    assert_eq!(centre_from_content_quad(&quad), Some((2, 3)));
}

#[test]
fn centre_handles_negative_coordinates() {
    // Box that's partially off-screen (legitimate; the daemon may have
    // scrolled the page). f64 midpoint is (-5, -10).
    let quad = [-10.0, -20.0, 0.0, -20.0, 0.0, 0.0, -10.0, 0.0];
    assert_eq!(centre_from_content_quad(&quad), Some((-5, -10)));
}

// === Wrong arity ===

#[test]
fn wrong_arity_quad_is_rejected() {
    // 4-element quad (two corners) — not a valid CDP content array.
    assert_eq!(centre_from_content_quad(&[0.0, 0.0, 100.0, 100.0]), None);
}

#[test]
fn empty_quad_is_rejected() {
    assert_eq!(centre_from_content_quad(&[]), None);
}

#[test]
fn nine_element_quad_is_rejected() {
    let q = [0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0, 9.9];
    assert_eq!(centre_from_content_quad(&q), None);
}

// === Degenerate cases (HitTestFailed) ===

#[test]
fn collapsed_quad_all_corners_at_same_point_is_rejected() {
    // display:none / visibility:hidden / detached elements often present
    // as a quad with all four corners coinciding.
    let quad = [10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0];
    assert_eq!(centre_from_content_quad(&quad), None);
}

#[test]
fn zero_height_parallelogram_is_rejected() {
    // Width 100, height 0 → area = 0 → reject.
    let quad = [0.0, 50.0, 100.0, 50.0, 100.0, 50.0, 0.0, 50.0];
    assert_eq!(centre_from_content_quad(&quad), None);
}

#[test]
fn zero_width_parallelogram_is_rejected() {
    // Width 0, height 50 → area = 0 → reject.
    let quad = [50.0, 0.0, 50.0, 0.0, 50.0, 50.0, 50.0, 50.0];
    assert_eq!(centre_from_content_quad(&quad), None);
}

// === Rotated (non-axis-aligned) parallelogram ===

#[test]
fn rotated_parallelogram_centre_is_diagonal_midpoint() {
    // 45°-rotated square with side √2 around centre (10, 10).
    // Corners: (10, 9), (11, 10), (10, 11), (9, 10).
    let quad = [10.0, 9.0, 11.0, 10.0, 10.0, 11.0, 9.0, 10.0];
    assert_eq!(centre_from_content_quad(&quad), Some((10, 10)));
}

// === Regression guard: this function MUST NEVER return Some((0,0))
// === for a non-origin element. The original web.click bug was
// === exactly this — silent (0,0) clicks. The debug_assert in the
// === public function catches it; this test pins the same invariant.

#[test]
fn no_quad_above_origin_returns_centre_zero_zero() {
    // Box entirely in the positive x/y plane — should never produce (0,0).
    let quad = [100.0, 100.0, 200.0, 100.0, 200.0, 200.0, 100.0, 200.0];
    let centre = centre_from_content_quad(&quad).unwrap();
    assert_ne!(
        centre,
        (0, 0),
        "hit_test must never return (0,0) for a non-origin box"
    );
    assert_eq!(centre, (150, 150));
}
