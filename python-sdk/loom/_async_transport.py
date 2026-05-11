"""
Asyncio Unix socket transport for the loom JSON-RPC 2.0 protocol.

Same wire protocol as ``_transport.py`` (sync) but supports concurrent
in-flight ``call()``s via a persistent reader task + id-keyed pending
map. See ``_transport.py`` for protocol documentation.

Cancellation: ``await call(...)`` integrates transparently with
``asyncio.CancelledError`` — when the caller's task is cancelled (e.g. by
``asyncio.wait_for``), the transport sends a ``request.cancel`` frame
(shielded so the cancel actually leaves the wire) and then re-raises
``CancelledError`` so structured-concurrency primitives like
``asyncio.TaskGroup`` and ``asyncio.timeout`` keep working.
"""

from __future__ import annotations

import asyncio
import json
import struct
from typing import Any

from loom._errors import LoomConnectionError, LoomRPCError
from loom._transport import _default_socket_path, _resolve_token


class AsyncLoomTransport:
    """
    Asyncio loom-rpc client transport.

    Must be created via the async factory ``AsyncLoomTransport.connect()``.
    Supports concurrent ``call()`` invocations on a single transport via
    JSON-RPC id-keyed demultiplexing.
    """

    def __init__(
        self,
        reader: asyncio.StreamReader,
        writer: asyncio.StreamWriter,
    ) -> None:
        self._reader = reader
        self._writer = writer
        # ─── MULTIPLEXER STATE ──────────────────────────────────────────
        # Monotonic id allocator + id-keyed pending map + persistent
        # _reader_task. Required for asyncio.CancelledError integration:
        # the request.cancel response can arrive on the same connection
        # while the original call() is still awaiting; per-call await-recv
        # would scramble. Mirror in typescript-sdk/src/transport.ts.
        # ────────────────────────────────────────────────────────────────
        self._next_id: int = 1
        self._pending: dict[int, asyncio.Future[Any]] = {}
        self._reader_task: asyncio.Task[None] | None = None
        self._closed = False

    @classmethod
    async def connect(
        cls, socket_path: str | None = None, token: str | None = None
    ) -> AsyncLoomTransport:
        path = socket_path or _default_socket_path()
        resolved_token = _resolve_token(token)
        try:
            reader, writer = await asyncio.open_unix_connection(path)
        except OSError as exc:
            raise LoomConnectionError(f"Cannot connect to loom socket at {path}: {exc}") from exc
        transport = cls(reader, writer)
        # Send HELLO frame — no ACK expected from server.
        hello = f"HELLO {resolved_token}".encode()
        await transport._send_frame(hello)
        # Spawn the persistent reader task BEFORE returning. The reader
        # pumps frames into the pending map for the lifetime of the
        # connection.
        transport._reader_task = asyncio.create_task(transport._reader_loop())
        return transport

    async def call(self, method: str, params: dict[str, Any]) -> Any:
        """Send a JSON-RPC 2.0 request and return the result.

        Concurrent in-flight calls are supported (id-keyed demux). On
        ``asyncio.CancelledError`` the transport fires a
        ``request.cancel`` for this call's id and re-raises so callers
        can use ``asyncio.wait_for`` / ``asyncio.timeout`` / TaskGroup
        as normal.
        """
        if self._closed:
            raise LoomConnectionError("Transport is closed")
        request_id = self._next_id
        self._next_id += 1
        loop = asyncio.get_running_loop()
        future: asyncio.Future[Any] = loop.create_future()
        self._pending[request_id] = future

        request = json.dumps(
            {"jsonrpc": "2.0", "method": method, "params": params, "id": request_id}
        )
        try:
            await self._send_frame(request.encode("utf-8"))
        except Exception:
            self._pending.pop(request_id, None)
            raise

        try:
            response = await future
        except asyncio.CancelledError:
            # Caller's task was cancelled (e.g. via asyncio.wait_for).
            # Best-effort: fire-and-forget the cancel envelope while
            # shielded, then drop our pending entry and re-raise so
            # TaskGroup / timeout machinery keeps working.
            self._pending.pop(request_id, None)
            try:
                await asyncio.shield(self._send_cancel(request_id))
            except Exception:
                # Cleanup failure must not mask the original
                # CancelledError. The cancel is best-effort anyway.
                pass
            raise

        if "error" in response:
            raise LoomRPCError._from_envelope(response)
        return response.get("result")

    async def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        if self._reader_task is not None:
            self._reader_task.cancel()
            try:
                await self._reader_task
            except (asyncio.CancelledError, Exception):
                pass
        # Reject any callers still waiting.
        for fut in list(self._pending.values()):
            if not fut.done():
                fut.set_exception(LoomConnectionError("Transport closed"))
        self._pending.clear()
        self._writer.close()
        try:
            await self._writer.wait_closed()
        except OSError:
            pass

    async def __aenter__(self) -> AsyncLoomTransport:
        return self

    async def __aexit__(self, *_: Any) -> None:
        await self.close()

    # ------------------------------------------------------------------ #
    # Internal framing + reader

    async def _send_frame(self, data: bytes) -> None:
        header = struct.pack(">I", len(data))
        self._writer.write(header + data)
        await self._writer.drain()

    async def _send_cancel(self, target_id: int) -> None:
        """Fire-and-forget ``request.cancel`` for an in-flight call.

        The daemon's cancel handler is idempotent and returns
        ``{cancelled: bool}`` on a fresh request id; we silently drop
        that response in the reader loop since we don't register a
        pending entry for the cancel envelope itself.
        """
        if self._closed:
            return
        cancel_id = self._next_id
        self._next_id += 1
        env = json.dumps(
            {
                "jsonrpc": "2.0",
                "method": "request.cancel",
                "params": {"request_id": target_id},
                "id": cancel_id,
            }
        )
        try:
            await self._send_frame(env.encode("utf-8"))
        except Exception:
            # If the wire is wedged, the original future has already
            # been popped and we're returning from a cancellation anyway.
            pass

    async def _reader_loop(self) -> None:
        """Persistent task: pump complete frames into self._pending."""
        try:
            while True:
                header = await self._reader.readexactly(4)
                (length,) = struct.unpack(">I", header)
                payload = await self._reader.readexactly(length)
                try:
                    envelope = json.loads(payload)
                except json.JSONDecodeError:
                    continue  # malformed frame — drop
                rid = envelope.get("id")
                if not isinstance(rid, int):
                    continue  # notification or malformed
                fut = self._pending.pop(rid, None)
                if fut is None:
                    # Unknown id — likely a request.cancel response we
                    # didn't register a pending entry for. Silently drop.
                    continue
                if not fut.done():
                    fut.set_result(envelope)
        except asyncio.IncompleteReadError:
            err = LoomConnectionError("Connection closed by daemon mid-frame")
            for fut in list(self._pending.values()):
                if not fut.done():
                    fut.set_exception(err)
            self._pending.clear()
        except asyncio.CancelledError:
            raise
        except Exception as exc:
            err = LoomConnectionError(f"Reader loop error: {exc}")
            for fut in list(self._pending.values()):
                if not fut.done():
                    fut.set_exception(err)
            self._pending.clear()
