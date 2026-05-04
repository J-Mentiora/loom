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
