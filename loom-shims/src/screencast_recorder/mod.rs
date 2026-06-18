//! `screencast_recorder` — see crate root.
//!
//! video-capture: per-target CDP `Page.startScreencast` recording. Mirrors the
//! event-streaming/accumulator pattern of `network_interceptor`, but adds a
//! per-frame `Page.screencastFrameAck` loop (Chrome throttles the stream until
//! each frame is acked) and an ffmpeg encode step on stop. The recorder lives
//! host/shim-side (NOT in the WASM guest): streaming CDP events + an ffmpeg
//! subprocess + large byte buffers can only run here. Determinism: the encoded
//! `.webm` is non-deterministic and lives OUTSIDE the manifest hash chain (only
//! its content hash is recorded in the stop receipt), so recording never affects
//! replay-equality (NFR-DET-01).
pub mod screencast_recorder;
pub use screencast_recorder::*;

#[cfg(test)]
mod interface_tests;
