"""
Asyncio Unix socket transport for the loom JSON-RPC 2.0 protocol.

Same protocol as ``_transport.py`` (sync) but uses ``asyncio.open_unix_connection``.
See ``_transport.py`` for protocol documentation.
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
    """

    def __init__(
        self,
        reader: asyncio.StreamReader,
        writer: asyncio.StreamWriter,
    ) -> None:
        self._reader = reader
        self._writer = writer

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
        return transport

    async def call(self, method: str, params: dict[str, Any]) -> Any:
        """Send a JSON-RPC 2.0 request and return the result."""
        request = json.dumps({"jsonrpc": "2.0", "method": method, "params": params, "id": 1})
        await self._send_frame(request.encode("utf-8"))
        response_bytes = await self._recv_frame()
        response = json.loads(response_bytes)
        if "error" in response:
            raise LoomRPCError._from_envelope(response)
        return response.get("result")

    async def close(self) -> None:
        self._writer.close()
        try:
            await self._writer.wait_closed()
        except OSError:
            pass

    async def __aenter__(self) -> AsyncLoomTransport:
        return self

    async def __aexit__(self, *_: Any) -> None:
        await self.close()

    async def _send_frame(self, data: bytes) -> None:
        header = struct.pack(">I", len(data))
        self._writer.write(header + data)
        await self._writer.drain()

    async def _recv_frame(self) -> bytes:
        header = await self._recv_exact(4)
        (length,) = struct.unpack(">I", header)
        return await self._recv_exact(length)

    async def _recv_exact(self, n: int) -> bytes:
        try:
            data = await self._reader.readexactly(n)
        except asyncio.IncompleteReadError as exc:
            raise LoomConnectionError("Connection closed by daemon mid-frame") from exc
        return data
