"""
Tests for concurrent in-flight calls on AsyncLoomTransport.

Verifies the id-keyed demux contract: multiple `await call()` invocations
issued concurrently (via asyncio.gather) all resolve with their correct
id-correlated payloads. A regression here would manifest as cross-talk
(wrong result for the wrong call) or hung futures.
"""

from __future__ import annotations

import asyncio

import loom


def test_concurrent_calls_no_cross_talk(daemon):
    """Three concurrent calls; each handler returns a distinct sentinel."""

    def _make_handler(sentinel: str):
        def _h(_params: dict) -> dict:
            # Stagger response times so the order of replies differs from
            # the order of sends — exercises the demux directly.
            import time

            time.sleep(0.05)
            return {"sentinel": sentinel}

        return _h

    daemon.register_handler("test.a", _make_handler("alpha"))
    daemon.register_handler("test.b", _make_handler("beta"))
    daemon.register_handler("test.c", _make_handler("gamma"))

    async def _run() -> tuple[dict, dict, dict]:
        t = await loom.AsyncLoomTransport.connect(
            socket_path=str(daemon.socket_path), token=daemon.token
        )
        try:
            results = await asyncio.gather(
                t.call("test.a", {}),
                t.call("test.b", {}),
                t.call("test.c", {}),
            )
            return results
        finally:
            await t.close()

    a, b, c = asyncio.run(_run())
    assert a["sentinel"] == "alpha"
    assert b["sentinel"] == "beta"
    assert c["sentinel"] == "gamma"


def test_pending_drains_after_concurrent_calls(daemon):
    """After all gathered calls resolve, _pending should be empty."""

    def _handler(_params: dict) -> dict:
        return {"ok": True}

    daemon.register_handler("test.ping", _handler)

    async def _run() -> int:
        t = await loom.AsyncLoomTransport.connect(
            socket_path=str(daemon.socket_path), token=daemon.token
        )
        try:
            await asyncio.gather(*(t.call("test.ping", {}) for _ in range(5)))
            return len(t._pending)
        finally:
            await t.close()

    pending = asyncio.run(_run())
    assert pending == 0
