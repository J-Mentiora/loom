"""
Error types for the loom Python SDK.

LoomErrorCode values mirror LoomErrorCode in loom-rpc/src/error_translator/error_translator.rs.
All snake_case strings are stable wire values (BC-RPC-03).
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


class LoomConnectionError(LoomError):
    """Failed to connect to the daemon socket."""


class LoomTokenError(LoomError):
    """Token file not found and no explicit token provided."""
