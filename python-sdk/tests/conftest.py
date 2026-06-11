"""
MockDaemon fixture — implements the loom-rpc framed Unix socket protocol:

Protocol:
1. Server listens on a Unix socket.
2. Client connects and sends first frame: `HELLO {token}` (UTF-8 bytes,
   no trailing newline), length-prefixed with 4-byte big-endian integer.
3. Server reads HELLO frame. On token mismatch, sends error frame + closes.
   On success, silently enters Authenticated state (no ACK sent).
4. Server loops: read framed JSON-RPC 2.0 request → dispatch → write framed
   JSON response.

Frame format:
    [4 bytes big-endian uint32 = length][length bytes of payload]
"""
from __future__ import annotations

import json
import os
import socket
import struct
import tempfile
import threading
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest


DAEMON_TOKEN = "test-token-abc123"


def _recv_frame(conn: socket.socket) -> bytes:
    header = b""
    while len(header) < 4:
        chunk = conn.recv(4 - len(header))
        if not chunk:
            raise ConnectionError("socket closed before frame header")
        header += chunk
    (length,) = struct.unpack(">I", header)
    data = b""
    while len(data) < length:
        chunk = conn.recv(length - len(data))
        if not chunk:
            raise ConnectionError("socket closed mid-frame")
        data += chunk
    return data


def _send_frame(conn: socket.socket, payload: bytes) -> None:
    header = struct.pack(">I", len(payload))
    conn.sendall(header + payload)


def _make_error_envelope(code: str, message: str, request_id: int | None = None) -> bytes:
    env: dict[str, Any] = {"error": {"code": code, "message": message}}
    if request_id is not None:
        env["id"] = request_id
    return json.dumps(env).encode()


def _make_bare_error_frame(code: str, message: str) -> bytes:
    """The real daemon's HELLO-failure shape: a BARE serialized JsonRpcError —
    ``{"code", "message"}`` with NO ``{"error": ...}`` wrapper and NO ``id``
    (loom-rpc ``connection_handler::send_error``; pinned by the daemon's own
    integration tests asserting top-level ``resp["code"]``)."""
    return json.dumps({"code": code, "message": message}).encode()


def _make_result(result: Any, request_id: int | None = None) -> bytes:
    env: dict[str, Any] = {"result": result}
    if request_id is not None:
        env["id"] = request_id
    return json.dumps(env).encode()


SCHEMA_REGISTRY = {
    "methods": [
        {
            "method": "session.create",
            "request": {
                "type": "object",
                "properties": {
                    "profile": {"type": "string"},
                    "network_mode": {"type": "string"},
                    "capture": {"type": "boolean"},
                },
            },
            "response": {
                "type": "object",
                "properties": {
                    "session_id": {"type": "string"},
                    "status": {"type": "string"},
                    "created_at_ms": {"type": "integer"},
                },
            },
        },
        {
            "method": "session.list",
            "request": {"type": "object", "properties": {}},
            "response": {"type": "array"},
        },
        {
            "method": "rpc.schemas",
            "request": {"type": "object", "properties": {}},
            "response": {
                "type": "object",
                "properties": {
                    "methods": {"type": "array"},
                    "source_wit_sha256": {"type": "string"},
                },
            },
        },
    ],
    "source_wit_sha256": "aabbccdd" * 8,
}


class MockDaemon:
    """
    Minimal loom-rpc daemon for tests.

    Starts a Unix socket server in a background thread.
    Callers may register custom method handlers via `register_handler`.
    After test use, call `stop()`.
    """

    def __init__(self, token: str = DAEMON_TOKEN) -> None:
        self.token = token
        self._tmpdir = tempfile.mkdtemp()
        self.socket_path = Path(self._tmpdir) / "loom.sock"
        self.token_file = Path(self._tmpdir) / "loom.token"
        self.token_file.write_text(self.token)
        self._handlers: dict[str, Callable[[dict], Any]] = {}
        self._server_sock: socket.socket | None = None
        self._thread: threading.Thread | None = None
        self._stop_event = threading.Event()
        self.sessions: dict[str, dict] = {}
        self._session_counter = 0
        self._register_defaults()

    # ------------------------------------------------------------------ #
    # Public API

    def start(self) -> None:
        self._server_sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._server_sock.bind(str(self.socket_path))
        self._server_sock.listen(5)
        self._server_sock.settimeout(0.1)
        self._thread = threading.Thread(target=self._serve, daemon=True)
        self._thread.start()

    def stop(self) -> None:
        self._stop_event.set()
        if self._server_sock:
            self._server_sock.close()
        if self._thread:
            self._thread.join(timeout=2)
        import shutil
        shutil.rmtree(self._tmpdir, ignore_errors=True)

    def register_handler(self, method: str, fn: Callable[[dict], Any]) -> None:
        self._handlers[method] = fn

    # ------------------------------------------------------------------ #
    # Internal

    def _register_defaults(self) -> None:
        def _session_create(params: dict) -> dict:
            self._session_counter += 1
            sid = f"01TEST{self._session_counter:020d}"
            self.sessions[sid] = {
                "session_id": sid,
                "status": "active",
                "created_at_ms": int(time.time() * 1000),
            }
            return self.sessions[sid].copy()

        def _session_list(_: dict) -> list:
            return list(self.sessions.values())

        def _rpc_schemas(_: dict) -> dict:
            return SCHEMA_REGISTRY

        def _session_close(params: dict) -> dict:
            sid = params.get("session_id", "")
            if sid in self.sessions:
                self.sessions[sid]["status"] = "closed"
                return self.sessions[sid].copy()
            return {"session_id": sid, "status": "closed", "created_at_ms": 0}

        def _session_abort(params: dict) -> dict:
            sid = params.get("session_id", "")
            if sid in self.sessions:
                self.sessions[sid]["status"] = "aborted"
                return self.sessions[sid].copy()
            return {"session_id": sid, "status": "aborted", "created_at_ms": 0}

        self._handlers["session.create"] = _session_create
        self._handlers["session.list"] = _session_list
        self._handlers["session.close"] = _session_close
        self._handlers["session.abort"] = _session_abort
        self._handlers["rpc.schemas"] = _rpc_schemas

    def _serve(self) -> None:
        assert self._server_sock is not None
        while not self._stop_event.is_set():
            try:
                conn, _ = self._server_sock.accept()
            except (socket.timeout, OSError):
                continue
            t = threading.Thread(target=self._handle_connection, args=(conn,), daemon=True)
            t.start()

    def _handle_connection(self, conn: socket.socket) -> None:
        try:
            # AwaitingHello: read first frame
            conn.settimeout(5.0)
            frame = _recv_frame(conn)
            hello = frame.decode("utf-8")
            parts = hello.split(" ", 1)
            if len(parts) != 2 or parts[0] != "HELLO":
                _send_frame(
                    conn, _make_bare_error_frame("protocol_auth_required", "malformed hello")
                )
                return
            if parts[1] != self.token:
                _send_frame(
                    conn, _make_bare_error_frame("protocol_auth_required", "token mismatch")
                )
                return
            # Authenticated: request dispatch loop
            conn.settimeout(5.0)
            while not self._stop_event.is_set():
                try:
                    req_bytes = _recv_frame(conn)
                except (ConnectionError, OSError):
                    break
                try:
                    req = json.loads(req_bytes)
                except json.JSONDecodeError:
                    _send_frame(conn, _make_error_envelope("protocol_malformed", "invalid JSON"))
                    continue
                method = req.get("method", "")
                params = req.get("params") or {}
                req_id = req.get("id")
                handler = self._handlers.get(method)
                if handler is None:
                    _send_frame(
                        conn,
                        _make_error_envelope(
                            "method_not_found", f"unknown method: {method}", req_id
                        ),
                    )
                    continue
                try:
                    result = handler(params)
                    _send_frame(conn, _make_result(result, req_id))
                except Exception as exc:
                    _send_frame(conn, _make_error_envelope("internal_error", str(exc), req_id))
        except Exception:
            pass
        finally:
            conn.close()


@pytest.fixture
def daemon() -> MockDaemon:
    d = MockDaemon()
    d.start()
    yield d
    d.stop()
