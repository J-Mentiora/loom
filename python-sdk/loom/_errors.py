"""
Error types for the loom Python SDK.

LoomErrorCode values mirror ``loom_shared::LoomErrorCode::as_wire``
(loom-shared/src/error_format.rs). All snake_case strings are stable wire
values (BC-RPC-03). ``LoomRPCError.code`` is kept as the raw wire string;
parse it with ``LoomErrorCode(err.code)``, which degrades unknown codes to
``LoomErrorCode.internal`` instead of raising.
"""

from typing import Any


class LoomError(Exception):
    """Base class for all loom SDK errors."""


class LoomRPCError(LoomError):
    """
    JSON-RPC error envelope received from the daemon.

    Maps to ``{"error": {"code": <code>, "message": <message>, "data": <data>}}``.
    """

    def __init__(self, code: str, message: str, data: Any = None) -> None:
        super().__init__(f"{code}: {message}")
        self.code = code
        self.message = message
        self.data = data

    @classmethod
    def _from_envelope(cls, envelope: dict) -> "LoomRPCError":
        err = envelope.get("error", {})
        return cls(
            code=err.get("code", "internal_error"),
            message=err.get("message", "unknown error"),
            data=err.get("data"),
        )

    @classmethod
    def _from_bare_frame(cls, frame: Any) -> "LoomRPCError | None":
        """Recognize the daemon's BARE ``JsonRpcError`` frame.

        On HELLO auth failure the daemon serializes the ``JsonRpcError``
        struct directly — ``{"code": ..., "message": ...}`` with NO
        ``{"error": ...}`` wrapper and NO ``id`` (loom-rpc
        ``connection_handler::send_error``) — then closes the connection.
        Returns ``None`` when ``frame`` is not that shape (normal response
        envelopes carry ``id`` and ``result``/``error``).
        """
        if not isinstance(frame, dict):
            return None
        if "id" in frame or "result" in frame or "error" in frame:
            return None
        code = frame.get("code")
        message = frame.get("message")
        if not isinstance(code, str) or not isinstance(message, str):
            return None
        return cls(code=code, message=message, data=frame.get("data"))


class LoomConnectionError(LoomError):
    """Failed to connect to the daemon socket."""


class LoomTokenError(LoomError):
    """Token file not found and no explicit token provided."""
