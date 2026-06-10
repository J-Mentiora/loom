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
    """Wrong token: LoomRPCError(protocol_auth_required) raised on first call."""
    t = LoomTransport(str(daemon.socket_path), "wrong-token")
    with pytest.raises(LoomRPCError) as exc_info:
        t.call("session.list", {})
    assert exc_info.value.code == "protocol_auth_required"
    t.close()


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
    daemon's code AND message, not a generic connection error."""
    t = LoomTransport(str(daemon.socket_path), "wrong-token")
    with pytest.raises(LoomRPCError) as exc_info:
        t.call("session.list", {})
    assert exc_info.value.code == "protocol_auth_required"
    assert "token mismatch" in exc_info.value.message
    t.close()


def test_auth_failure_latched_for_subsequent_calls(daemon: MockDaemon):
    """After the daemon closes post-auth-failure, later calls must re-raise
    the same typed error — not BrokenPipeError or a generic close error."""
    t = LoomTransport(str(daemon.socket_path), "wrong-token")
    with pytest.raises(LoomRPCError):
        t.call("session.list", {})
    with pytest.raises(LoomRPCError) as exc_info:
        t.call("session.list", {})
    assert exc_info.value.code == "protocol_auth_required"
    t.close()


async def test_async_transport_recognizes_bare_auth_frame(daemon: MockDaemon):
    """The async reader loop must recognize the id-less bare error frame and
    fail the pending call with the typed auth error."""
    from loom._async_transport import AsyncLoomTransport

    t = await AsyncLoomTransport.connect(str(daemon.socket_path), "wrong-token")
    try:
        with pytest.raises(LoomRPCError) as exc_info:
            await t.call("session.list", {})
        assert exc_info.value.code == "protocol_auth_required"
        # Latched: subsequent calls surface the same typed error.
        with pytest.raises(LoomRPCError) as exc_info2:
            await t.call("session.list", {})
        assert exc_info2.value.code == "protocol_auth_required"
    finally:
        await t.close()


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
