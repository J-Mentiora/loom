// IPC_Endpoint — sole external surface of `loom-shim-chromium`.
//
// # Contract semantics
// - **Wire format.** Length-prefixed CBOR. Every frame is
//   `[4 bytes big-endian length][CBOR payload]`. JSON / unprefixed
//   framing → KILL.
// - **Transport (HARD).** `socketpair(2)` FD inherited from
//   `loom-host::ShimManager` via `posix_spawn`. NO TCP, NO Unix-socket
//   file, NO shared memory.
// - **Sole boundary owner.** `IPC_Endpoint` is the SOLE module on the
//   shim side that touches the wire. `Dispatcher` consumes typed
//   `ShimRequest` values; `LogForwarder` and `Dispatcher` push typed
//   `ShimResponse` / `cdp_event` values back through this module.
// - **Frame size cap (soft).** 16 MB max payload. Larger → `FrameTooLarge`
//   → fatal exit (daemon respawns).
// - **Failure mode.** Wire-protocol errors are FATAL — `eof`, `cbor_decode`,
//   `frame_too_large` all `std::process::exit(1)`. Daemon's `ShimManager`
//   sees socketpair EOF and treats it as a crash.
//
// # CBOR schema (shared via `loom_shared::shim_protocol`)
// `ShimRequest` / `ShimResponse` / `ShimErrorCode` / `CdpMessage` / framing
// helpers are defined in `loom_shared::shim_protocol` and re-exported here
// for backward compatibility. They MUST stay in `loom-shared` because
// `loom-host::ShimManager` reads/writes the same wire format and is
// forbidden from depending on `loom-shims` (chromiumoxide isolation).

use std::os::unix::io::RawFd;
use std::sync::Arc;
use tokio::sync::mpsc;

// Re-export the wire-format types from the shared crate so existing
// `crate::ipc_endpoint::ipc_endpoint::*` imports keep working.
pub use loom_shared::shim_protocol::{
    ciborium_from_slice, ciborium_to_vec, decode_frame, encode_frame, CdpMessage, IpcError,
    SessionId, ShimErrorCode, ShimRequest, ShimResponse, TargetId, LENGTH_PREFIX_BYTES,
    MAX_FRAME_BYTES,
};

/// Concrete `IPC_Endpoint`. Owns the inherited socketpair FD; the
/// FD itself is not closed on drop unless `shutdown()` is called —
/// the OS reclaims it on process exit, which is the canonical path.
pub struct SocketpairEndpoint {
    pub(crate) fd: RawFd,
    pub(crate) max_frame_bytes: u32,
}

impl SocketpairEndpoint {
    /// Adopt an inherited FD. Called from `main.rs` after parsing the
    /// shim's argv (the daemon passes `--socket-fd <N>` at posix_spawn).
    pub fn from_inherited_fd(fd: RawFd) -> Self {
        Self {
            fd,
            max_frame_bytes: MAX_FRAME_BYTES,
        }
    }

    /// Override the frame size cap. Used only by tests.
    pub fn with_max_frame_bytes(mut self, n: u32) -> Self {
        self.max_frame_bytes = n;
        self
    }
}

/// Public IPC trait surface. Two halves: read-loop and write-loop.
/// Both run as separate tokio tasks; they share the same FD via
/// duplicate file descriptors (`dup`) on the read and write halves.
pub trait IpcEndpoint: Send + Sync {
    /// Spawn the read loop. Drains `[len][CBOR]` frames; emits typed
    /// `ShimRequest` values to the bounded channel. On any wire error
    /// (EOF / decode / size), exits the process (FATAL).
    fn spawn_read_loop(&self, request_tx: mpsc::Sender<ShimRequest>) -> Result<(), IpcError>;

    /// Spawn the write loop. Reads `ShimResponse` values from the
    /// channel and serialises each as `[4 BE len][CBOR payload]`.
    fn spawn_write_loop(&self, response_rx: mpsc::Receiver<ShimResponse>) -> Result<(), IpcError>;

    /// Cooperative shutdown — flushes the write side and closes
    /// the FD. Returns once both loops have exited.
    fn shutdown(&self) -> Result<(), IpcError>;
}

impl IpcEndpoint for SocketpairEndpoint {
    fn spawn_read_loop(&self, _request_tx: mpsc::Sender<ShimRequest>) -> Result<(), IpcError> {
        // socketpair read-loop with CBOR framing
        Err(IpcError::Io(
            "socketpair read-loop not yet implemented".into(),
        ))
    }

    fn spawn_write_loop(&self, _response_rx: mpsc::Receiver<ShimResponse>) -> Result<(), IpcError> {
        // socketpair write-loop with CBOR framing
        Err(IpcError::Io(
            "socketpair write-loop not yet implemented".into(),
        ))
    }

    fn shutdown(&self) -> Result<(), IpcError> {
        panic!("v5.4 implementation")
    }
}

/// Type alias for the inbound ShimRequest channel — exposed so
/// `Dispatcher` can hold the receiver without re-deriving the type.
pub type RequestChannel = (mpsc::Sender<ShimRequest>, mpsc::Receiver<ShimRequest>);

/// Type alias for the outbound ShimResponse channel. Wrapped in `Arc`
/// because `LogForwarder` and `Dispatcher` both push to the same writer.
pub type ResponseSender = Arc<mpsc::Sender<ShimResponse>>;
