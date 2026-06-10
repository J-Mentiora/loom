"""
Synchronous Unix socket transport for the loom JSON-RPC 2.0 protocol.

Wire protocol:
- Framing: 4-byte big-endian uint32 length prefix + payload bytes.
- HELLO handshake: first frame sent MUST be ``HELLO {token}`` (UTF-8,
  no trailing newline). Server sends NO ACK on success. On auth failure,
  server sends one error frame then closes.
- All subsequent frames are JSON-RPC 2.0 request/response pairs.

Token discovery order (if ``token`` not supplied explicitly):
1. ``~/.loom/loom.token`` file (daemon writes this at startup).
2. ``LOOM_TOKEN`` environment variable.
3. Raises ``LoomTokenError`` if neither found.

Socket path defaults (if ``socket_path`` not supplied):
- macOS: ``~/Library/Caches/loom/loom.sock``
- Linux: ``$XDG_RUNTIME_DIR/loom.sock``
"""

from __future__ import annotations

import json
import os
import platform
import socket
import struct
from pathlib import Path
from typing import Any, NoReturn

from loom._errors import LoomConnectionError, LoomRPCError, LoomTokenError


def _default_socket_path() -> str:
    if platform.system() == "Darwin":
        return str(Path.home() / "Library" / "Caches" / "loom" / "loom.sock")
    xdg = os.environ.get("XDG_RUNTIME_DIR", "/tmp")
    return str(Path(xdg) / "loom.sock")


def _resolve_token(token: str | None) -> str:
    if token is not None:
        return token
    token_file = Path.home() / ".loom" / "loom.token"
    if token_file.exists():
        return token_file.read_text().strip()
    env_token = os.environ.get("LOOM_TOKEN")
    if env_token:
        return env_token
    raise LoomTokenError(
        "No loom token found. Pass token= explicitly, set LOOM_TOKEN env var, "
        "or ensure the daemon has written ~/.loom/loom.token."
    )


class LoomTransport:
    """
    Synchronous loom-rpc client transport.

    Opens a Unix domain socket, sends the HELLO handshake frame, then
    allows repeated ``call()`` invocations. Each ``call()`` sends one
    JSON-RPC request frame and reads one response frame.
    """

    def __init__(self, socket_path: str | None = None, token: str | None = None) -> None:
        self._path = socket_path or _default_socket_path()
        self._token = _resolve_token(token)
        # Monotonic id allocator. The sync transport is single-in-flight
        # by design (one call() at a time on one instance); the allocator
        # exists so that JSON-RPC `id` values are unique-per-connection,
        # which the daemon's `request.cancel` correlation requires for any
        # future concurrent path. For cancellable / concurrent use, see
        # AsyncLoomTransport.
        self._next_id: int = 1
        # Latched daemon-level protocol error (e.g. the HELLO auth-failure
        # frame). Once set, every subsequent call() re-raises it instead of
        # surfacing a generic connection error.
        self._daemon_error: LoomRPCError | None = None
        self._sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            self._sock.connect(self._path)
        except OSError as exc:
            self._sock.close()
            raise LoomConnectionError(
                f"Cannot connect to loom socket at {self._path}: {exc}"
            ) from exc
        # Send HELLO frame — no ACK expected from server.
        # Auth failure surfaces as an error envelope on the first call().
        hello = f"HELLO {self._token}".encode()
        self._send_frame(hello)

    # ------------------------------------------------------------------ #
    # Public API

    def call(self, method: str, params: dict[str, Any]) -> Any:
        """
        Send a JSON-RPC 2.0 request and return the ``result`` value.

        Raises ``LoomRPCError`` if the response contains an ``"error"`` key,
        or if the daemon sent a bare ``JsonRpcError`` frame (the HELLO
        auth-failure shape — see ``LoomRPCError._from_bare_frame``).

        This transport is single-in-flight: ``call()`` blocks until the
        response arrives. For cancellable or concurrent use, use
        :class:`AsyncLoomTransport`.
        """
        if self._daemon_error is not None:
            raise self._daemon_error
        request_id = self._next_id
        self._next_id += 1
        request = json.dumps(
            {"jsonrpc": "2.0", "method": method, "params": params, "id": request_id}
        )
        try:
            self._send_frame(request.encode("utf-8"))
        except OSError as exc:
            # The daemon may have already sent a terminal error frame (e.g.
            # the HELLO auth-failure frame) and closed the connection, so the
            # write fails before we ever read it. Salvage that frame so the
            # typed error surfaces instead of a bare BrokenPipeError.
            self._raise_connection_lost(exc)
        try:
            response_bytes = self._recv_frame()
        except LoomConnectionError:
            raise
        except OSError as exc:
            raise LoomConnectionError(f"Connection to daemon lost: {exc}") from exc
        response = json.loads(response_bytes)
        if "error" in response:
            raise LoomRPCError._from_envelope(response)
        bare = LoomRPCError._from_bare_frame(response)
        if bare is not None:
            # Daemon-level protocol error: on HELLO auth failure the daemon
            # sends a BARE serialized JsonRpcError — {"code", "message"} with
            # no {"error": ...} wrapper and no id — then closes. Latch it so
            # later calls re-raise the same typed error.
            self._daemon_error = bare
            raise bare
        return response.get("result")

    def _raise_connection_lost(self, cause: OSError) -> NoReturn:
        """Send failed: drain a pending daemon error frame if there is one,
        otherwise raise a connection error wrapping ``cause``."""
        try:
            pending = json.loads(self._recv_frame())
        except Exception:
            pending = None
        bare = LoomRPCError._from_bare_frame(pending)
        if bare is not None:
            self._daemon_error = bare
            raise bare from cause
        raise LoomConnectionError(f"Connection to daemon lost: {cause}") from cause

    def close(self) -> None:
        """Close the socket connection."""
        try:
            self._sock.close()
        except OSError:
            pass

    def __enter__(self) -> LoomTransport:
        return self

    def __exit__(self, *_: Any) -> None:
        self.close()

    # ------------------------------------------------------------------ #
    # Internal framing

    def _send_frame(self, data: bytes) -> None:
        header = struct.pack(">I", len(data))
        self._sock.sendall(header + data)

    def _recv_frame(self) -> bytes:
        header = self._recv_exact(4)
        (length,) = struct.unpack(">I", header)
        return self._recv_exact(length)

    def _recv_exact(self, n: int) -> bytes:
        buf = b""
        while len(buf) < n:
            chunk = self._sock.recv(n - len(buf))
            if not chunk:
                raise LoomConnectionError("Connection closed by daemon mid-frame")
            buf += chunk
        return buf
