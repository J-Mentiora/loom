# Mock harnesses

Each system's mock module lives **inside the owning crate** at
`<crate>/src/mocks.rs` and is gated by `#[cfg(any(test, feature = "mock"))]`.
This keeps each mock co-located with the types it stubs and avoids a
synthetic crate that would have to depend on every system simultaneously.

To enable mocks during a feature worker's TDD phase:

```toml
# In the feature worker's dev-dependencies on the relevant crate:
loom-core = { workspace = true, features = ["mock"] }
loom-host = { workspace = true, features = ["mock"] }
# ... etc.
```

Files indexed here:

| System         | Mock module                          |
|----------------|--------------------------------------|
| loom-core      | `loom-core/src/mocks.rs`             |
| loom-host      | `loom-host/src/mocks.rs`             |
| loom-rpc       | `loom-rpc/src/mocks.rs`              |
| loom-mcp       | `loom-mcp/src/mocks.rs`              |
| loom-cli       | `loom-cli/src/mocks.rs`              |
| loom-surfaces  | `loom-surfaces/src/mocks.rs`         |
| loom-shims     | `loom-shims/src/mocks.rs`            |
