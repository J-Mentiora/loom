"""
settle-capture: Session.navigate readiness options + settle receipt fields.

Verifies the SDK threads ``until`` / ``timeout_ms`` into the ``web.navigate``
action payload, and surfaces the wire receipt's ``settle_until`` /
``settle_outcome`` onto ``Receipt.settle_until`` / ``Receipt.settle_outcome``.
Covers both the sync and async navigate paths.
"""

from __future__ import annotations

import asyncio
import json
from typing import Any

import loom


def _decode_payload(params: dict) -> dict:
    """Decode the JSON action payload the SDK packs into action.payload bytes."""
    return json.loads(bytes(params["action"]["payload"]).decode("utf-8"))


def _register_navigate(daemon: Any) -> dict[str, Any]:
    """Capture the last navigate payload and echo a synthetic settle receipt."""
    state: dict[str, Any] = {"payload": None}

    def _navigate(params: dict) -> dict:
        payload = _decode_payload(params)
        state["payload"] = payload
        until = payload.get("until", "settled")
        return {
            "action_hash": "a" * 64,
            "outcome_hash": "b" * 64,
            "emitted_at_ms": 1_730_000_000_000,
            "settle_until": until,
            "settle_outcome": "reached",
        }

    daemon.register_handler("action.web.navigate", _navigate)
    return state


def test_default_navigate_omits_options_and_surfaces_settle_fields(daemon):
    state = _register_navigate(daemon)
    with loom.Session.create(socket_path=str(daemon.socket_path), token=daemon.token) as s:
        receipt = s.navigate("https://example.com")

    # The SDK injects no default `until`/`timeout_ms`; the daemon applies settled.
    assert state["payload"] == {"url": "https://example.com"}
    # Settle fields flow from the wire receipt onto the typed Receipt.
    assert receipt.settle_until == "settled"
    assert receipt.settle_outcome == "reached"


def test_navigate_threads_until_and_timeout_ms(daemon):
    state = _register_navigate(daemon)
    with loom.Session.create(socket_path=str(daemon.socket_path), token=daemon.token) as s:
        receipt = s.navigate("https://spa.example.com", until="networkidle", timeout_ms=1234)

    assert state["payload"]["until"] == "networkidle"
    assert state["payload"]["timeout_ms"] == 1234
    assert receipt.settle_until == "networkidle"
    assert receipt.settle_outcome == "reached"


def test_async_navigate_threads_until_and_timeout_ms(daemon):
    state = _register_navigate(daemon)

    async def _run() -> loom.Receipt:
        s = await loom.AsyncSession.create(
            socket_path=str(daemon.socket_path), token=daemon.token
        )
        try:
            return await s.navigate("https://spa.example.com", until="load", timeout_ms=500)
        finally:
            await s.close()

    receipt = asyncio.run(_run())
    assert state["payload"]["until"] == "load"
    assert state["payload"]["timeout_ms"] == 500
    assert receipt.settle_until == "load"
    assert receipt.settle_outcome == "reached"


def _register_wait_for(daemon):
    """Capture the last wait_for payload and echo a synthetic settle receipt."""
    state = {"payload": None}

    def _wait_for(params: dict) -> dict:
        payload = _decode_payload(params)
        state["payload"] = payload
        until = payload.get("until", "settled")
        return {
            "action_hash": "c" * 64,
            "outcome_hash": "d" * 64,
            "emitted_at_ms": 1_730_000_000_000,
            "settle_until": until,
            "settle_outcome": "reached",
        }

    daemon.register_handler("action.web.wait_for", _wait_for)
    return state


def test_wait_for_default_omits_options_and_surfaces_settle_fields(daemon):
    state = _register_wait_for(daemon)
    with loom.Session.create(socket_path=str(daemon.socket_path), token=daemon.token) as s:
        receipt = s.wait_for()

    assert state["payload"] == {}
    assert receipt.settle_until == "settled"
    assert receipt.settle_outcome == "reached"


def test_wait_for_threads_until_and_timeout_ms(daemon):
    state = _register_wait_for(daemon)
    with loom.Session.create(socket_path=str(daemon.socket_path), token=daemon.token) as s:
        receipt = s.wait_for(until="networkidle", timeout_ms=2500)

    assert state["payload"]["until"] == "networkidle"
    assert state["payload"]["timeout_ms"] == 2500
    assert receipt.settle_until == "networkidle"


def test_async_wait_for_threads_options(daemon):
    state = _register_wait_for(daemon)

    async def _run() -> loom.Receipt:
        s = await loom.AsyncSession.create(
            socket_path=str(daemon.socket_path), token=daemon.token
        )
        try:
            return await s.wait_for(until="load", timeout_ms=750)
        finally:
            await s.close()

    receipt = asyncio.run(_run())
    assert state["payload"]["until"] == "load"
    assert state["payload"]["timeout_ms"] == 750
    assert receipt.settle_until == "load"
