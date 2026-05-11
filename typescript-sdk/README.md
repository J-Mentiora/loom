# @mentiora-ai/loom-sdk

TypeScript client library for the
[loom](https://github.com/mentiora-ai/loom) browser-automation daemon.

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
npm install @mentiora-ai/loom-sdk
```

Requires Node ≥ 20.

## Quick start

```ts
import { Session } from "@mentiora-ai/loom-sdk";

const session = await Session.create();
try {
  const receipt = await session.navigate("https://example.com");
  console.log(receipt.action_hash);
} finally {
  await session.close();
}
```

## What the SDK exposes

- `Session` — session lifecycle (create / close / abort / kill / replay /
  inspect / validate / export).
- `Session.{navigate, click, typeText, select, hover, scroll, wait,
  evaluate, screenshot, snapshot}` — every web action surface.
- Admin RPCs: `killSession(sessionId, ...)` (force-terminate without a
  handle), `daemonHealth({ deep?, signal? })` (operational snapshot).
- Receipt + summary types in `@mentiora-ai/loom-sdk/types` — `Receipt`,
  `SessionInfo`, `SessionInspection`, `DiffReport`, `ExportInfo`,
  `ValidationResult`, `GrantInfo`, `SchemaRegistry`, `LoomErrorCode`,
  plus `DaemonHealthResult`, `ShimDeepHealth`, `ShimBreakerSnapshot`,
  `ProbeStatus`.
- Typed errors: `LoomError`, `LoomRPCError`, `LoomConnectionError`,
  `LoomTokenError`, `LoomAbortError`.

## Cancellation

`LoomTransport.call(method, params, { signal })` accepts an
`AbortSignal`. On abort, the transport fires a `request.cancel` envelope
at the daemon and rejects the returned promise with `LoomAbortError`
(`name === "AbortError"`). Compose with `AbortSignal.timeout(ms)` or
your own controller:

```ts
import { LoomTransport, daemonHealth, LoomAbortError } from "@mentiora-ai/loom-sdk";

try {
  const health = await daemonHealth({
    deep: true,
    signal: AbortSignal.timeout(2_000),
  });
  console.log(health);
} catch (err) {
  if (err instanceof LoomAbortError) {
    console.log("aborted");
  } else {
    throw err;
  }
}
```

## Connection details

`Session.create()` defaults work when a single user runs the daemon on
their own machine. Override per-call if needed:

```ts
const session = await Session.create({
  socketPath: "/var/run/loom/loom.sock",  // custom daemon socket
  token: "...",                            // explicit HELLO-token
  profile: "standard",                     // or "safe", "full"
  networkMode: "live",                     // or "replay"
  seed: 42,                                // determinism seed
});
```

## License

Apache-2.0. See the
[main repository](https://github.com/mentiora-ai/loom) for details.
