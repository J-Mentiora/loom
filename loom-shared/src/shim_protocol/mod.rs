//! `shim_protocol` — wire-format types shared between `loom-host::ShimManager`
//! (parent of the shim subprocess) and `loom-shims::ipc_endpoint` (in-shim).
//!
//! # Why this lives in `loom-shared`
//! The CBOR shape on the socketpair must match bit-for-bit on both ends.
//! Putting the types in `loom-shims` would require `loom-host` to depend on
//! `loom-shims`, which is forbidden — `loom-shims` is the only crate allowed
//! to link `chromiumoxide` (cargo-deny enforced).
//!
//! # Wire format
//! Length-prefixed CBOR. Every frame is `[4 bytes big-endian length][CBOR
//! payload]`. JSON / unprefixed framing → KILL.
//!
//! # Closed enums
//! `ShimErrorCode` is a 5-variant closed enum . Adding a variant
//! requires a `loom-host::ErrorMapper` update.
//!
//! `ShimRequest` and `ShimResponse` are also closed — there is no
//! `#[serde(other)]` fallback. An unknown `kind` is a fatal CBOR decode
//! error in the shim's read loop. **Adding a variant requires
//! same-tree daemon+shim shipping; mixed deployments are NOT supported.**
//! The codebase ships everything in one tarball
//! ([dist-workspace.toml](../../dist-workspace.toml)) so this invariant
//! holds in practice. The `Health` variant was added under this
//! invariant; an older shim will exit its read loop on receiving it,
//! observable at the host as a 1s probe timeout via `daemon.health`.

use crate::types::{EpochMs, Seed};
use serde::{Deserialize, Serialize};

/// Serialize a value to CBOR bytes using ciborium.
pub fn ciborium_to_vec<T: Serialize>(val: &T) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    ciborium::ser::into_writer(val, &mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

/// Deserialize CBOR bytes using ciborium.
pub fn ciborium_from_slice<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    ciborium::de::from_reader(bytes).map_err(|e| e.to_string())
}

/// Maximum CBOR payload size on a single frame. Frames larger than
/// this are a protocol violation → fatal.
pub const MAX_FRAME_BYTES: u32 = 16 * 1024 * 1024;

/// Length-prefix size in bytes (big-endian u32).
pub const LENGTH_PREFIX_BYTES: usize = 4;

/// Session identity assigned by `loom-host`. Opaque to the shim.
pub type SessionId = u64;

/// Chromium target id from CDP. Opaque to the daemon.
pub type TargetId = u64;

/// Closed `ShimErrorCode` enum. 5 variants only.
/// New variants require schema bump + `loom-host::ErrorMapper` update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShimErrorCode {
    /// `Supervisor` restart budget exhausted, or process died and could
    /// not be respawned. Mapped to `LoomErrorCode::SurfaceTrap`.
    ChromiumUnavailable,
    /// CDP roundtrip exceeded the action's budget. Mapped to
    /// `LoomErrorCode::WebActionTimeout`.
    CdpTimeout,
    /// chromiumoxide returned a CDP-protocol error response (e.g.
    /// invalid params, target detached). Mapped to
    /// `LoomErrorCode::SurfaceTrap`.
    CdpProtocolError,
    /// `session_id` ↔ `target_id` lookup miss in `TargetManager`.
    /// Mapped to `LoomErrorCode::SurfaceTrap`.
    TargetUnknown,
    /// Catch-all for shim-internal failures (CBOR decode of internal
    /// types, version mismatch, posix_spawn errno). Mapped to
    /// `LoomErrorCode::SurfaceTrap`.
    ShimInternalError,
}

/// CDP message body. Opaque to the shim's outer types — internally it
/// is a `ciborium::value::Value` that `ActionExecutor` re-encodes for
/// chromiumoxide. The shim does NOT introspect the method/params.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CdpMessage {
    pub method: String,
    /// CBOR-encoded params object. Validated by Chromium itself.
    pub params: ciborium::value::Value,
}

/// Serde default for `ShimRequest::PageNavigate.blocklist_enabled`.
/// Returns `true` so that an old-host payload (no field) is decoded
/// as enforcement-on by a new shim — the safe-default-on policy
/// documented inline on the variant.
pub fn default_blocklist_enabled() -> bool {
    true
}

/// Serde default for `ShimRequest::PageNavigate.until` (settle-capture).
/// Returns `"settled"` so an old-host payload (no field) is decoded as the
/// strict default by a new shim. The RPC layer validates the value before it
/// reaches the wire, so this is just the safe default, not a parse gate.
pub fn default_settle_until() -> String {
    "settled".to_string()
}

/// Serde default for the `determinism_enabled` wire fields (settle-capture 4b).
/// `true` so an old-shape payload decodes to the strict deterministic default.
pub fn default_determinism_enabled() -> bool {
    true
}

/// Inbound request frames from `loom-host::ShimManager`.
/// CBOR-encoded as a discriminated union via the `kind` tag.
///
/// Every variant carries a `request_id: u64` allocated by the host before
/// the frame is sent. The shim must echo it back in `ShimResponse::Ok` or
/// `ShimResponse::Error` so the host can demultiplex concurrent in-flight
/// requests on the single socketpair.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ShimRequest {
    /// Open a new Chromium target (browsing context). Returns `target_id`.
    /// `seed` and `epoch_ms` are rendered into the determinism JS template
    /// at inject time — both are EXPLICIT (no `serde(default)`); old
    /// payloads omitting these fields fail to decode (single-binary-tree
    /// project; host + shims always ship together).
    SpawnTarget {
        request_id: u64,
        session_id: SessionId,
        profile: String,
        seed: Seed,
        epoch_ms: EpochMs,
        /// settle-capture (4b): when `false`, the target_manager SKIPS the
        /// determinism freeze-inject (real clock + unseeded RNG) — the R3
        /// readiness flag still flips. Serde-default `true` so old-shape
        /// payloads decode to the strict deterministic default.
        #[serde(default = "default_determinism_enabled")]
        determinism_enabled: bool,
    },
    /// Issue a CDP command against a previously-spawned target.
    /// Note: no `grant_id` field. Vault substitution is upstream
    /// (a hard invariant of the shim protocol).
    CdpSend {
        request_id: u64,
        session_id: SessionId,
        target_id: TargetId,
        message: CdpMessage,
    },
    /// Navigate the target to `url`. Triggers `NetworkInterceptor`
    /// subscription before navigate (R1 pre-condition). When `target_id`
    /// is unknown to the shim's `TargetManager`, the shim lazily calls
    /// `create_new_target(session_id, "default", seed, epoch_ms)` to obtain
    /// a real target; otherwise the existing target is reused
    /// (idempotency). `seed`/`epoch_ms` are EXPLICIT (no
    /// `serde(default)`).
    ///
    /// `blocklist_enabled` — DELIBERATE
    /// DIVERGENCE from the seed/epoch_ms explicit-field convention:
    /// this field uses `serde(default = "default_blocklist_enabled")`
    /// returning `true` to preserve safe behavior under host/shim
    /// version skew. The wire-skew matrix:
    ///   - old-host + old-shim: field absent on wire → safe (pre-feature
    ///     behavior was no enforcement; nothing changes).
    ///   - old-host + new-shim: field absent on wire → defaults to
    ///     `true` → blocklist enforced (safe-default-on; matches the
    ///     brief's intent that the default blocklist applies unless
    ///     explicitly opted out).
    ///   - new-host + new-shim: field present, value carried as-is.
    ///   - new-host + old-shim: field on wire ignored by old decoder →
    ///     fails-open (no enforcement). Acknowledged downgrade hazard;
    ///     the codebase ships host+shims as a single binary tree, so
    ///     mismatched versions are not a supported deployment.
    PageNavigate {
        request_id: u64,
        session_id: SessionId,
        target_id: TargetId,
        url: String,
        seed: Seed,
        epoch_ms: EpochMs,
        #[serde(default = "default_blocklist_enabled")]
        blocklist_enabled: bool,
        /// settle-capture: readiness mode the shim gates the capture on
        /// (`load|networkidle|settled`). Serde-default `settled` so an
        /// old-shape payload decodes to the strict default.
        #[serde(default = "default_settle_until")]
        until: String,
        /// settle-capture (4b): when `false`, the navigate lazy-spawn path
        /// SKIPS the determinism freeze-inject (see `SpawnTarget`). Serde-
        /// default `true` so an old-shape payload decodes to deterministic.
        #[serde(default = "default_determinism_enabled")]
        determinism_enabled: bool,
    },
    /// settle-capture slice 2: standalone readiness wait on the current page
    /// (no navigation, no capture). The shim runs the same SettleDriver /
    /// ReadinessMonitor the navigate gate uses, then returns the settle verdict.
    WaitFor {
        request_id: u64,
        session_id: SessionId,
        target_id: TargetId,
        /// Readiness mode (`load|networkidle|settled`). Serde-default `settled`
        /// so an old-shape payload decodes to the strict default.
        #[serde(default = "default_settle_until")]
        until: String,
    },
    /// Close the target and drop its `TargetState`.
    PageClose {
        request_id: u64,
        session_id: SessionId,
        target_id: TargetId,
    },
    /// Read (NON-draining) the full-capture network-entries accumulator for a
    /// target — everything observed since the last navigate (document +
    /// click-triggered xhr/fetch). Backs the `loom.web.network_log` tool.
    /// Returns a `ShimResponse::Ok` whose CBOR payload is a
    /// `NetworkLogOutcome` (loom-shared). Observation-only (no CDP round-trip).
    GetNetworkLog {
        request_id: u64,
        session_id: SessionId,
        target_id: TargetId,
    },
    /// Cooperative shutdown. Drains in-flight, then exits.
    Shutdown { request_id: u64 },
    /// Liveness + introspection probe. The shim responds with a
    /// `ShimResponse::Ok` whose `payload` is a CBOR-encoded
    /// [`ShimHealthInfo`]. Used by `daemon.health({deep:true})` to surface
    /// subprocess uptime and request-throughput counters without spawning
    /// any new shim. Process-local (no session/target).
    Health { request_id: u64 },
}

/// Payload returned in the `ShimResponse::Ok.payload` of a successful
/// `ShimRequest::Health` round-trip. Subprocess-local introspection only —
/// the shim cannot track its own restarts (a restart is a fresh process
/// with no memory of the prior life); daemon-side bookkeeping in
/// `ShimManager::shim_state` complements this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShimHealthInfo {
    /// Wall-clock milliseconds since this subprocess started.
    pub uptime_ms: u64,
    /// Count of `ShimRequest`s served. Excludes `Shutdown` and `Health`
    /// (meta-requests) so the number reflects user work.
    pub requests_served: u64,
    /// Epoch milliseconds of the most recent non-meta request. `None`
    /// if no user request has been served yet.
    pub last_request_at_ms: Option<u64>,
}

/// Outbound frames from the shim to `loom-host::ShimManager`.
/// `cdp_event` and `log_line` are async upstream channels ;
/// they are NOT correlated to a specific request and so do not carry
/// `request_id`. `Ok` and `Error` ARE correlated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ShimResponse {
    /// Successful response. Echoes `request_id` from the originating
    /// request so the host can demultiplex.
    Ok {
        /// Echoed from the originating request — host uses this to
        /// resolve the matching oneshot.
        request_id: u64,
        session_id: Option<SessionId>,
        payload: ciborium::value::Value,
    },
    /// Stable-envelope error .
    Error {
        request_id: u64,
        session_id: Option<SessionId>,
        code: ShimErrorCode,
        detail: String,
    },
    /// Async CDP event push . The shim does NOT buffer
    /// events; they are pushed as they arrive. Daemon correlates by
    /// `target_id`. NOT request-correlated.
    CdpEvent {
        target_id: TargetId,
        message: CdpMessage,
    },
    /// Forwarded log line from `LogForwarder`. Daemon's `ShimManager`
    /// folds these into its tracing pipeline. NOT request-correlated.
    LogLine {
        level: String, // "trace" | "debug" | "info" | "warn" | "error"
        target: String,
        message: String,
    },
}

/// IPC-layer error class. Distinct from `ShimErrorCode` — these are
/// wire-protocol failures (NOT action-level failures) and are FATAL.
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("daemon closed socket (EOF) — shim must exit")]
    Eof,
    #[error("CBOR decode failure: {0}")]
    CborDecode(String),
    #[error("frame too large: {observed} > {limit}")]
    FrameTooLarge { observed: u32, limit: u32 },
    #[error("io error: {0}")]
    Io(String),
}

/// Encode a single frame to its on-wire bytes.
/// Pure helper — no I/O. Used by tests + write loop.
/// Format: `[4 BE u32 len][CBOR payload]`.
pub fn encode_frame(response: &ShimResponse) -> Result<Vec<u8>, IpcError> {
    let payload = ciborium_to_vec(response).map_err(|e| IpcError::CborDecode(e.to_string()))?;
    let len = payload.len();
    if len > MAX_FRAME_BYTES as usize {
        return Err(IpcError::FrameTooLarge {
            observed: len as u32,
            limit: MAX_FRAME_BYTES,
        });
    }
    let mut out = Vec::with_capacity(LENGTH_PREFIX_BYTES + len);
    out.extend_from_slice(&(len as u32).to_be_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Decode a single frame from its on-wire bytes. Pure helper.
/// Errors: short input, length-prefix mismatch, CBOR decode failure,
/// frame too large.
pub fn decode_frame(bytes: &[u8]) -> Result<ShimRequest, IpcError> {
    if bytes.len() < LENGTH_PREFIX_BYTES {
        return Err(IpcError::CborDecode(
            "frame shorter than length prefix".into(),
        ));
    }
    let mut len_bytes = [0u8; 4];
    len_bytes.copy_from_slice(&bytes[..LENGTH_PREFIX_BYTES]);
    let len = u32::from_be_bytes(len_bytes);
    if len > MAX_FRAME_BYTES {
        return Err(IpcError::FrameTooLarge {
            observed: len,
            limit: MAX_FRAME_BYTES,
        });
    }
    let payload_start = LENGTH_PREFIX_BYTES;
    let payload_end = payload_start
        .checked_add(len as usize)
        .ok_or_else(|| IpcError::CborDecode("length overflow".into()))?;
    if bytes.len() < payload_end {
        return Err(IpcError::CborDecode(
            "payload shorter than declared length".into(),
        ));
    }
    ciborium_from_slice(&bytes[payload_start..payload_end])
        .map_err(|e| IpcError::CborDecode(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ciborium::value::Value as CborValue;

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
            seed: Seed(42),
            epoch_ms: EpochMs(1_700_000_000_000),
            determinism_enabled: true,
        };
        let payload = ciborium_to_vec(&req).unwrap();
        let mut wire = (payload.len() as u32).to_be_bytes().to_vec();
        wire.extend_from_slice(&payload);
        let decoded = decode_frame(&wire).expect("decode");
        assert_eq!(decoded, req);
    }

    /// L0b fitness test for L8/L10's correlation-id demux logic: two
    /// requests with distinct `request_id`s must round-trip independently
    /// so the host can resolve out-of-order responses.
    #[test]
    fn two_concurrent_requests_demux_by_request_id() {
        let req_a = ShimRequest::PageNavigate {
            request_id: 1,
            session_id: 100,
            target_id: 5,
            url: "https://a.example.com".into(),
            seed: Seed(0),
            epoch_ms: EpochMs(0),
            blocklist_enabled: true,
            until: default_settle_until(),
            determinism_enabled: default_determinism_enabled(),
        };
        let req_b = ShimRequest::PageNavigate {
            request_id: 2,
            session_id: 100,
            target_id: 5,
            url: "https://b.example.com".into(),
            seed: Seed(0),
            epoch_ms: EpochMs(0),
            blocklist_enabled: true,
            until: default_settle_until(),
            determinism_enabled: default_determinism_enabled(),
        };

        let resp_b = ShimResponse::Ok {
            request_id: 2,
            session_id: Some(100),
            payload: CborValue::Text("b-result".into()),
        };
        let resp_a = ShimResponse::Ok {
            request_id: 1,
            session_id: Some(100),
            payload: CborValue::Text("a-result".into()),
        };

        // Round-trip both responses. Host extracts request_id and routes
        // to the matching oneshot — order-independent.
        for resp in [&resp_b, &resp_a] {
            let bytes = encode_frame(resp).unwrap();
            let mut len_bytes = [0u8; 4];
            len_bytes.copy_from_slice(&bytes[..4]);
            let len = u32::from_be_bytes(len_bytes) as usize;
            let decoded: ShimResponse = ciborium_from_slice(&bytes[4..4 + len]).unwrap();
            assert_eq!(&decoded, resp);
            // Extract request_id from response — this is the host-side demux key.
            let id = match decoded {
                ShimResponse::Ok { request_id, .. } => request_id,
                ShimResponse::Error { request_id, .. } => request_id,
                _ => panic!("non-correlated response shouldn't reach demux"),
            };
            assert!(id == 1 || id == 2);
        }

        // Verify request payload integrity at the shim boundary.
        for req in [&req_a, &req_b] {
            let payload = ciborium_to_vec(req).unwrap();
            let mut wire = (payload.len() as u32).to_be_bytes().to_vec();
            wire.extend_from_slice(&payload);
            let decoded = decode_frame(&wire).unwrap();
            assert_eq!(&decoded, req);
        }
    }

    #[test]
    fn decode_frame_rejects_oversize_frame() {
        let bogus = MAX_FRAME_BYTES + 1;
        let mut wire = bogus.to_be_bytes().to_vec();
        wire.push(0);
        let err = decode_frame(&wire).unwrap_err();
        match err {
            IpcError::FrameTooLarge { observed, limit } => {
                assert_eq!(observed, bogus);
                assert_eq!(limit, MAX_FRAME_BYTES);
            }
            _ => panic!("expected FrameTooLarge, got {:?}", err),
        }
    }

    #[test]
    fn shim_error_code_serialises_snake_case() {
        let s = ciborium_to_vec(&ShimErrorCode::ChromiumUnavailable).unwrap();
        let back: ShimErrorCode = ciborium_from_slice(&s).unwrap();
        assert_eq!(back, ShimErrorCode::ChromiumUnavailable);
    }

    // === wire-protocol tests (J.5) ===

    /// J.5: PageNavigate with explicit seed + epoch_ms round-trips.
    #[test]
    fn page_navigate_with_seed_round_trips() {
        let req = ShimRequest::PageNavigate {
            request_id: 7,
            session_id: 1,
            target_id: 0,
            url: "https://example.com".into(),
            seed: Seed(42),
            epoch_ms: EpochMs(1_700_000_000_000),
            blocklist_enabled: true,
            until: default_settle_until(),
            determinism_enabled: default_determinism_enabled(),
        };
        let bytes = ciborium_to_vec(&req).unwrap();
        let back: ShimRequest = ciborium_from_slice(&bytes).unwrap();
        assert_eq!(back, req);
    }

    /// J.5: Seed(0) is a real value distinct from "absent" — round-trips cleanly.
    #[test]
    fn seed_zero_is_a_real_value_on_the_wire() {
        let req = ShimRequest::PageNavigate {
            request_id: 1,
            session_id: 1,
            target_id: 0,
            url: "https://example.com".into(),
            seed: Seed(0),
            epoch_ms: EpochMs(0),
            blocklist_enabled: true,
            until: default_settle_until(),
            determinism_enabled: default_determinism_enabled(),
        };
        let bytes = ciborium_to_vec(&req).unwrap();
        let back: ShimRequest = ciborium_from_slice(&bytes).unwrap();
        assert_eq!(back, req);
    }

    /// J.5 (negative): an old-shape `PageNavigate` without `seed`/`epoch_ms`
    /// fields fails to decode rather than silently coercing to Seed(0).
    /// This is the load-bearing wire-test: we MUST surface a structured
    /// error so a future regression cannot reintroduce silent-fallback.
    #[test]
    fn old_shape_page_navigate_without_seed_fails_to_decode() {
        // Construct CBOR for a PageNavigate without seed/epoch_ms.
        let old_payload = ciborium::value::Value::Map(vec![
            (
                ciborium::value::Value::Text("kind".into()),
                ciborium::value::Value::Text("page_navigate".into()),
            ),
            (
                ciborium::value::Value::Text("request_id".into()),
                ciborium::value::Value::Integer(1.into()),
            ),
            (
                ciborium::value::Value::Text("session_id".into()),
                ciborium::value::Value::Integer(1.into()),
            ),
            (
                ciborium::value::Value::Text("target_id".into()),
                ciborium::value::Value::Integer(0.into()),
            ),
            (
                ciborium::value::Value::Text("url".into()),
                ciborium::value::Value::Text("https://example.com".into()),
            ),
        ]);
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&old_payload, &mut buf).unwrap();
        let result: Result<ShimRequest, _> = ciborium_from_slice(&buf);
        assert!(
            result.is_err(),
            "old-shape PageNavigate (no seed/epoch_ms) MUST fail to decode, \
             got: {result:?}"
        );
    }

    // === blocklist_enabled wire-protocol ===
    //
    // Unlike seed/epoch_ms (which are EXPLICIT and fail-decode on
    // absence), `blocklist_enabled` defaults to `true` to preserve
    // safe behavior under host/shim version skew. We do NOT add a
    // negative-decode test asserting that omission FAILS — that would
    // contradict the safe-default-on policy. The seed/epoch_ms negative
    // tests below remain untouched (they construct payloads omitting
    // seed, not blocklist_enabled).

    /// explicit `blocklist_enabled=false` round-trips.
    #[test]
    fn page_navigate_with_blocklist_disabled_round_trips() {
        let req = ShimRequest::PageNavigate {
            request_id: 7,
            session_id: 1,
            target_id: 0,
            url: "https://example.com".into(),
            seed: Seed(42),
            epoch_ms: EpochMs(1_700_000_000_000),
            blocklist_enabled: false,
            until: default_settle_until(),
            determinism_enabled: default_determinism_enabled(),
        };
        let bytes = ciborium_to_vec(&req).unwrap();
        let back: ShimRequest = ciborium_from_slice(&bytes).unwrap();
        assert_eq!(back, req);
    }

    /// Safe-default-on policy: a payload that omits
    /// `blocklist_enabled` (e.g. emitted by a pre-feature host) is
    /// decoded as `true` — enforcement-on. This is the deliberate
    /// asymmetry from the seed/epoch_ms explicit-field convention.
    #[test]
    fn page_navigate_without_blocklist_enabled_field_defaults_to_true() {
        // Construct a CBOR payload with all fields EXCEPT blocklist_enabled.
        let payload = ciborium::value::Value::Map(vec![
            (
                ciborium::value::Value::Text("kind".into()),
                ciborium::value::Value::Text("page_navigate".into()),
            ),
            (
                ciborium::value::Value::Text("request_id".into()),
                ciborium::value::Value::Integer(7.into()),
            ),
            (
                ciborium::value::Value::Text("session_id".into()),
                ciborium::value::Value::Integer(1.into()),
            ),
            (
                ciborium::value::Value::Text("target_id".into()),
                ciborium::value::Value::Integer(0.into()),
            ),
            (
                ciborium::value::Value::Text("url".into()),
                ciborium::value::Value::Text("https://example.com".into()),
            ),
            (
                ciborium::value::Value::Text("seed".into()),
                ciborium::value::Value::Integer(42.into()),
            ),
            (
                ciborium::value::Value::Text("epoch_ms".into()),
                ciborium::value::Value::Integer(1_700_000_000_000_i64.into()),
            ),
        ]);
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&payload, &mut buf).unwrap();
        let decoded: ShimRequest = ciborium_from_slice(&buf)
            .expect("payload missing blocklist_enabled MUST decode (safe-default-on)");
        match decoded {
            ShimRequest::PageNavigate {
                blocklist_enabled, ..
            } => {
                assert!(
                    blocklist_enabled,
                    "missing blocklist_enabled must default to true (safe-default-on)"
                );
            }
            other => panic!("expected PageNavigate, got {other:?}"),
        }
    }

    /// J.5 (negative): same for SpawnTarget.
    #[test]
    fn old_shape_spawn_target_without_seed_fails_to_decode() {
        let old_payload = ciborium::value::Value::Map(vec![
            (
                ciborium::value::Value::Text("kind".into()),
                ciborium::value::Value::Text("spawn_target".into()),
            ),
            (
                ciborium::value::Value::Text("request_id".into()),
                ciborium::value::Value::Integer(1.into()),
            ),
            (
                ciborium::value::Value::Text("session_id".into()),
                ciborium::value::Value::Integer(1.into()),
            ),
            (
                ciborium::value::Value::Text("profile".into()),
                ciborium::value::Value::Text("default".into()),
            ),
        ]);
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&old_payload, &mut buf).unwrap();
        let result: Result<ShimRequest, _> = ciborium_from_slice(&buf);
        assert!(
            result.is_err(),
            "old-shape SpawnTarget (no seed/epoch_ms) MUST fail to decode, \
             got: {result:?}"
        );
    }
}
