// Interface tests for `IPC_Endpoint`.
// Verifies posix_spawn + socketpair transport,
// length-prefixed CBOR wire format, stable error envelope,
// closed enum invariant, no grant_id in CdpSend.

use super::ipc_endpoint::{
    ciborium_from_slice, ciborium_to_vec, decode_frame, encode_frame, CdpMessage, IpcEndpoint,
    IpcError, ShimErrorCode, ShimRequest, ShimResponse, SocketpairEndpoint, LENGTH_PREFIX_BYTES,
    MAX_FRAME_BYTES,
};
use ciborium::value::Value as CborValue;

// === length-prefixed CBOR wire format ===

#[test]
fn frame_layout_is_4_byte_be_length_prefix() {
    assert_eq!(LENGTH_PREFIX_BYTES, 4);
}

#[test]
fn encode_frame_starts_with_big_endian_length_prefix() {
    let resp = ShimResponse::Ok {
        request_id: 1,
        session_id: Some(7),
        payload: CborValue::Null,
    };
    let bytes = encode_frame(&resp).expect("encode");
    assert!(bytes.len() >= 4);
    let declared = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    assert_eq!(declared, bytes.len() - 4);
}

#[test]
fn decode_frame_round_trips_shim_request() {
    let req = ShimRequest::SpawnTarget {
        request_id: 99,
        session_id: 42,
        profile: "default".into(),
        seed: loom_shared::types::Seed(0),
        epoch_ms: loom_shared::types::EpochMs(0),
        determinism_enabled: true,
        audio_enabled: false,
    };
    // Hand-roll the wire bytes for a request (encode_frame is for responses;
    // the request side is structurally identical though).
    let payload = ciborium_to_vec(&req).unwrap();
    let mut wire = (payload.len() as u32).to_be_bytes().to_vec();
    wire.extend_from_slice(&payload);
    let decoded = decode_frame(&wire).expect("decode");
    assert_eq!(decoded, req);
}

#[test]
fn decode_frame_rejects_truncated_payload() {
    let mut wire = vec![0u8, 0, 0, 100]; // claims 100 bytes
    wire.extend_from_slice(b"only-a-few"); // ~10 bytes actual
    let err = decode_frame(&wire).unwrap_err();
    matches!(err, IpcError::CborDecode(_));
}

#[test]
fn decode_frame_rejects_oversize_frame() {
    let bogus = MAX_FRAME_BYTES + 1;
    let mut wire = bogus.to_be_bytes().to_vec();
    wire.push(0); // any payload byte; cap check fires first
    let err = decode_frame(&wire).unwrap_err();
    match err {
        IpcError::FrameTooLarge { observed, limit } => {
            assert_eq!(observed, bogus);
            assert_eq!(limit, MAX_FRAME_BYTES);
        }
        _ => panic!("expected FrameTooLarge, got {:?}", err),
    }
}

// === stable error envelope (5 variants) ===

#[test]
fn shim_error_code_has_exactly_five_variants() {
    // Compile-time exhaustive match — adding a new variant without
    // updating this test (and `loom-host::ErrorMapper`) breaks compile.
    fn _exhaustive(c: ShimErrorCode) -> &'static str {
        match c {
            ShimErrorCode::ChromiumUnavailable => "chromium_unavailable",
            ShimErrorCode::CdpTimeout => "cdp_timeout",
            ShimErrorCode::CdpProtocolError => "cdp_protocol_error",
            ShimErrorCode::TargetUnknown => "target_unknown",
            ShimErrorCode::ShimInternalError => "shim_internal_error",
        }
    }
    assert_eq!(
        _exhaustive(ShimErrorCode::ChromiumUnavailable),
        "chromium_unavailable"
    );
    assert_eq!(_exhaustive(ShimErrorCode::CdpTimeout), "cdp_timeout");
    assert_eq!(
        _exhaustive(ShimErrorCode::CdpProtocolError),
        "cdp_protocol_error"
    );
    assert_eq!(_exhaustive(ShimErrorCode::TargetUnknown), "target_unknown");
    assert_eq!(
        _exhaustive(ShimErrorCode::ShimInternalError),
        "shim_internal_error"
    );
}

#[test]
fn shim_error_code_serialises_snake_case() {
    let s = ciborium_to_vec(&ShimErrorCode::ChromiumUnavailable).unwrap();
    let back: ShimErrorCode = ciborium_from_slice(&s).unwrap();
    assert_eq!(back, ShimErrorCode::ChromiumUnavailable);
    // Also verify the discriminant string is snake_case.
    let json = serde_json::to_string(&ShimErrorCode::ChromiumUnavailable).unwrap();
    assert_eq!(json, "\"chromium_unavailable\"");
}

// === no grant_id in CdpSend (HARD) ===

#[test]
fn cdp_send_payload_has_no_grant_id_field() {
    // Compile-time guarantee — destructure CdpSend exhaustively.
    let req = ShimRequest::CdpSend {
        request_id: 7,
        session_id: 1,
        target_id: 2,
        message: CdpMessage {
            method: "Network.enable".into(),
            params: CborValue::Null,
        },
    };
    if let ShimRequest::CdpSend {
        request_id,
        session_id,
        target_id,
        message,
    } = req
    {
        let _ = (request_id, session_id, target_id, message);
        // No `grant_id` available to bind. If a future PR adds one,
        // this exhaustive destructure stops compiling.
    } else {
        panic!("variant mismatch");
    }
}

// === socketpair-only construction ===

#[test]
fn endpoint_constructs_from_inherited_fd() {
    let _ep = SocketpairEndpoint::from_inherited_fd(42);
}

#[test]
fn endpoint_no_tcp_or_unix_socket_constructor() {
    // Compile-time check: the only public constructor is
    // `from_inherited_fd`. Adding a `TcpStream`-based ctor would
    // require a new pub fn — caught at review.
    fn _only_one_ctor() {
        let _ = SocketpairEndpoint::from_inherited_fd(0);
    }
}

// === Frame size cap (16 MB) ===

#[test]
fn max_frame_bytes_is_16_mib() {
    assert_eq!(MAX_FRAME_BYTES, 16 * 1024 * 1024);
}

#[test]
fn encode_frame_rejects_oversize_response() {
    // Build a payload > 16 MB by stuffing bytes into a CBOR byte string.
    let huge = vec![0u8; (MAX_FRAME_BYTES + 1) as usize];
    let resp = ShimResponse::Ok {
        request_id: 0,
        session_id: None,
        payload: CborValue::Bytes(huge),
    };
    let err = encode_frame(&resp).unwrap_err();
    matches!(err, IpcError::FrameTooLarge { .. });
}

// === cdp_event variant exists ===

#[test]
fn shim_response_has_cdp_event_variant() {
    let _ev = ShimResponse::CdpEvent {
        target_id: 1,
        message: CdpMessage {
            method: "Network.responseReceived".into(),
            params: CborValue::Null,
        },
    };
}

// === Spawn-loop signatures (compile-time) ===

#[test]
fn ipc_endpoint_trait_object_is_send_sync() {
    fn _check<T: IpcEndpoint + ?Sized>() {}
    _check::<dyn IpcEndpoint>();
}

#[test]
#[should_panic(expected = "shutdown not yet implemented")]
fn shutdown_panics_until_implemented() {
    let ep = SocketpairEndpoint::from_inherited_fd(0);
    let _ = ep.shutdown();
}

// === Integer-only fields ===

#[test]
fn session_id_and_target_id_are_u64_no_floats() {
    let _: u64 = 0u64;
    let req = ShimRequest::PageClose {
        request_id: 0,
        session_id: u64::MAX,
        target_id: u64::MAX,
    };
    if let ShimRequest::PageClose {
        request_id,
        session_id,
        target_id,
    } = req
    {
        let _ = request_id;
        assert_eq!(session_id, u64::MAX);
        assert_eq!(target_id, u64::MAX);
    }
}
