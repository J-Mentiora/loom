"""
loom — Python client library for the loom browser-automation daemon.

Quick start::

    import loom

    # Synchronous
    with loom.Session.create() as session:
        receipt = session.navigate("https://example.com")

    # Asynchronous
    import asyncio

    async def main():
        async with await loom.AsyncSession.create() as session:
            receipt = await session.navigate("https://example.com")

    asyncio.run(main())
"""

from __future__ import annotations

from typing import Any

from loom._async_transport import AsyncLoomTransport
from loom._errors import LoomConnectionError, LoomError, LoomRPCError, LoomTokenError
from loom._transport import LoomTransport
from loom.types import (
    DiffReport,
    ExportInfo,
    GrantInfo,
    LoomErrorCode,
    Receipt,
    SchemaRegistry,
    SessionInfo,
    SessionInspection,
    ValidationResult,
)

__version__ = "0.9.0"
__all__ = [
    "Session",
    "AsyncSession",
    "LoomError",
    "LoomRPCError",
    "LoomConnectionError",
    "LoomTokenError",
    "SessionInfo",
    "SessionInspection",
    "Receipt",
    "DiffReport",
    "ExportInfo",
    "ValidationResult",
    "GrantInfo",
    "SchemaRegistry",
    "LoomErrorCode",
]


def _build_action_params(session_id: str, kind: str, payload: dict, deadline_ms: int) -> dict:
    import json

    return {
        "session_id": session_id,
        "action": {
            "kind": kind,
            "payload": list(json.dumps(payload).encode("utf-8")),
            "deadline_ms": deadline_ms,
        },
    }


class Session:
    """
    Synchronous loom session handle.

    Create via ``Session.create()``. Use as a context manager or call
    ``close()`` explicitly when done.
    """

    def __init__(self, session_id: str, status: str, transport: LoomTransport) -> None:
        self.session_id = session_id
        self.status = status
        self._transport = transport

    @classmethod
    def create(
        cls,
        *,
        profile: str = "default",
        network_mode: str = "live",
        capture: bool = True,
        seed: int | None = None,
        budget: Any = None,
        socket_path: str | None = None,
        token: str | None = None,
    ) -> Session:
        """Create a new session on the daemon and return a Session handle."""
        transport = LoomTransport(socket_path, token)
        params: dict[str, Any] = {
            "profile": profile,
            "network_mode": network_mode,
            "capture": capture,
        }
        if seed is not None:
            params["seed"] = seed
        if budget is not None:
            params["budget"] = budget
        result = transport.call("session.create", params)
        return cls(
            session_id=result["session_id"],
            status=result.get("status", "active"),
            transport=transport,
        )

    def navigate(self, url: str, *, deadline_ms: int = 5000) -> Receipt:
        r = self._transport.call(
            "action.web.navigate",
            _build_action_params(self.session_id, "navigate", {"url": url}, deadline_ms),
        )
        return Receipt._from_dict(r)

    def click(self, selector: str, *, deadline_ms: int = 5000) -> Receipt:
        r = self._transport.call(
            "action.web.click",
            _build_action_params(self.session_id, "click", {"selector": selector}, deadline_ms),
        )
        return Receipt._from_dict(r)

    def type_text(self, selector: str, text: str, *, deadline_ms: int = 5000) -> Receipt:
        r = self._transport.call(
            "action.web.type_text",
            _build_action_params(
                self.session_id, "type_text", {"selector": selector, "text": text}, deadline_ms
            ),
        )
        return Receipt._from_dict(r)

    def select(self, selector: str, value: str, *, deadline_ms: int = 5000) -> Receipt:
        r = self._transport.call(
            "action.web.select",
            _build_action_params(
                self.session_id, "select", {"selector": selector, "value": value}, deadline_ms
            ),
        )
        return Receipt._from_dict(r)

    def hover(self, selector: str, *, deadline_ms: int = 5000) -> Receipt:
        r = self._transport.call(
            "action.web.hover",
            _build_action_params(self.session_id, "hover", {"selector": selector}, deadline_ms),
        )
        return Receipt._from_dict(r)

    def scroll(self, selector: str, *, delta_y: int = 300, deadline_ms: int = 5000) -> Receipt:
        r = self._transport.call(
            "action.web.scroll",
            _build_action_params(
                self.session_id, "scroll", {"selector": selector, "delta_y": delta_y}, deadline_ms
            ),
        )
        return Receipt._from_dict(r)

    def wait(self, selector: str, *, deadline_ms: int = 5000) -> Receipt:
        r = self._transport.call(
            "action.web.wait",
            _build_action_params(self.session_id, "wait", {"selector": selector}, deadline_ms),
        )
        return Receipt._from_dict(r)

    def evaluate(self, expression: str, *, deadline_ms: int = 5000) -> Receipt:
        r = self._transport.call(
            "action.web.evaluate",
            _build_action_params(
                self.session_id, "evaluate", {"expression": expression}, deadline_ms
            ),
        )
        return Receipt._from_dict(r)

    def screenshot(self, *, deadline_ms: int = 5000) -> Receipt:
        r = self._transport.call(
            "action.web.screenshot",
            _build_action_params(self.session_id, "screenshot", {}, deadline_ms),
        )
        return Receipt._from_dict(r)

    def snapshot(self, *, deadline_ms: int = 5000) -> Receipt:
        r = self._transport.call(
            "action.web.snapshot",
            _build_action_params(self.session_id, "snapshot", {}, deadline_ms),
        )
        return Receipt._from_dict(r)

    def close(self) -> SessionInfo:
        result = self._transport.call("session.close", {"session_id": self.session_id})
        self._transport.close()
        if result:
            return SessionInfo._from_dict(result)
        return SessionInfo(self.session_id, "closed", 0)

    def abort(self, reason: str) -> SessionInfo:
        result = self._transport.call(
            "session.abort", {"session_id": self.session_id, "reason": reason}
        )
        return SessionInfo._from_dict(result)

    def inspect(self, *, at_action: int | None = None) -> SessionInspection:
        params: dict[str, Any] = {"session_id": self.session_id}
        if at_action is not None:
            params["at_action"] = at_action
        result = self._transport.call("session.inspect", params)
        return SessionInspection._from_dict(result)

    def export(self, format: str) -> ExportInfo:
        result = self._transport.call(
            "session.export", {"session_id": self.session_id, "format": format}
        )
        return ExportInfo._from_dict(result)

    def validate(self) -> ValidationResult:
        result = self._transport.call("session.validate", {"session_id": self.session_id})
        return ValidationResult._from_dict(result)

    def replay(self, *, speed: float = 1.0, network_mode: str = "replay") -> SessionInfo:
        result = self._transport.call(
            "session.replay",
            {"session_id": self.session_id, "speed": speed, "network_mode": network_mode},
        )
        return SessionInfo._from_dict(result)

    def diff(
        self,
        other_session_id: str,
        *,
        include_screenshots: bool = False,
        show_dom_diffs: bool = False,
    ) -> DiffReport:
        result = self._transport.call(
            "session.diff",
            {
                "a": self.session_id,
                "b": other_session_id,
                "include_screenshots": include_screenshots,
                "show_dom_diffs": show_dom_diffs,
            },
        )
        return DiffReport._from_dict(result)

    def __enter__(self) -> Session:
        return self

    def __exit__(self, *_: Any) -> None:
        self.close()


def session_list(*, socket_path: str | None = None, token: str | None = None) -> list[SessionInfo]:
    """List all sessions on the daemon."""
    with LoomTransport(socket_path, token) as t:
        result = t.call("session.list", {})
        return [SessionInfo._from_dict(s) for s in (result or [])]


def vault_grant(
    session_id: str,
    origin: str,
    scopes: list[str],
    ttl_seconds: int,
    label: str,
    *,
    socket_path: str | None = None,
    token: str | None = None,
) -> GrantInfo:
    with LoomTransport(socket_path, token) as t:
        result = t.call(
            "vault.grant",
            {
                "session_id": session_id,
                "origin": origin,
                "scopes": scopes,
                "ttl_seconds": ttl_seconds,
                "label": label,
            },
        )
        return GrantInfo._from_dict(result)


def vault_revoke(
    grant_id: str,
    reason: str,
    *,
    socket_path: str | None = None,
    token: str | None = None,
) -> None:
    with LoomTransport(socket_path, token) as t:
        t.call("vault.revoke", {"grant_id": grant_id, "reason": reason})


def vault_list_grants(
    session_id: str | None = None,
    *,
    socket_path: str | None = None,
    token: str | None = None,
) -> list[GrantInfo]:
    params: dict[str, Any] = {}
    if session_id is not None:
        params["session_id"] = session_id
    with LoomTransport(socket_path, token) as t:
        result = t.call("vault.list_grants", params)
        return [GrantInfo._from_dict(g) for g in (result or [])]


class AsyncSession:
    """
    Asyncio loom session handle.

    Create via ``await AsyncSession.create()``. Use as an async context
    manager or call ``await close()`` explicitly.
    """

    def __init__(self, session_id: str, status: str, transport: AsyncLoomTransport) -> None:
        self.session_id = session_id
        self.status = status
        self._transport = transport

    @classmethod
    async def create(
        cls,
        *,
        profile: str = "default",
        network_mode: str = "live",
        capture: bool = True,
        seed: int | None = None,
        budget: Any = None,
        socket_path: str | None = None,
        token: str | None = None,
    ) -> AsyncSession:
        transport = await AsyncLoomTransport.connect(socket_path, token)
        params: dict[str, Any] = {
            "profile": profile,
            "network_mode": network_mode,
            "capture": capture,
        }
        if seed is not None:
            params["seed"] = seed
        if budget is not None:
            params["budget"] = budget
        result = await transport.call("session.create", params)
        return cls(
            session_id=result["session_id"],
            status=result.get("status", "active"),
            transport=transport,
        )

    async def navigate(self, url: str, *, deadline_ms: int = 5000) -> Receipt:
        r = await self._transport.call(
            "action.web.navigate",
            _build_action_params(self.session_id, "navigate", {"url": url}, deadline_ms),
        )
        return Receipt._from_dict(r)

    async def close(self) -> SessionInfo:
        result = await self._transport.call("session.close", {"session_id": self.session_id})
        await self._transport.close()
        if result:
            return SessionInfo._from_dict(result)
        return SessionInfo(self.session_id, "closed", 0)

    async def __aenter__(self) -> AsyncSession:
        return self

    async def __aexit__(self, *_: Any) -> None:
        await self.close()
