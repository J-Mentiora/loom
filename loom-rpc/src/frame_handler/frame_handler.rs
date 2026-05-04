// Re-export of the locked Phase 5.3 interface. DO NOT EDIT here.
// Edit `systems/loom-rpc/modules/frame_handler/interfaces.rs` instead.
// FrameHandler — `tokio_util::codec::LengthDelimitedCodec` wrapper
// per IC-RPC-04. 4-byte big-endian length prefix; 16 MB payload cap.
//
// # Contract semantics
// - **Codec (IC-RPC-04).** `tokio_util::codec::LengthDelimitedCodec`
//   configured with `length_field_offset=0`, `length_field_length=4`,
//   `length_adjustment=0`, big-endian length encoding.
// - **Payload cap.** `MAX_FRAME_BYTES = 16 * 1024 * 1024` (16 MB).
//   Frames exceeding this cap return a codec error; the handler
//   drops the connection (DoS guard) per design.md §4.
// - **Pure codec.** No state beyond the codec configuration. The
//   `Framed` wrapper owns the read/write buffers.

use tokio::net::UnixStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

/// Maximum payload bytes accepted per request (IC-RPC-04 + design.md
/// soft binding). Exceeding this returns `LoomErrorCode::ProtocolMalformed`
/// and closes the connection.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// Length-prefix size in bytes (IC-RPC-04: 4-byte big-endian).
pub const LENGTH_PREFIX_BYTES: usize = 4;

/// The framed stream type used per-connection. Returned from
/// `FrameHandler::wrap_stream` and consumed by `ConnectionHandler`.
pub type FramedUnixStream = Framed<UnixStream, LengthDelimitedCodec>;

pub struct FrameHandler;

impl FrameHandler {
    /// Build the codec configured per IC-RPC-04. Pure builder; no I/O.
    pub fn build_codec() -> LengthDelimitedCodec {
        LengthDelimitedCodec::builder()
            .length_field_offset(0)
            .length_field_length(LENGTH_PREFIX_BYTES)
            .length_adjustment(0)
            .max_frame_length(MAX_FRAME_BYTES)
            .big_endian()
            .new_codec()
    }

    /// Wrap a `UnixStream` in the configured codec. Called once per
    /// accepted connection by `ConnectionHandler`.
    pub fn wrap_stream(stream: UnixStream) -> FramedUnixStream {
        Framed::new(stream, Self::build_codec())
    }
}
