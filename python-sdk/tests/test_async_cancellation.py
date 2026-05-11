"""
Tests for asyncio.CancelledError integration on AsyncLoomTransport.

Verifies that:
1. asyncio.wait_for(call, timeout=...) raises asyncio.TimeoutError as expected.
2. The transport fires a request.cancel envelope to the daemon (best-effort)
   before re-raising CancelledError, so structured-concurrency callers work.
3. Subsequent calls on the same transport still succeed (no pending leak).
"""

from __future__ import annotations

import asyncio
import time

import loom


def test_cancelled_during_slow_call(daemon):
    """Wrap a slow call in asyncio.wait_for; assert TimeoutError raised."""
    cancel_calls: list[dict] = []
    slow_started = asyncio.Event()  # not used cross-thread; just a flag

    def _slow_handler(_params: dict) -> dict:
        # Block the handler thread; the test cancels the awaiter before this
        # returns. MockDaemon uses one handler thread per connection, so
        # this single sleep is enough to keep the call pending.
        time.sleep(0.3)
        return {}

    def _cancel_handler(params: dict) -> dict:
        cancel_calls.append(params)
        return {"cancelled": True}

    daemon.register_handler("test.slow", _slow_handler)
    daemon.register_handler("request.cancel", _cancel_handler)

    async def _run() -> None:
        t = await loom.AsyncSession.create(
            socket_path=str(daemon.socket_path), token=daemon.token
        )
        try:
            # Wrap the slow call in a timeout shorter than the handler's sleep.
            try:
                await asyncio.wait_for(t._transport.call("test.slow", {}), timeout=0.05)
            except asyncio.TimeoutError:
                pass  # expected
            else:
                raise AssertionError("expected asyncio.TimeoutError")
            # Give the transport a moment to flush the cancel envelope.
            await asyncio.sleep(0.4)
        finally:
            await t._transport.close()

    asyncio.run(_run())
    # request.cancel was sent at least once with the right shape
    # (request_id is monotonic; we don't assert a specific value).
    assert cancel_calls, "expected request.cancel envelope to have been sent"
    assert "request_id" in cancel_calls[0]


def test_cancelled_call_does_not_leak_pending(daemon):
    """After a cancellation, the transport's _pending map should be drained."""

    def _slow_handler(_params: dict) -> dict:
        time.sleep(0.2)
        return {}

    def _cancel_handler(_params: dict) -> dict:
        return {"cancelled": True}

    daemon.register_handler("test.slow", _slow_handler)
    daemon.register_handler("request.cancel", _cancel_handler)

    async def _run() -> int:
        t = await loom.AsyncLoomTransport.connect(
            socket_path=str(daemon.socket_path), token=daemon.token
        )
        try:
            try:
                await asyncio.wait_for(t.call("test.slow", {}), timeout=0.05)
            except asyncio.TimeoutError:
                pass
            # Allow a tick for any cleanup.
            await asyncio.sleep(0.1)
            return len(t._pending)
        finally:
            await t.close()

    pending_after = asyncio.run(_run())
    assert pending_after == 0, f"expected _pending empty, found {pending_after} entries"
