"""
settle-capture (4b): Session.create(no_determinism=...) forwards the
`no_determinism` flag to the session.create RPC. Default sessions omit it.
Covers sync + async.

Cross-run determinism v2: Session.create(clock_anchor=...) forwards
`clock_anchor` (the fixed epoch-ms that pins the injected browser clock).
Default sessions omit it (wall-clock epoch, unchanged wire shape).
"""

from __future__ import annotations

import asyncio

import loom


def _capture_create(daemon) -> dict:
    state = {"params": None, "n": 0}

    def _create(params: dict) -> dict:
        state["params"] = params
        state["n"] += 1
        return {
            "session_id": f"01TEST{state['n']:020}",
            "status": "active",
            "created_at_ms": 0,
        }

    daemon.register_handler("session.create", _create)
    return state


def test_default_create_omits_no_determinism(daemon):
    state = _capture_create(daemon)
    with loom.Session.create(socket_path=str(daemon.socket_path), token=daemon.token):
        pass
    assert "no_determinism" not in state["params"]


def test_no_determinism_true_is_forwarded(daemon):
    state = _capture_create(daemon)
    with loom.Session.create(
        socket_path=str(daemon.socket_path), token=daemon.token, no_determinism=True
    ):
        pass
    assert state["params"]["no_determinism"] is True


def test_async_no_determinism_true_is_forwarded(daemon):
    state = _capture_create(daemon)

    async def _run() -> None:
        s = await loom.AsyncSession.create(
            socket_path=str(daemon.socket_path), token=daemon.token, no_determinism=True
        )
        await s.close()

    asyncio.run(_run())
    assert state["params"]["no_determinism"] is True


def test_default_create_omits_clock_anchor(daemon):
    state = _capture_create(daemon)
    with loom.Session.create(socket_path=str(daemon.socket_path), token=daemon.token):
        pass
    assert "clock_anchor" not in state["params"]


def test_clock_anchor_is_forwarded_with_seed(daemon):
    state = _capture_create(daemon)
    with loom.Session.create(
        socket_path=str(daemon.socket_path),
        token=daemon.token,
        seed=42,
        clock_anchor=1_700_000_000_000,
    ):
        pass
    assert state["params"]["seed"] == 42
    assert state["params"]["clock_anchor"] == 1_700_000_000_000


def test_async_clock_anchor_is_forwarded(daemon):
    state = _capture_create(daemon)

    async def _run() -> None:
        s = await loom.AsyncSession.create(
            socket_path=str(daemon.socket_path),
            token=daemon.token,
            clock_anchor=1_700_000_000_000,
        )
        await s.close()

    asyncio.run(_run())
    assert state["params"]["clock_anchor"] == 1_700_000_000_000
