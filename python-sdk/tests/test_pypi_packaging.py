"""
Smoke test for the published Python SDK.

After `pip install loom` on Python >= 3.11, importing `loom` should
expose `Session` / `AsyncSession`, and `Session.create()` should
round-trip through the daemon and return a Session whose `session_id`
matches the daemon's record.
"""
from __future__ import annotations

import asyncio

import pytest

import loom
from tests.conftest import MockDaemon


def test_import_loom_exposes_session():
    """Session class is accessible at top-level import."""
    assert hasattr(loom, "Session")
    assert hasattr(loom, "AsyncSession")


def test_session_create_returns_session(daemon: MockDaemon):
    """Session.create() returns a Session with a non-empty session_id."""
    session = loom.Session.create(
        socket_path=str(daemon.socket_path),
        token=daemon.token,
    )
    assert isinstance(session, loom.Session)
    assert session.session_id
    session.close()


def test_session_id_matches_daemon_record(daemon: MockDaemon):
    """session_id returned by SDK matches the daemon's record."""
    session = loom.Session.create(
        socket_path=str(daemon.socket_path),
        token=daemon.token,
    )
    assert session.session_id in daemon.sessions
    assert daemon.sessions[session.session_id]["session_id"] == session.session_id
    session.close()


def test_session_has_status(daemon: MockDaemon):
    """Session.create() populates .status from the daemon response."""
    session = loom.Session.create(
        socket_path=str(daemon.socket_path),
        token=daemon.token,
    )
    assert session.status == "active"
    session.close()


def test_session_context_manager(daemon: MockDaemon):
    """Session works as a context manager."""
    with loom.Session.create(
        socket_path=str(daemon.socket_path),
        token=daemon.token,
    ) as session:
        assert session.session_id


def test_async_session_create(daemon: MockDaemon):
    """AsyncSession.create() coroutine returns AsyncSession with session_id."""

    async def _run():
        session = await loom.AsyncSession.create(
            socket_path=str(daemon.socket_path),
            token=daemon.token,
        )
        assert isinstance(session, loom.AsyncSession)
        assert session.session_id
        assert session.session_id in daemon.sessions
        await session.close()

    asyncio.run(_run())


def test_wrong_token_raises_loom_error(daemon: MockDaemon):
    """Wrong token causes LoomRPCError with protocol_auth_required code."""
    with pytest.raises(loom.LoomRPCError) as exc_info:
        loom.Session.create(
            socket_path=str(daemon.socket_path),
            token="wrong-token",
        )
    assert exc_info.value.code == "protocol_auth_required"
