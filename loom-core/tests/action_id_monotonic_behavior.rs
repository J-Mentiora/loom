//! Behavior tests for per-session monotonic action_id.
//!
//! - receipts in a session monotonically increase action_id from 0
//! - `loom session inspect` entries[].action_id matches the action_id
//!   returned in the receipt at the time of dispatch
//! - integration: 5 actions in one session yield action_id [0,1,2,3,4]

use loom_core::core_api_facade::{CoreApiFacade, CoreConfig};
use loom_core::manifest_writer::ManifestEntry;
use loom_core::session_manager::session_manager::SessionCreateOpts;
use std::sync::Arc;
use tempfile::TempDir;

fn make_core(dir: &TempDir) -> Arc<CoreApiFacade> {
    let data_root = dir.path().to_path_buf();
    let config = CoreConfig {
        data_root: data_root.clone(),
        log_path: data_root.join("daemon.log"),
        otel_enabled: false,
        default_seed: 42,
        checkpoint_every_n: 100,
    };
    let keychain: Arc<dyn loom_core::vault::KeychainAccess> = Arc::new(loom_keychain::StubKeychain);
    CoreApiFacade::new(config, keychain).expect("CoreApiFacade::new must succeed")
}

fn default_opts() -> SessionCreateOpts {
    SessionCreateOpts {
        agent_id: "test-agent".into(),
        surface: "test".into(),
        seed: None,
        limits: None,
        replay_of: None,
        started_at_ms_override: None,
        capture_policy: None,
        no_blocklist: false,
        no_determinism: false,
        profile: "safe".to_string(),
    }
}

// === monotonic increment ===

#[test]
fn session_allocate_action_id_starts_at_zero_and_increments_monotonically() {
    let dir = TempDir::new().unwrap();
    let core = make_core(&dir);

    let id = core.session_manager.create(default_opts()).unwrap();
    let session = core.session_manager.get(id).unwrap();

    let ids: Vec<u64> = (0..5).map(|_| session.allocate_action_id()).collect();
    assert_eq!(
        ids,
        vec![0, 1, 2, 3, 4],
        "first 5 allocations on a fresh session must be 0..4"
    );
}

// === five-action integration (also covers id parity in WAL & inspect) ===

#[test]
fn five_actions_in_one_session_yield_ids_zero_through_four() {
    let dir = TempDir::new().unwrap();
    let core = make_core(&dir);

    let id = core.session_manager.create(default_opts()).unwrap();
    let session = core.session_manager.get(id.clone()).unwrap();

    // Simulate 5 dispatches: allocate id, persist a corresponding ActionReceipt
    // entry. This is the same pattern the daemon's receipt_marshaller uses
    // after a real host.dispatch — the id stamped on the WAL entry equals the
    // receipt's action_id because both come from the same
    // `allocate_action_id()` call.
    let mut allocated_ids = Vec::new();
    for i in 0..5 {
        let action_id = session.allocate_action_id();
        allocated_ids.push(action_id);
        core.manifest_writer
            .append(
                id.clone(),
                ManifestEntry::ActionReceipt {
                    action_id,
                    emitted_at_ms: 1_000 + i,
                    receipt_canonical_bytes: format!("receipt-{action_id}").into_bytes(),
                    prev_hash: String::new(),
                },
            )
            .unwrap();
    }
    assert_eq!(
        allocated_ids,
        vec![0, 1, 2, 3, 4],
        "5 dispatches must yield action_ids [0,1,2,3,4]"
    );

    // inspect_session_json must report the same ids in entries[].
    let inspect = core.inspect_session_json(&id.0, None).unwrap();
    let entries = inspect["entries"]
        .as_array()
        .expect("entries must be array");
    assert_eq!(entries.len(), 5);
    let inspect_ids: Vec<u64> = entries
        .iter()
        .map(|e| e["action_id"].as_u64().expect("action_id must be u64"))
        .collect();
    assert_eq!(
        inspect_ids,
        vec![0, 1, 2, 3, 4],
        "inspect entries[].action_id must match allocated ids"
    );
    assert_eq!(
        inspect_ids, allocated_ids,
        "inspect ids must match the receipt ids returned at dispatch"
    );
}

// === inspect-id parity (focused) ===

#[test]
fn inspect_action_id_matches_allocated_action_id() {
    let dir = TempDir::new().unwrap();
    let core = make_core(&dir);

    let id = core.session_manager.create(default_opts()).unwrap();
    let session = core.session_manager.get(id.clone()).unwrap();

    let dispatched_id = session.allocate_action_id();
    core.manifest_writer
        .append(
            id.clone(),
            ManifestEntry::ActionReceipt {
                action_id: dispatched_id,
                emitted_at_ms: 42,
                receipt_canonical_bytes: b"receipt".to_vec(),
                prev_hash: String::new(),
            },
        )
        .unwrap();

    let inspect = core.inspect_session_json(&id.0, None).unwrap();
    let entries = inspect["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0]["action_id"].as_u64().unwrap(),
        dispatched_id,
        "inspect entry action_id must equal the dispatched action_id"
    );
}
