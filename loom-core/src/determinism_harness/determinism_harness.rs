// DeterminismHarness — virtual clock + seeded RNG + JCS canonicalization.
//
// Single home for all determinism mechanisms. Owns the `SideEffectTape`
// recorded during action execution, hands it to `ReplayEngine` on demand.
//
// # Contract semantics
// - `canonicalize` uses `serde_jcs` (RFC 8785). The ONLY canonicalizer
//   in `loom-core`. `clippy::disallowed_methods` bans direct use of
//   `serde_json::to_string` in manifest/receipt paths (SR-CORE-10).
// - `clock_now` returns virtual time (a monotonically advancing u64 of
//   nanoseconds since session start). NEVER reads the wall clock during
//   normal operation; honored when `--no-virtual-clock` flag flips it
//   into pass-through mode.
// - `rng_next` consumes from a seeded ChaCha20 stream (BC HARD #5
//   determinism). Seeded at session creation.
// - `install_replay_mode(tape)` swaps the host-fn vtable so that
//   `clock_now`/`rng_next`/`net_request` resolve against the tape, not
//   live sources.

use loom_core::manifest_writer::ManifestWriter;
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// One frame on the side-effect tape. Each variant is what was observed
/// (clock read, RNG draw, network response, blob ref, LLM response) during
/// recording and what gets replayed during replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TapeFrame {
    ClockRead { observed_ns: u64 },
    RngDraw { value_u64: u64 },
    NetResponse {
        request_id: u64,
        status: u16,
        body_ref_sha256: String,
        body_size_bytes: u64,
    },
    BlobRead {
        sha256: String,
        size_bytes: u64,
    },
    /// AC-DET-07.1: LLM response recorded during a session.
    /// `ReplayEngine` uses this frame to seed the `LlmCache` on replay.
    LlmResponse {
        model: String,
        /// sha256(JCS(prompt_json)) — 64 lowercase hex chars.
        prompt_hash: String,
        tool_schema_version: String,
        body_json: serde_json::Value,
    },
}

/// The recorded tape for a session. Materialized in `ReplayEngine` on
/// replay; written incrementally to the manifest hash chain during recording.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SideEffectTape {
    pub frames: Vec<TapeFrame>,
}

/// Replay-mode host-function table. Returned by `install_replay_mode`;
/// `ReplayEngine` hands it to `loom-host`'s dispatcher.
pub struct ReplayHostFnTable {
    pub(crate) tape: SideEffectTape,
    pub(crate) cursor: parking_lot::Mutex<usize>,
}


/// Per-session tape writer (recording mode). Borrowed by host-functions.
pub struct TapeWriter {
    pub(crate) frames: Vec<TapeFrame>,
}

impl TapeWriter {
    pub fn new() -> Self {
        Self { frames: Vec::new() }
    }
    pub fn record(&mut self, frame: TapeFrame) {
        self.frames.push(frame);
    }
    pub fn snapshot(&self) -> SideEffectTape {
        SideEffectTape { frames: self.frames.clone() }
    }
}

impl Default for TapeWriter {
    fn default() -> Self { Self::new() }
}

/// The DeterminismHarness module. Held by `CoreApiFacade` as
/// `Arc<DeterminismHarness>`; per-session tape writers are minted via
/// `new_tape_writer`.
pub struct DeterminismHarness {
    pub(crate) seed: u64,
    pub(crate) virtual_clock_enabled: bool,
    pub(crate) seeded_rng_enabled: bool,
    pub(crate) manifest_writer: Arc<dyn ManifestWriter>,
    /// Frozen per-action clock in milliseconds. All `clock_now()` calls
    /// within one action return this value. Advanced via `begin_action()`.
    pub(crate) action_clock_ms: parking_lot::Mutex<u64>,
    /// ChaCha20 RNG seeded from `seed`. Per-harness instance (harnesses
    /// are per-session), so concurrent sessions do not share RNG state.
    pub(crate) rng: parking_lot::Mutex<ChaCha20Rng>,
}

impl DeterminismHarness {
    pub fn new(seed: u64, manifest_writer: Arc<dyn ManifestWriter>) -> Self {
        Self {
            seed,
            virtual_clock_enabled: true,
            seeded_rng_enabled: true,
            manifest_writer,
            action_clock_ms: parking_lot::Mutex::new(0),
            rng: parking_lot::Mutex::new(ChaCha20Rng::seed_from_u64(seed)),
        }
    }

    /// Advance the frozen action clock by `delta_ms` milliseconds.
    /// Called by `SessionExecutor` at each action boundary (between actions).
    /// NOT called during an action — `clock_now()` is frozen within an action.
    pub fn begin_action(&self, delta_ms: u64) {
        *self.action_clock_ms.lock() += delta_ms;
    }

    /// Read the per-harness seed. Used by tests + the seed-threading
    /// e2e validation that `LocalSessionManager::create({seed: Some(N)})`
    /// produces a session whose harness reports `seed() == N`.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// Mint a fresh per-session tape writer.
    pub fn new_tape_writer(&self) -> TapeWriter {
        TapeWriter::new()
    }

    /// Install replay mode: returns a host-fn table that resolves
    /// clock/rng/net against the tape. Called by `ReplayEngine`.
    pub fn install_replay_mode(&self, tape: SideEffectTape) -> ReplayHostFnTable {
        ReplayHostFnTable {
            tape,
            cursor: parking_lot::Mutex::new(0),
        }
    }
}
