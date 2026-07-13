//! `loom blob get <hash>` retrieval contract (voice-call-io AC11).
//!
//! The captured-audio ContentRef must be fetchable to a playable `.wav`
//! without touching the internal ContentStore API: bytes are seeded into
//! the daemon's CAS on disk (same fixture approach as the export tests —
//! `content.get` is a pure CAS read, no browser needed), then retrieved
//! through the real CLI → daemon → `content.get` path.

mod common;

use common::daemon_test_harness::DaemonTestHarness;
use loom_core::content_store::{sha256_hex, shard_path};

/// A minimal valid WAV: 44-byte RIFF/WAVE header (16 kHz mono s16le)
/// followed by four PCM samples. Shaped like the capture pipeline's
/// output so the "playable .wav" assertion is meaningful.
fn wav_fixture() -> Vec<u8> {
    let samples: [i16; 4] = [0, 1000, -1000, 0];
    let data_len = (samples.len() * 2) as u32;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&16_000u32.to_le_bytes()); // sample rate
    wav.extend_from_slice(&32_000u32.to_le_bytes()); // byte rate
    wav.extend_from_slice(&2u16.to_le_bytes()); // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        wav.extend_from_slice(&s.to_le_bytes());
    }
    wav
}

/// Seed `bytes` into the daemon's CAS exactly where `LocalContentStore`
/// would put them (root = `<data_root>/cas`, shard depth 2).
fn seed_blob(data_root: &std::path::Path, bytes: &[u8]) -> String {
    let hash = sha256_hex(bytes);
    let target = shard_path(&data_root.join("cas"), &hash, 2);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, bytes).unwrap();
    hash
}

fn harness() -> (DaemonTestHarness, std::path::PathBuf) {
    let h0 = DaemonTestHarness::new();
    let data_root = h0.home().join("loom-data");
    std::fs::create_dir_all(&data_root).unwrap();
    let mut h = h0
        .env("LOOM_DATA_ROOT", &data_root)
        .env("LOOM_AUTH_DIR", data_root.join("auth"))
        .env("LOOM_REAPER_SWEEP_SECS", "300");
    h.start();
    (h, data_root)
}

#[test]
fn blob_get_writes_byte_identical_playable_wav_to_output_file() {
    let (h, data_root) = harness();
    let wav = wav_fixture();
    let hash = seed_blob(&data_root, &wav);

    let out_path = h.home().join("answer.wav");
    let out = h
        .loom_command()
        .args(["blob", "get", &hash, "-o"])
        .arg(&out_path)
        .output()
        .expect("run loom blob get");
    assert!(
        out.status.success(),
        "blob get must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let fetched = std::fs::read(&out_path).unwrap();
    assert_eq!(fetched, wav, "retrieved bytes must be byte-identical");
    // Playability shape: RIFF/WAVE magic + declared sizes parse.
    assert_eq!(&fetched[0..4], b"RIFF");
    assert_eq!(&fetched[8..12], b"WAVE");
    let riff_len = u32::from_le_bytes(fetched[4..8].try_into().unwrap());
    assert_eq!(riff_len as usize + 8, fetched.len());
}

#[test]
fn blob_get_lowercases_an_uppercase_hash_paste() {
    let (h, data_root) = harness();
    let wav = wav_fixture();
    let hash = seed_blob(&data_root, &wav);

    let out_path = h.home().join("upper.wav");
    let out = h
        .loom_command()
        .args(["blob", "get", &hash.to_ascii_uppercase(), "-o"])
        .arg(&out_path)
        .output()
        .expect("run loom blob get with uppercase hash");
    assert!(
        out.status.success(),
        "uppercase paste must be normalized; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read(&out_path).unwrap(), wav);
}

#[test]
fn blob_get_streams_to_piped_stdout_without_output_flag() {
    let (h, data_root) = harness();
    let wav = wav_fixture();
    let hash = seed_blob(&data_root, &wav);

    // .output() wires stdout to a pipe, so this exercises the no-`-o`
    // pipe branch of the output matrix (a TTY is not available here;
    // the refusal branch is unit-tested in blob_commands).
    let out = h
        .loom_command()
        .args(["blob", "get", &hash])
        .output()
        .expect("run loom blob get to stdout");
    assert!(
        out.status.success(),
        "piped stdout must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, wav, "stdout must carry the raw blob bytes");
}

#[test]
fn blob_get_overwrites_an_existing_output_file() {
    let (h, data_root) = harness();
    let wav = wav_fixture();
    let hash = seed_blob(&data_root, &wav);

    let out_path = h.home().join("existing.wav");
    std::fs::write(&out_path, b"stale previous contents").unwrap();
    let out = h
        .loom_command()
        .args(["blob", "get", &hash, "--output"])
        .arg(&out_path)
        .output()
        .expect("run loom blob get over existing file");
    assert!(out.status.success());
    assert_eq!(std::fs::read(&out_path).unwrap(), wav);
}

#[test]
fn blob_get_missing_hash_is_a_typed_daemon_error_exit_1() {
    let (h, _data_root) = harness();
    let absent = "a".repeat(64);

    let out = h
        .loom_command()
        .args(["blob", "get", &absent, "-o"])
        .arg(h.home().join("never.wav"))
        .output()
        .expect("run loom blob get for absent hash");
    assert_eq!(
        out.status.code(),
        Some(1),
        "daemon store_not_found must map to exit 1; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !h.home().join("never.wav").exists(),
        "no output file on failure"
    );
}

#[test]
fn blob_get_malformed_hash_is_rejected_by_the_daemon() {
    let (h, _data_root) = harness();

    let out = h
        .loom_command()
        .args(["blob", "get", "not-a-hash", "-o"])
        .arg(h.home().join("never.wav"))
        .output()
        .expect("run loom blob get with malformed hash");
    assert!(!out.status.success(), "malformed ref must fail");
    assert!(
        !h.home().join("never.wav").exists(),
        "no output file on failure"
    );
}
