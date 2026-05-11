"""
Tests for the admin RPC wrappers: Session.kill, AsyncSession.kill,
kill_session(), and daemon_health().

Uses the MockDaemon fixture from conftest.py with custom handlers for
session.kill and daemon.health.
"""

from __future__ import annotations

import asyncio
from typing import Any

import loom


def _register_admin_handlers(daemon: Any, *, deep_shims: list[dict] | None = None) -> dict[str, Any]:
    """Wire session.kill + daemon.health handlers into MockDaemon."""
    state: dict[str, Any] = {"kill_calls": [], "health_calls": []}

    def _session_kill(params: dict) -> dict:
        sid = params.get("session_id", "")
        state["kill_calls"].append(sid)
        if sid in daemon.sessions:
            daemon.sessions[sid]["status"] = "killed"
            return daemon.sessions[sid].copy()
        return {"session_id": sid, "status": "killed", "created_at_ms": 0}

    def _daemon_health(params: dict) -> dict:
        state["health_calls"].append(params)
        out = {
            "active_sessions": len(
                [s for s in daemon.sessions.values() if s["status"] == "active"]
            ),
            "shim_breaker_states": [],
            "otel_exporter": "disabled",
            "deep": None,
        }
        if params.get("deep") and deep_shims is not None:
            out["deep"] = deep_shims
        return out

    daemon.register_handler("session.kill", _session_kill)
    daemon.register_handler("daemon.health", _daemon_health)
    return state


def test_session_kill_method_via_handle(daemon):
    state = _register_admin_handlers(daemon)
    with loom.Session.create(
        socket_path=str(daemon.socket_path), token=daemon.token
    ) as s:
        s.kill()
    assert state["kill_calls"], "session.kill RPC was not invoked"
    assert state["kill_calls"][0].startswith("01TEST")


def test_kill_session_free_function(daemon):
    state = _register_admin_handlers(daemon)
    # Create a session first so we have an id to kill.
    with loom.Session.create(
        socket_path=str(daemon.socket_path), token=daemon.token
    ) as s:
        sid = s.session_id
    # Now kill it admin-style (separate connection).
    loom.kill_session(sid, socket_path=str(daemon.socket_path), token=daemon.token)
    # Two kill calls: one from `with` exit (close not kill), one from kill_session
    # Actually close is session.close not session.kill, so only ONE kill call:
    assert len(state["kill_calls"]) == 1
    assert state["kill_calls"][0] == sid


def test_daemon_health_shallow(daemon):
    _register_admin_handlers(daemon)
    result = loom.daemon_health(
        socket_path=str(daemon.socket_path), token=daemon.token
    )
    assert "active_sessions" in result
    assert "shim_breaker_states" in result
    assert "otel_exporter" in result
    assert result["deep"] is None


def test_daemon_health_deep(daemon):
    deep_payload = [
        {
            "shim_id": "chromium-01",
            "daemon_restart_count": 2,
            "daemon_last_restart_at_ms": 1730000000000,
            "shim_uptime_ms": 12345,
            "shim_requests_served": 7,
            "shim_last_request_at_ms": 1730000005000,
            "probe_status": "ok",
        }
    ]
    _register_admin_handlers(daemon, deep_shims=deep_payload)
    result = loom.daemon_health(
        deep=True, socket_path=str(daemon.socket_path), token=daemon.token
    )
    assert result["deep"] is not None
    assert len(result["deep"]) == 1
    assert result["deep"][0]["shim_id"] == "chromium-01"
    assert result["deep"][0]["probe_status"] == "ok"
    assert result["deep"][0]["shim_uptime_ms"] == 12345


def test_async_session_kill(daemon):
    _register_admin_handlers(daemon)

    async def _run() -> None:
        s = await loom.AsyncSession.create(
            socket_path=str(daemon.socket_path), token=daemon.token
        )
        try:
            await s.kill()
        finally:
            await s._transport.close()

    asyncio.run(_run())


def test_kill_session_nonexistent_returns_rpc_error(daemon):
    """Negative path: kill_session on an unknown id should surface a
    typed LoomRPCError (not a connection error or silent success)."""
    import pytest

    # Override handler to error on unknown session ids.
    def _kill_strict(params: dict) -> dict:
        sid = params.get("session_id", "")
        if sid not in daemon.sessions:
            raise ValueError(f"session not found: {sid}")  # MockDaemon maps to internal_error envelope
        daemon.sessions[sid]["status"] = "killed"
        return daemon.sessions[sid].copy()

    daemon.register_handler("session.kill", _kill_strict)
    with pytest.raises(loom.LoomRPCError):
        loom.kill_session(
            "01NONEXISTENT" + "0" * 12,
            socket_path=str(daemon.socket_path),
            token=daemon.token,
        )


def test_daemon_health_error_envelope_surfaces_as_rpc_error(daemon):
    """Negative path: daemon.health returning an error envelope should
    raise LoomRPCError, not silently return a malformed result."""
    import pytest

    def _broken_health(_params: dict) -> dict:
        raise RuntimeError("simulated daemon-side failure")

    daemon.register_handler("daemon.health", _broken_health)
    with pytest.raises(loom.LoomRPCError):
        loom.daemon_health(socket_path=str(daemon.socket_path), token=daemon.token)
