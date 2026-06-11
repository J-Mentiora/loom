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
from loom._transport import _HANDSHAKE_TIMEOUT_S, _default_socket_path, _resolve_token


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
        # `_closed` gates call(): set by close() AND by the reader-loop death
        # latch (_mark_dead) so a post-hangup call() fails fast. `_torn_down`
        # tracks the one-shot writer/reader teardown so close() stays
        # idempotent and still runs even when _mark_dead set _closed first.
        self._closed = False
        self._torn_down = False
        # Latched daemon-level protocol error (e.g. the HELLO auth-failure
        # frame — a bare JsonRpcError with no id, after which the daemon
        # closes). Once set, every subsequent call() re-raises it instead
        # of surfacing a generic connection error.
        self._daemon_error: LoomRPCError | None = None
        # Latched terminal connection error. Set when the reader loop exits
        # for ANY reason (EOF / half-close / framing error): without it a
        # later call() — after the reader died — would write into the
        # still-open write side and await a future no reader will ever
        # resolve, hanging forever. Once set, call() fails immediately.
        # Mirrors the TS transport's dead-latch fix (#163).
        self._terminal_err: LoomConnectionError | None = None

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
        # Send HELLO + the pipelined daemon.hello ack probe (probe id 0
        # is reserved for the handshake; call() ids start at 1). New
        # daemons ack with {"hello": "ok", "server": <version>}; pre-ack
        # daemons (<= 0.10.x) answer a method_not_found envelope —
        # either reply proves auth passed (rejections are a bare
        # JsonRpcError frame + close, which the reader latches typed).
        hello = f"HELLO {resolved_token}".encode()
        await transport._send_frame(hello)
        loop = asyncio.get_running_loop()
        ack_future: asyncio.Future[Any] = loop.create_future()
        transport._pending[0] = ack_future
        probe = json.dumps(
            {"jsonrpc": "2.0", "id": 0, "method": "daemon.hello", "params": {}}
        )
        await transport._send_frame(probe.encode("utf-8"))
        # Spawn the persistent reader task BEFORE awaiting the ack. The
        # reader pumps frames into the pending map for the lifetime of
        # the connection.
        transport._reader_task = asyncio.create_task(transport._reader_loop())
        try:
            # Any id-correlated envelope = authenticated (ack result OR
            # an old daemon's method_not_found error envelope). A bare
            # auth-rejection frame fails this future with the typed
            # LoomRPCError; EOF fails it with LoomConnectionError.
            await asyncio.wait_for(ack_future, timeout=_HANDSHAKE_TIMEOUT_S)
        except asyncio.TimeoutError:
            await transport.close()
            raise LoomConnectionError(
                "Daemon did not answer the HELLO handshake within "
                f"{_HANDSHAKE_TIMEOUT_S:g}s (wedged daemon?)"
            ) from None
        except (LoomRPCError, LoomConnectionError):
            await transport.close()
            raise
        return transport

    async def call(self, method: str, params: dict[str, Any]) -> Any:
        """Send a JSON-RPC 2.0 request and return the result.

        Concurrent in-flight calls are supported (id-keyed demux). On
        ``asyncio.CancelledError`` the transport fires a
        ``request.cancel`` for this call's id and re-raises so callers
        can use ``asyncio.wait_for`` / ``asyncio.timeout`` / TaskGroup
        as normal.
        """
        if self._daemon_error is not None:
            raise self._daemon_error
        if self._terminal_err is not None:
            # The reader loop already died (daemon hangup / half-close). Fail
            # fast and typed instead of writing into a dead connection and
            # awaiting a future nothing will ever resolve.
            raise self._terminal_err
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
        except OSError as exc:
            self._pending.pop(request_id, None)
            # The daemon may have closed after sending a terminal error
            # frame (HELLO auth failure); prefer the typed error the
            # reader latched over a raw BrokenPipeError.
            if self._daemon_error is not None:
                raise self._daemon_error from exc
            if self._terminal_err is not None:
                raise self._terminal_err from exc
            raise LoomConnectionError(f"Connection to daemon lost: {exc}") from exc
        except Exception:
            self._pending.pop(request_id, None)
            raise

        # Close the race window: the reader may have died (and latched a
        # terminal error) AFTER our guard checks but while our write was in
        # flight — on a clean half-close the write succeeds with no error.
        # Without this the future below would never be resolved by any reader.
        if self._terminal_err is not None or self._daemon_error is not None:
            self._pending.pop(request_id, None)
            raise self._daemon_error or self._terminal_err  # type: ignore[misc]

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
        # Idempotent on the actual writer/reader teardown — note _closed may
        # already be True because the reader-loop death latch (_mark_dead) set
        # it, in which case the socket still needs an explicit close here.
        if self._torn_down:
            return
        self._torn_down = True
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
                    # No id. The daemon's HELLO auth-failure frame is a BARE
                    # serialized JsonRpcError — {"code", "message"} with no
                    # {"error": ...} wrapper and no id — sent just before it
                    # closes the connection. Latch it and fail ALL pending
                    # callers so the typed error surfaces instead of a
                    # generic "connection closed" error.
                    bare = LoomRPCError._from_bare_frame(envelope)
                    if bare is not None:
                        self._daemon_error = bare
                        for fut in list(self._pending.values()):
                            if not fut.done():
                                fut.set_exception(bare)
                        self._pending.clear()
                    continue  # notification or malformed
                fut = self._pending.pop(rid, None)
                if fut is None:
                    # Unknown id — likely a request.cancel response we
                    # didn't register a pending entry for. Silently drop.
                    continue
                if not fut.done():
                    fut.set_result(envelope)
        except asyncio.IncompleteReadError:
            # EOF / half-close. Prefer the latched daemon error (e.g. auth
            # failure) over a generic close error — the daemon closes right
            # after sending it.
            close_err = LoomConnectionError("Connection closed by daemon mid-frame")
            self._mark_dead(close_err)
        except asyncio.CancelledError:
            raise
        except Exception as exc:
            self._mark_dead(LoomConnectionError(f"Reader loop error: {exc}"))

    def _mark_dead(self, close_err: LoomConnectionError) -> None:
        """Latch a terminal error and fail every pending future.

        Called from the reader-loop exit handlers. Marks the transport closed
        and stashes ``close_err`` so a LATER call() — after the reader has
        died — fails fast and typed instead of writing into a dead connection
        and awaiting a future no reader will ever resolve (the half-close hang;
        audit 2026-06-10, F67). A latched daemon protocol error (auth failure)
        takes precedence as the surfaced exception.
        """
        self._closed = True
        self._terminal_err = close_err
        err: Exception = self._daemon_error or close_err
        for fut in list(self._pending.values()):
            if not fut.done():
                fut.set_exception(err)
        self._pending.clear()
