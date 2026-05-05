# Credits

Loom was incubated inside a private Mentiora development project before
this open-source release. It was built with substantial AI-assisted
authoring (Anthropic Claude — Sonnet 4.6 and Opus 4.7) under human
review at every gate. Human stewardship + final design decisions by
Johannes Rummel and the Mentiora team.

The implementation went through many iterative rounds of testing and
hardening — security, deterministic replay, MCP integration, Chromium
crash detection, GC reference protection, runtime correctness — before
the v0.9 public extraction.

## v0.9.0 extraction

This repository is a clean extraction. The git history visible here
starts at the v0.9.0 ship; pre-extraction history lives in a private
internal repo and is not published.

If you depend on a specific behavior and want to know how it was
designed, the [README's "Status" matrix](README.md#status) flags what
is stable vs beta, and the per-action documentation in
[docs/actions.md](docs/actions.md) is the canonical wire-shape spec.

## Third-party crates

`Cargo.lock` is the authoritative list. Notable load-bearing
dependencies, with the role they fill:

- **wasmtime** — host-side runtime for the WASM-isolated surface API
- **chromiumoxide** — Chrome DevTools Protocol typed bindings (only in
  `loom-shims`; out-of-process Chromium driver)
- **tokio** — async runtime
- **jsonrpsee** — Unix-socket JSON-RPC server for the daemon
- **clap** — CLI parser
- **ring** — vault crypto primitives
- **serde / serde_jcs** — canonical-bytes serialization for manifest
  hash chains and receipts
- **wit-bindgen** — host/guest binding generation against the surface
  ABI declared in `wit/loom-surface.wit`
- **tokio-tungstenite** — WebSocket transport for raw CDP

License audit (`cargo deny check licenses`) gates the release; see
[deny.toml](deny.toml) for the allow-list.
