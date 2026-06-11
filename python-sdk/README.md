# loom (Python SDK)

Python client library for the [loom](https://github.com/mentiora-ai/loom)
browser-automation daemon.

## Prerequisites

The SDK talks to a running `loom-daemon` over a Unix socket. Install
loom (CLI + daemon + chromium pin) first via Homebrew, `cargo install`,
or the install script — see the
[main README](https://github.com/mentiora-ai/loom#install). Then start
the daemon:

```bash
loom serve
```

## Install

```bash
pip install mentiora-loom
```

The distribution is named `mentiora-loom`; it still imports as `loom`
(`import loom`). Requires Python ≥ 3.11.

## Quick start (sync)

```python
import loom

with loom.Session.create() as session:
    receipt = session.navigate("https://example.com")
    print(receipt.action_hash)
```

## Quick start (async)

```python
import asyncio
import loom

async def main():
    async with await loom.AsyncSession.create() as session:
        receipt = await session.navigate("https://example.com")
        print(receipt.action_hash)

asyncio.run(main())
```

## What the SDK exposes

- `Session` / `AsyncSession` — session lifecycle (create / close / abort
  / kill / replay / inspect / validate / export).
- `Session.{navigate, click, type_text, select, hover, scroll, wait,
  evaluate, screenshot, snapshot}` — every web action surface.
- Admin RPCs: `kill_session(session_id, ...)` (force-terminate without a
  handle), `daemon_health(deep=...)` (operational snapshot).
- Receipt + summary types in `loom.types` — `Receipt`, `SessionInfo`,
  `SessionInspection`, `DiffReport`, `ExportInfo`, `ValidationResult`,
  `GrantInfo`, `SchemaRegistry`, `LoomErrorCode`.
- Typed errors: `LoomError`, `LoomRPCError`, `LoomConnectionError`,
  `LoomTokenError`.

## Cancellation (async only)

`AsyncSession` supports transparent cancellation via standard asyncio
primitives. Wrap a call in `asyncio.wait_for` or cancel its enclosing
task and the SDK fires a `request.cancel` envelope at the daemon, then
re-raises `asyncio.CancelledError` so `TaskGroup` / `asyncio.timeout`
keep working:

```python
import asyncio
import loom

async with await loom.AsyncSession.create() as s:
    try:
        await asyncio.wait_for(s.navigate("https://example.com"), timeout=5.0)
    except asyncio.TimeoutError:
        # SDK already sent request.cancel before the timeout re-raised.
        ...
```

Sync `Session` is single-in-flight by design — there is no cancellation
hook. For cancellable work, reach for `AsyncSession`.

## Connection details

`Session.create()` defaults work when a single user runs the daemon on
their own machine. Override per-call if needed:

```python
session = loom.Session.create(
    socket_path="/var/run/loom/loom.sock",  # custom daemon socket
    token="...",                             # explicit HELLO-token
    profile="standard",                      # or "safe", "full"
    network_mode="live",                     # the only mode (see below)
    seed=42,                                 # determinism seed
)
```

## Network mode: page traffic is always live

``network_mode`` accepts exactly one value, ``"live"`` (the default).
Page traffic is always fetched live from the network — loom records the
manifest hash chain and per-request metadata, **not** page-network
responses. Response bodies are never captured, so HAR exports contain
request entries without bodies, and replay re-executes the recorded
actions rather than serving recorded responses. Any other value
(``"recorded"``, ``"mixed"``, ``"replay"``) is rejected at
``Session.create()`` with ``invalid_network_mode``.

## License

Apache-2.0. See the [main repository](https://github.com/mentiora-ai/loom)
for details.
