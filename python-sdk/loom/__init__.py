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

# Single source of truth for the package version: pyproject.toml reads this via
# [tool.hatch.version], and the publish workflow asserts it matches the release tag.
__version__ = "0.10.1"
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
    "kill_session",
    "daemon_health",
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


def _navigate_payload(url: str, until: str | None, timeout_ms: int | None) -> dict:
    """Build the web.navigate action payload, omitting settle-capture options
    when unset so the daemon applies its defaults (until="settled")."""
    payload: dict = {"url": url}
    if until is not None:
        payload["until"] = until
    if timeout_ms is not None:
        payload["timeout_ms"] = timeout_ms
    return payload


def _wait_for_payload(until: str | None, timeout_ms: int | None) -> dict:
    """Build the web.wait_for action payload, omitting options when unset so the
    daemon applies its defaults (until="settled")."""
    payload: dict = {}
    if until is not None:
        payload["until"] = until
    if timeout_ms is not None:
        payload["timeout_ms"] = timeout_ms
    return payload


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
        no_determinism: bool = False,
    ) -> Session:
        """Create a new session on the daemon and return a Session handle.

        Determinism is ON by default (frozen clock/animations + seeded RNG →
        byte-reproducible captures). Pass ``no_determinism=True`` for
        live/non-reproducible capture; such a session is recorded as
        NON-REPLAYABLE (``replay`` refuses it).
        """
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
        if no_determinism:
            params["no_determinism"] = True
        result = transport.call("session.create", params)
        return cls(
            session_id=result["session_id"],
            status=result.get("status", "active"),
            transport=transport,
        )

    def navigate(
        self,
        url: str,
        *,
        deadline_ms: int = 5000,
        until: str | None = None,
        timeout_ms: int | None = None,
    ) -> Receipt:
        """Navigate and capture DOM + screenshot, gating the capture on a
        readiness state (settle-capture).

        By default loom waits until the page is ``"settled"`` (network quiet +
        ``readyState`` complete + the final URL stable after client-side
        redirects + the DOM quiescent), so the capture is a real rendered page
        rather than a blank SPA shell or an arbitrary animation frame.

        Args:
            until: ``"load" | "networkidle" | "settled"`` (default ``"settled"``).
            timeout_ms: bound on the readiness wait. If readiness is never
                reached (persistent connection, perpetual animation) the call
                still returns — the receipt's ``settle_outcome`` is ``"timeout"``
                / ``"dom_unstable"`` instead of ``"reached"``. It never hangs.
        """
        r = self._transport.call(
            "action.web.navigate",
            _build_action_params(
                self.session_id, "navigate", _navigate_payload(url, until, timeout_ms), deadline_ms
            ),
        )
        return Receipt._from_dict(r)

    def wait_for(
        self,
        *,
        deadline_ms: int = 30000,
        until: str | None = None,
        timeout_ms: int | None = None,
    ) -> Receipt:
        """Wait for the CURRENT page to reach a readiness state (settle-capture),
        without navigating.

        Use after a navigate or an interaction that triggers async re-render to
        gate a subsequent screenshot/snapshot on real readiness instead of a
        magic sleep.

        Args:
            until: ``"load" | "networkidle" | "settled"`` (default ``"settled"``).
            timeout_ms: bound on the wait. If readiness is never reached the call
                still returns — the receipt's ``settle_outcome`` is ``"timeout"``
                / ``"dom_unstable"`` instead of ``"reached"``. It never hangs.
        """
        r = self._transport.call(
            "action.web.wait_for",
            _build_action_params(
                self.session_id, "wait_for", _wait_for_payload(until, timeout_ms), deadline_ms
            ),
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

    def kill(self) -> None:
        """Force-terminate this session.

        Use ``close()`` for normal shutdown. Use ``abort()`` to cancel
        in-flight actions while keeping the session. ``kill()`` is an
        ADMIN ESCAPE HATCH — the daemon tears down the shim with a 5 s
        ceiling then SIGKILL.
        """
        _do_kill_session_sync(self._transport, self.session_id)

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
        no_determinism: bool = False,
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
        if no_determinism:
            params["no_determinism"] = True
        result = await transport.call("session.create", params)
        return cls(
            session_id=result["session_id"],
            status=result.get("status", "active"),
            transport=transport,
        )

    async def navigate(
        self,
        url: str,
        *,
        deadline_ms: int = 5000,
        until: str | None = None,
        timeout_ms: int | None = None,
    ) -> Receipt:
        """Navigate and capture DOM + screenshot, gating the capture on a
        readiness state (settle-capture). See :meth:`Session.navigate` for the
        ``until`` / ``timeout_ms`` semantics."""
        r = await self._transport.call(
            "action.web.navigate",
            _build_action_params(
                self.session_id, "navigate", _navigate_payload(url, until, timeout_ms), deadline_ms
            ),
        )
        return Receipt._from_dict(r)

    async def wait_for(
        self,
        *,
        deadline_ms: int = 30000,
        until: str | None = None,
        timeout_ms: int | None = None,
    ) -> Receipt:
        """Wait for the CURRENT page to reach a readiness state (settle-capture),
        without navigating. See :meth:`Session.wait_for` for the ``until`` /
        ``timeout_ms`` semantics."""
        r = await self._transport.call(
            "action.web.wait_for",
            _build_action_params(
                self.session_id, "wait_for", _wait_for_payload(until, timeout_ms), deadline_ms
            ),
        )
        return Receipt._from_dict(r)

    async def close(self) -> SessionInfo:
        result = await self._transport.call("session.close", {"session_id": self.session_id})
        await self._transport.close()
        if result:
            return SessionInfo._from_dict(result)
        return SessionInfo(self.session_id, "closed", 0)

    async def kill(self) -> None:
        """Force-terminate this session (async).

        Same semantics as ``Session.kill()``: ADMIN ESCAPE HATCH; daemon
        tears down the shim with a 5 s ceiling then SIGKILL. Supports
        :class:`asyncio.CancelledError` integration via the underlying
        transport.
        """
        await self._transport.call("session.kill", {"session_id": self.session_id})

    async def __aenter__(self) -> AsyncSession:
        return self

    async def __aexit__(self, *_: Any) -> None:
        await self.close()


# ─── admin RPCs (kill_session, daemon_health) ─────────────────────────────


def _do_kill_session_sync(transport: LoomTransport, session_id: str) -> None:
    """Internal single call site shared by ``Session.kill()`` and
    the top-level ``kill_session()`` free function."""
    transport.call("session.kill", {"session_id": session_id})


def kill_session(
    session_id: str,
    *,
    socket_path: str | None = None,
    token: str | None = None,
) -> None:
    """ADMIN ESCAPE HATCH — force-terminate a stuck session by id without
    holding a :class:`Session` handle.

    Performs the abort flow plus a blocking 5 s shim-teardown ceiling,
    then SIGKILL. Prefer ``Session.close()`` for normal shutdown; reach
    for ``kill_session()`` only when normal shutdown is wedged.

    The daemon authenticates the calling transport at the connection
    level (HELLO token handshake) — there is no separate per-call gate
    on this admin function.
    """
    with LoomTransport(socket_path, token) as t:
        _do_kill_session_sync(t, session_id)


def daemon_health(
    *,
    deep: bool = False,
    socket_path: str | None = None,
    token: str | None = None,
) -> dict[str, Any]:
    """Query daemon health.

    Shallow path is non-blocking. ``deep=True`` fans out a per-shim probe
    (1 s budget per shim, 3 s overall) and returns uptime/requests-served
    counters per running shim.

    Returns the parsed JSON payload as a dict; field names use snake_case
    (matching the wire format) — see ``loom-rpc/src/rpc_handlers/rpc_handlers.rs``
    for the field schema.
    """
    with LoomTransport(socket_path, token) as t:
        return t.call("daemon.health", {"deep": deep})
