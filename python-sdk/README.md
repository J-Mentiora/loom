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
pip install loom
```

Requires Python ≥ 3.11.

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
  / replay / inspect / validate / export).
- `Session.{navigate, click, type_text, select, hover, scroll, wait,
  evaluate, screenshot, snapshot}` — every web action surface.
- Receipt + summary types in `loom.types` — `Receipt`, `SessionInfo`,
  `SessionInspection`, `DiffReport`, `ExportInfo`, `ValidationResult`,
  `GrantInfo`, `SchemaRegistry`, `LoomErrorCode`.
- Typed errors: `LoomError`, `LoomRPCError`, `LoomConnectionError`,
  `LoomTokenError`.

## Connection details

`Session.create()` defaults work when a single user runs the daemon on
their own machine. Override per-call if needed:

```python
session = loom.Session.create(
    socket_path="/var/run/loom/loom.sock",  # custom daemon socket
    token="...",                             # explicit HELLO-token
    profile="standard",                      # or "safe", "full"
    network_mode="live",                     # or "replay"
    seed=42,                                 # determinism seed
)
```

## License

Apache-2.0. See the [main repository](https://github.com/mentiora-ai/loom)
for details.
