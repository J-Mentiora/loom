//! G5a 100ms perf test (A-W5.3 / FND-0018).
//!
//! The plan commits to "audit appended ≤100ms after the keychain RPC
//! returns" for credential ops. This file wraps `vault.set_secret` against
//! `InMemoryKeychain` (a constant-time backend) so the measured latency
//! is dominated by audit-chain serialization + WAL fsync — not by the
//! keychain backend. Across 1000 iterations:
//!
//!   p95 < 100ms ∧ max < 500ms
//!
//! Run: `cargo test -p loom-core --test audit_timing -- --include-ignored`
//! Filed `#[ignore]` because it's a perf assertion (10–60 seconds wall),
//! not a correctness test. CI runs it on the perf cell.

use loom_core::manifest_writer::{LocalManifestWriter, ManifestWriter, SessionId};
use loom_core::observability::Observability;
use loom_core::vault::vault::{LocalVault, Vault};
use loom_keychain::InMemoryKeychain;
use std::sync::Arc;
use std::time::Instant;
use zeroize::Zeroizing;

const N: usize = 1000;
const P95_MAX_MS: u128 = 100;
const HARD_MAX_MS: u128 = 500;

#[test]
#[ignore]
fn vault_set_secret_audit_latency_p95_under_100ms() {
    let tmp = std::env::temp_dir().join(format!("loom-audit-timing-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("create tmp");
    let obs = Observability::new(tmp.join("loom.log"), false);
    let writer: Arc<dyn ManifestWriter> = Arc::new(LocalManifestWriter::new(tmp.clone(), obs.clone()));
    let session = SessionId(format!("01HZTIMINGZZZZZZZZZZZZ{:04x}", std::process::id() % 0xffff));
    // Bootstrap the manifest so `append_audit` has a header to chain off.
    let _ = writer
        .open_manifest(session.clone(), None)
        .expect("open_manifest");

    let keychain = Arc::new(InMemoryKeychain::new());
    let vault = LocalVault::new(keychain, writer, obs);

    let mut samples_us: Vec<u128> = Vec::with_capacity(N);
    for i in 0..N {
        let label = format!("perf-{i:04}");
        let secret = Zeroizing::new(b"audit-timing-secret".to_vec());
        let start = Instant::now();
        vault
            .set_secret(Some(&session), &label, secret)
            .expect("set_secret");
        samples_us.push(start.elapsed().as_micros());
    }
    samples_us.sort_unstable();
    let p95 = samples_us[(N * 95) / 100];
    let max = *samples_us.last().unwrap();
    let p50 = samples_us[N / 2];

    eprintln!(
        "audit_timing: N={N} p50={}us p95={}us max={}us (limits p95<{}ms hard<{}ms)",
        p50, p95, max, P95_MAX_MS, HARD_MAX_MS
    );

    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        p95 < P95_MAX_MS * 1000,
        "G5a regression: p95 {}us ≥ {}ms",
        p95,
        P95_MAX_MS
    );
    assert!(
        max < HARD_MAX_MS * 1000,
        "G5a hard ceiling: max {}us ≥ {}ms",
        max,
        HARD_MAX_MS
    );
}
