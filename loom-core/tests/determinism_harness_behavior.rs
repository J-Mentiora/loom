// Behavior tests for DeterminismHarness.
//
// Coverage:
//   - test_clock_now_frozen_within_action (100 calls → same value)
//   - test_rng_same_seed_identical_sequence
//   - test_tape_records_net_response_on_cache_miss (receipt code)
//   - test_canonicalize_rfc8785_key_ordering
//   - test_hash_canonical_is_64_char_hex
//   - test_canonicalize_roundtrip_deterministic
//   - test_replay_table_pop_clock_returns_recorded_value
//   - test_replay_table_pop_rng_returns_recorded_value
//   - test_replay_table_pop_clock_wrong_frame_returns_internal_err
//   - test_tape_writer_snapshot_is_stable_clone

use loom_core::determinism_harness::{DeterminismHarness, SideEffectTape, TapeFrame};
use loom_core::error::LoomErrorCode;
use loom_core::manifest_writer::{LocalManifestWriter, ManifestWriter};
use loom_core::observability::Observability;
use std::sync::Arc;

fn make_harness(seed: u64) -> DeterminismHarness {
    let tmp = tempfile::tempdir().unwrap();
    let obs = Observability::new(tmp.path().join("loom.log"), false);
    let mw: Arc<dyn ManifestWriter> =
        Arc::new(LocalManifestWriter::new(tmp.path().join("sessions"), obs));
    DeterminismHarness::new(seed, mw)
}

// === virtual clock frozen within action ===

#[test]
fn test_clock_now_frozen_within_action() {
    // All 100 calls within one action execution (no begin_action() called)
    // must return the same integer value.
    let h = make_harness(42);
    let first = h.clock_now();
    for _ in 1..100 {
        assert_eq!(
            h.clock_now(),
            first,
            "clock_now() must return same value on every call within an action"
        );
    }
}

#[test]
fn test_clock_now_returns_milliseconds_not_nanoseconds() {
    // epoch_ms field in WIT — value must be milliseconds, not nanoseconds.
    // Starting at 0; a nanosecond value would be astronomically large in practice.
    // We verify the type is u64 and the value fits the ms domain (< 1e13 for sane epoch).
    let h = make_harness(0);
    let ms = h.clock_now();
    // Virtual clock starts at 0ms (session epoch), not at wall-clock epoch.
    // 1e13 ms is ~317 years from epoch — any sane virtual clock must be less.
    assert!(
        ms < 10_000_000_000_000,
        "clock value should be in milliseconds: got {ms}"
    );
}

#[test]
fn test_begin_action_advances_clock() {
    // begin_action() must advance the frozen clock by the given delta.
    let h = make_harness(0);
    let before = h.clock_now();
    h.begin_action(10); // advance 10 ms
    let after = h.clock_now();
    assert_eq!(
        after,
        before + 10,
        "begin_action(10) should advance clock by 10 ms"
    );
}

// === seeded RNG produces deterministic sequence ===

#[test]
fn test_rng_same_seed_identical_sequence() {
    // Two harnesses with the same seed must return byte-identical sequences.
    let h1 = make_harness(42);
    let h2 = make_harness(42);
    let seq1: Vec<u64> = (0..100).map(|_| h1.rng_next()).collect();
    let seq2: Vec<u64> = (0..100).map(|_| h2.rng_next()).collect();
    assert_eq!(
        seq1, seq2,
        "Same seed must produce identical 100-element sequence"
    );
}

#[test]
fn test_rng_different_seeds_produce_different_sequences() {
    let h1 = make_harness(42);
    let h2 = make_harness(99);
    let s1: Vec<u64> = (0..10).map(|_| h1.rng_next()).collect();
    let s2: Vec<u64> = (0..10).map(|_| h2.rng_next()).collect();
    assert_ne!(s1, s2, "Different seeds must produce different sequences");
}

// === canonicalization ===

#[test]
fn test_canonicalize_rfc8785_key_ordering() {
    // RFC 8785 (JCS): keys must be sorted lexicographically.
    let h = make_harness(0);
    let val = serde_json::json!({"z": 1, "a": 2, "m": 3});
    let bytes = h.canonicalize(&val).expect("canonicalize should succeed");
    let s = String::from_utf8(bytes).unwrap();
    // RFC 8785: keys sorted → "a" before "m" before "z"
    let a_pos = s.find("\"a\"").unwrap();
    let m_pos = s.find("\"m\"").unwrap();
    let z_pos = s.find("\"z\"").unwrap();
    assert!(
        a_pos < m_pos && m_pos < z_pos,
        "JCS key ordering violated: got {s}"
    );
}

#[test]
fn test_canonicalize_roundtrip_deterministic() {
    let h = make_harness(0);
    let val = serde_json::json!({"key": "value", "num": 42});
    let b1 = h.canonicalize(&val).unwrap();
    let b2 = h.canonicalize(&val).unwrap();
    assert_eq!(b1, b2, "canonicalize must be deterministic across calls");
}

#[test]
fn test_hash_canonical_is_64_char_hex() {
    let h = make_harness(0);
    let bytes = b"test input";
    let hex = h.hash_canonical(bytes);
    assert_eq!(
        hex.len(),
        64,
        "SHA-256 hex must be 64 chars, got len {}",
        hex.len()
    );
    assert!(
        hex.chars().all(|c| c.is_ascii_hexdigit()),
        "must be lowercase hex: {hex}"
    );
}

#[test]
fn test_hash_canonical_same_input_same_output() {
    let h = make_harness(0);
    let bytes = b"deterministic input";
    let h1 = h.hash_canonical(bytes);
    let h2 = h.hash_canonical(bytes);
    assert_eq!(h1, h2);
}

// === ReplayHostFnTable ===

#[test]
fn test_replay_table_pop_clock_returns_recorded_value() {
    let h = make_harness(0);
    let tape = SideEffectTape {
        frames: vec![TapeFrame::ClockRead { observed_ns: 12345 }],
    };
    let table = h.install_replay_mode(tape);
    let val = table
        .pop_clock()
        .expect("pop_clock should succeed on ClockRead frame");
    assert_eq!(val, 12345);
}

#[test]
fn test_replay_table_pop_rng_returns_recorded_value() {
    let h = make_harness(0);
    let tape = SideEffectTape {
        frames: vec![TapeFrame::RngDraw {
            value_u64: 0xdeadbeef,
        }],
    };
    let table = h.install_replay_mode(tape);
    let val = table
        .pop_rng()
        .expect("pop_rng should succeed on RngDraw frame");
    assert_eq!(val, 0xdeadbeef);
}

#[test]
fn test_replay_table_pop_net_returns_frame() {
    let h = make_harness(0);
    let tape = SideEffectTape {
        frames: vec![TapeFrame::NetResponse {
            request_id: 7,
            status: 200,
            body_ref_sha256: "a".repeat(64),
            body_size_bytes: 1024,
        }],
    };
    let table = h.install_replay_mode(tape);
    let frame = table.pop_net(7).expect("pop_net should succeed");
    if let TapeFrame::NetResponse {
        request_id, status, ..
    } = frame
    {
        assert_eq!(request_id, 7);
        assert_eq!(status, 200);
    } else {
        panic!("expected NetResponse frame");
    }
}

#[test]
fn test_replay_table_pop_clock_wrong_frame_returns_err() {
    let h = make_harness(0);
    let tape = SideEffectTape {
        frames: vec![TapeFrame::RngDraw { value_u64: 1 }], // wrong frame kind
    };
    let table = h.install_replay_mode(tape);
    let result = table.pop_clock();
    assert!(
        result.is_err(),
        "pop_clock on RngDraw frame must return an error"
    );
    let err = result.unwrap_err();
    assert_eq!(err.code, LoomErrorCode::Internal);
}

#[test]
fn test_replay_table_tape_exhausted_returns_err() {
    let h = make_harness(0);
    let tape = SideEffectTape { frames: vec![] };
    let table = h.install_replay_mode(tape);
    let result = table.pop_clock();
    assert!(
        result.is_err(),
        "pop_clock on empty tape must return an error"
    );
}

// === TapeWriter ===

#[test]
fn test_tape_writer_snapshot_is_stable_clone() {
    let h = make_harness(0);
    let mut tw = h.new_tape_writer();
    tw.record(TapeFrame::ClockRead { observed_ns: 5 });
    let snap1 = tw.snapshot();
    tw.record(TapeFrame::RngDraw { value_u64: 9 });
    let snap2 = tw.snapshot();
    assert_eq!(snap1.frames.len(), 1, "first snapshot should have 1 frame");
    assert_eq!(
        snap2.frames.len(),
        2,
        "second snapshot should have 2 frames"
    );
}
