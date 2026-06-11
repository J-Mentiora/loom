"""
Transport-layer tests: framing, HELLO auth, call/response, error handling.
"""
from __future__ import annotations

import json
import socket
import struct
import threading
import time
from pathlib import Path

import pytest

import loom
from loom._errors import LoomRPCError
from loom._transport import LoomTransport
from tests.conftest import MockDaemon


def test_length_delimited_framing(daemon: MockDaemon):
    """Round-trip: send a framed request and receive a framed response."""
    t = LoomTransport(str(daemon.socket_path), daemon.token)
    result = t.call("session.list", {})
    assert isinstance(result, list)
    t.close()


def test_hello_auth_succeeds(daemon: MockDaemon):
    """Correct token accepted — call returns valid result."""
    t = LoomTransport(str(daemon.socket_path), daemon.token)
    result = t.call("session.create", {"profile": "default", "network_mode": "live", "capture": True})
    assert "session_id" in result
    t.close()


def test_hello_auth_fails_wrong_token(daemon: MockDaemon):
    """Wrong token: typed LoomRPCError(protocol_auth_required) raised at
    construction — the handshake probe surfaces the rejection immediately
    instead of deferring it to the first call."""
    with pytest.raises(LoomRPCError) as exc_info:
        LoomTransport(str(daemon.socket_path), "wrong-token")
    assert exc_info.value.code == "protocol_auth_required"


def test_method_not_found_raises_error(daemon: MockDaemon):
    """Unknown method returns error envelope → LoomRPCError."""
    t = LoomTransport(str(daemon.socket_path), daemon.token)
    with pytest.raises(LoomRPCError) as exc_info:
        t.call("no.such.method", {})
    assert exc_info.value.code == "method_not_found"
    t.close()


def test_context_manager(daemon: MockDaemon):
    """LoomTransport works as a context manager."""
    with LoomTransport(str(daemon.socket_path), daemon.token) as t:
        result = t.call("session.list", {})
        assert isinstance(result, list)


def test_multiple_calls_on_same_connection(daemon: MockDaemon):
    """Multiple successive calls on the same connection all succeed."""
    with LoomTransport(str(daemon.socket_path), daemon.token) as t:
        r1 = t.call("session.create", {"profile": "default", "network_mode": "live", "capture": True})
        r2 = t.call("session.create", {"profile": "default", "network_mode": "live", "capture": True})
        r3 = t.call("session.list", {})
    assert r1["session_id"] != r2["session_id"]
    ids = {s["session_id"] for s in r3}
    assert r1["session_id"] in ids
    assert r2["session_id"] in ids


# ─── bare daemon error frames (the real HELLO auth-failure wire shape) ────
# On HELLO auth failure the real daemon sends a BARE serialized JsonRpcError
# — {"code": ..., "message": ...} with NO {"error": ...} wrapper and NO id
# (loom-rpc connection_handler::send_error) — then closes the connection.
# The MockDaemon emits that exact shape, so these tests exercise the true
# wire contract.


def test_bare_error_frame_surfaces_message(daemon: MockDaemon):
    """The bare auth-failure frame parses into a typed LoomRPCError with the
    daemon's code AND message, not a generic connection error — raised at
    construction by the handshake."""
    with pytest.raises(LoomRPCError) as exc_info:
        LoomTransport(str(daemon.socket_path), "wrong-token")
    assert exc_info.value.code == "protocol_auth_required"
    assert "token mismatch" in exc_info.value.message


async def test_async_transport_recognizes_bare_auth_frame(daemon: MockDaemon):
    """The async reader loop must recognize the id-less bare error frame and
    fail the handshake probe with the typed auth error at connect()."""
    from loom._async_transport import AsyncLoomTransport

    with pytest.raises(LoomRPCError) as exc_info:
        await AsyncLoomTransport.connect(str(daemon.socket_path), "wrong-token")
    assert exc_info.value.code == "protocol_auth_required"


# ─── HELLO ack handshake (connection-protocol redesign) ───────────────────
# New clients pipeline a daemon.hello probe after HELLO. New daemons ack it
# with {"hello": "ok", "server": <version>}; pre-ack daemons (<= 0.10.x)
# answer a normal method_not_found envelope — both prove auth passed. The
# default MockDaemon has no daemon.hello handler, so every other test in
# this file already exercises the old-daemon path.


def test_handshake_old_daemon_method_not_found_authenticates(daemon: MockDaemon):
    """Old-daemon compat: a method_not_found reply to the probe counts as
    authenticated and calls proceed normally."""
    with LoomTransport(str(daemon.socket_path), daemon.token) as t:
        assert isinstance(t.call("session.list", {}), list)


def test_handshake_new_daemon_ack_authenticates(daemon: MockDaemon):
    """New-daemon path: the explicit {"hello": "ok", "server": ...} ack
    resolves the handshake and calls proceed normally."""
    daemon.register_handler("daemon.hello", lambda _p: {"hello": "ok", "server": "test"})
    with LoomTransport(str(daemon.socket_path), daemon.token) as t:
        assert isinstance(t.call("session.list", {}), list)


async def test_async_handshake_new_daemon_ack_authenticates(daemon: MockDaemon):
    from loom._async_transport import AsyncLoomTransport

    daemon.register_handler("daemon.hello", lambda _p: {"hello": "ok", "server": "test"})
    t = await AsyncLoomTransport.connect(str(daemon.socket_path), daemon.token)
    try:
        assert isinstance(await t.call("session.list", {}), list)
    finally:
        await t.close()


def test_handshake_wedged_daemon_times_out(monkeypatch):
    """A daemon that accepts the connection but never answers the probe must
    fail construction in bounded time with a connection error — silence is a
    wedged daemon, not an accepted HELLO."""
    import tempfile

    import loom._transport as transport_mod

    monkeypatch.setattr(transport_mod, "_HANDSHAKE_TIMEOUT_S", 0.3)

    # tempfile.mkdtemp keeps the AF_UNIX path under the 104-byte cap
    # (pytest's tmp_path can exceed it on macOS).
    sock_path = Path(tempfile.mkdtemp()) / "wedged.sock"
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(str(sock_path))
    server.listen(1)

    def _serve():
        conn, _ = server.accept()
        # Read frames forever; never answer.
        try:
            while conn.recv(4096):
                pass
        except OSError:
            pass

    thread = threading.Thread(target=_serve, daemon=True)
    thread.start()
    try:
        from loom._errors import LoomConnectionError

        with pytest.raises(LoomConnectionError, match="handshake"):
            LoomTransport(str(sock_path), "any-token")
    finally:
        server.close()


def test_from_bare_frame_recognizes_daemon_auth_shape():
    err = LoomRPCError._from_bare_frame(
        {"code": "protocol_auth_required", "message": "token mismatch"}
    )
    assert isinstance(err, LoomRPCError)
    assert err.code == "protocol_auth_required"
    assert err.message == "token mismatch"


def test_from_bare_frame_rejects_non_bare_shapes():
    """Frames carrying id/result/error keys (normal envelopes) or non-string
    code/message are NOT treated as bare daemon errors."""
    assert LoomRPCError._from_bare_frame({"id": 1, "result": {}}) is None
    assert LoomRPCError._from_bare_frame({"id": 1, "error": {"code": "x", "message": "y"}}) is None
    assert LoomRPCError._from_bare_frame({"error": {"code": "x", "message": "y"}}) is None
    assert LoomRPCError._from_bare_frame({"result": None}) is None
    assert LoomRPCError._from_bare_frame({"code": 401, "message": "nope"}) is None
    assert LoomRPCError._from_bare_frame({"code": "x"}) is None
    assert LoomRPCError._from_bare_frame("not a dict") is None
