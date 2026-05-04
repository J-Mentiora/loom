// Re-export of the locked Phase 5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-rpc/modules/frame_handler/interface_tests.rs` instead.
// Interface tests for `FrameHandler`. Verifies IC-RPC-04 codec
// configuration: 4-byte big-endian length prefix + 16 MB cap.

use super::frame_handler::{
    FrameHandler, FramedUnixStream, LENGTH_PREFIX_BYTES, MAX_FRAME_BYTES,
};
use tokio::net::UnixStream;
use tokio_util::codec::LengthDelimitedCodec;

#[test]
fn max_frame_bytes_is_16_mib() {
    // IC-RPC-04 payload cap.
    assert_eq!(MAX_FRAME_BYTES, 16 * 1024 * 1024);
}

#[test]
fn length_prefix_is_4_bytes() {
    // IC-RPC-04 length prefix size.
    assert_eq!(LENGTH_PREFIX_BYTES, 4);
}

#[test]
fn build_codec_returns_length_delimited_codec() {
    fn _ck() -> LengthDelimitedCodec {
        FrameHandler::build_codec()
    }
    let _ = _ck;
}

#[test]
fn wrap_stream_takes_unix_stream_returns_framed() {
    fn _ck(s: UnixStream) -> FramedUnixStream {
        FrameHandler::wrap_stream(s)
    }
    let _ = _ck;
}

#[test]
fn framed_unix_stream_alias_is_framed_over_unix_stream() {
    // Compile-time check that the alias resolves to the right concrete
    // type — guards IC-RPC-04 transport identity.
    fn _ck(f: FramedUnixStream) -> tokio_util::codec::Framed<
        UnixStream,
        LengthDelimitedCodec,
    > {
        f
    }
    let _ = _ck;
}
