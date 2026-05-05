// DeterminismHarness implementation.
//
// AC coverage:
//   AC-DET-01.1: clock_now() returns frozen per-action ms value
//   AC-DET-02.1: rng_next() draws from ChaCha20 seeded per-session
//   IC-CORE-04/SR-CORE-17: canonicalize() uses serde_jcs (RFC 8785)
//   IC-CORE-07: ReplayHostFnTable pop_{clock,rng,net}()

use crate::determinism_harness::determinism_harness::{
    DeterminismHarness, ReplayHostFnTable, SideEffectTape, TapeFrame, TapeWriter,
};
use loom_core::error::{LoomError, LoomErrorCode};
use rand_core::RngCore;
use ring::digest::{digest, SHA256};
use std::path::Path;

impl DeterminismHarness {
    /// Canonicalize a serializable value to RFC 8785 JCS bytes.
    /// THE ONLY canonicalizer in loom-core (IC-CORE-04 / SR-CORE-17).
    /// Uses serde_jcs::to_string (the 0.1 crate does not expose to_vec).
    pub fn canonicalize<T: serde::Serialize>(&self, value: &T) -> Result<Vec<u8>, LoomError> {
        serde_jcs::to_string(value)
            .map(|s| s.into_bytes())
            .map_err(|e| LoomError::new(LoomErrorCode::Internal, e.to_string()))
    }

    /// Compute sha256(canonical_bytes) — returns 64-char lowercase hex.
    pub fn hash_canonical(&self, canonical_bytes: &[u8]) -> String {
        let d = digest(&SHA256, canonical_bytes);
        hex::encode(d.as_ref())
    }

    /// Virtual clock read — returns the frozen per-action millisecond value.
    /// All calls within one action return the same value (AC-DET-01.1).
    /// `begin_action(delta_ms)` advances the clock between actions.
    pub fn clock_now(&self) -> u64 {
        *self.action_clock_ms.lock()
    }

    /// Seeded ChaCha20 draw — deterministic given session seed (AC-DET-02.1).
    pub fn rng_next(&self) -> u64 {
        self.rng.lock().next_u64()
    }
}

impl TapeWriter {
    /// Persist the in-memory tape to `sessions_root/<session_id>/tape.jsonl`.
    /// Each TapeFrame serialized as JCS JSON, one per line.
    pub fn persist(&self, sessions_root: &Path, session_id: &str) -> Result<(), LoomError> {
        let session_dir = sessions_root.join(session_id);
        std::fs::create_dir_all(&session_dir)?;
        let tape_path = session_dir.join("tape.jsonl");
        let mut content = String::new();
        for frame in &self.frames {
            let line = serde_jcs::to_string(frame).map_err(|e| {
                LoomError::new(LoomErrorCode::Internal, format!("tape serialize: {e}"))
            })?;
            content.push_str(&line);
            content.push('\n');
        }
        std::fs::write(&tape_path, content.as_bytes())?;
        Ok(())
    }
}

impl SideEffectTape {
    /// Load tape from `sessions_root/<session_id>/tape.jsonl`.
    /// Returns empty tape if file not found (pre-tape-persistence sessions).
    pub fn load_from_file(sessions_root: &Path, session_id: &str) -> Result<Self, LoomError> {
        let tape_path = sessions_root.join(session_id).join("tape.jsonl");
        let content = match std::fs::read_to_string(&tape_path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(LoomError::from(e)),
            Ok(c) => c,
        };
        let mut frames = Vec::new();
        for line in content.lines() {
            if line.is_empty() {
                continue;
            }
            let frame: TapeFrame = serde_json::from_str(line)
                .map_err(|e| LoomError::new(LoomErrorCode::Internal, format!("tape parse: {e}")))?;
            frames.push(frame);
        }
        Ok(Self { frames })
    }
}

impl ReplayHostFnTable {
    /// Pop the next `ClockRead` frame; error if next frame is wrong kind or tape exhausted.
    pub fn pop_clock(&self) -> Result<u64, LoomError> {
        let mut cur = self.cursor.lock();
        match self.tape.frames.get(*cur) {
            Some(TapeFrame::ClockRead { observed_ns }) => {
                let val = *observed_ns;
                *cur += 1;
                Ok(val)
            }
            Some(_) => Err(LoomError::new(
                LoomErrorCode::Internal,
                "tape mismatch: expected ClockRead".to_string(),
            )),
            None => Err(LoomError::new(
                LoomErrorCode::Internal,
                "tape exhausted: no ClockRead frame".to_string(),
            )),
        }
    }

    /// Pop the next `RngDraw` frame.
    pub fn pop_rng(&self) -> Result<u64, LoomError> {
        let mut cur = self.cursor.lock();
        match self.tape.frames.get(*cur) {
            Some(TapeFrame::RngDraw { value_u64 }) => {
                let val = *value_u64;
                *cur += 1;
                Ok(val)
            }
            Some(_) => Err(LoomError::new(
                LoomErrorCode::Internal,
                "tape mismatch: expected RngDraw".to_string(),
            )),
            None => Err(LoomError::new(
                LoomErrorCode::Internal,
                "tape exhausted: no RngDraw frame".to_string(),
            )),
        }
    }

    /// Pop the next `NetResponse` frame.
    pub fn pop_net(&self, _request_id: u64) -> Result<TapeFrame, LoomError> {
        let mut cur = self.cursor.lock();
        match self.tape.frames.get(*cur) {
            Some(f @ TapeFrame::NetResponse { .. }) => {
                let frame = f.clone();
                *cur += 1;
                Ok(frame)
            }
            Some(_) => Err(LoomError::new(
                LoomErrorCode::Internal,
                "tape mismatch: expected NetResponse".to_string(),
            )),
            None => Err(LoomError::new(
                LoomErrorCode::Internal,
                "tape exhausted: no NetResponse frame".to_string(),
            )),
        }
    }
}
