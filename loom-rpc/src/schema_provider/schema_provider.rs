// SchemaProvider — loads WIT-derived JSON Schemas at startup; serves
// the in-memory registry to `SchemaValidator` and to clients via
// `rpc.schemas`.
//
// # Contract semantics
// - **WIT is source of truth (HARD).** Schemas are emitted
//   at install time by `loom-tools/wit-to-json-schema` reading
//   `wit/loom-surface.wit`. `SchemaProvider` is read-only at runtime.
// - **Load once at startup.** `load_at_startup` reads
//   every `*.json` file in the schema directory and compiles each
//   schema. Daemon refuses to start if the directory is missing or
//   any expected method file is absent.
// - **In-memory only on the request path.**
//   `lookup_request_schema` and `lookup_response_schema` read the
//   compiled `HashMap`; never re-read disk. `get_registry_snapshot`
//   returns the canonical snapshot for `rpc.schemas`.
// - **Immutable after load.** SIGHUP triggers a full reload through
//   a fresh `SchemaProvider::load_at_startup` call; in-flight requests
//   continue to see their original snapshot via `Arc`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

// Stub: in the full crate, `CompiledJsonSchema` is the
// `jsonschema::JSONSchema` type. We name the bridge here so trait
// surfaces are testable without pulling the full crate in.
//
// module_kind: registry

/// Opaque compiled-schema handle. Real impl is
/// `jsonschema::JSONSchema`; the wrapper keeps the struct nominal so
/// we can swap implementations without breaking callers.
pub struct CompiledJsonSchema {
    pub inner: serde_json::Value,
}

impl CompiledJsonSchema {
    /// Return the underlying JSON Schema document. Used by the
    /// `rpc.schemas` registry snapshot.
    pub fn as_json(&self) -> &serde_json::Value {
        &self.inner
    }
}

/// One method's request + response schemas as they appear in the
/// `rpc.schemas` registry snapshot.
///
/// `aliases` lists every alternate spelling that resolves to `method`.
/// Empty (and elided from JSON) for methods with no
/// aliases. Order matches `loom_shared::action_aliases::METHOD_ALIASES`
/// for deterministic canonical-JSON output (per the wire-spec's
/// canonical-JSON ordering rules).
/// Backward-compatible additive field — `serde(default)` means
/// pre-rollout consumers that omit it still deserialize.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodSchema {
    pub method: String,
    pub request: serde_json::Value,
    pub response: serde_json::Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
}

/// Snapshot returned by `rpc.schemas`. `methods` is sorted
/// by method name for deterministic canonical-JSON output (per the
/// wire-spec's canonical-JSON ordering rules).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaRegistry {
    pub methods: Vec<MethodSchema>,
    pub source_wit_sha256: String,
}

/// Trait surface so `SchemaValidator` and `RpcHandlers::rpc_schemas`
/// can be unit-tested against a fake.
pub trait SchemaProviderApi: Send + Sync {
    /// Look up the compiled request schema for a method. Returns
    /// `None` if the method is not in the registry — callers translate
    /// to `LoomErrorCode::MethodNotFound`.
    fn lookup_request_schema(&self, method: &str) -> Option<Arc<CompiledJsonSchema>>;

    /// Look up the compiled response schema for a method.
    /// Used by `RpcHandlers` to validate vault.grant responses
    /// before returning to client.
    fn lookup_response_schema(&self, method: &str) -> Option<Arc<CompiledJsonSchema>>;

    /// Enumerate every registered method name. `RequestRouter` uses
    /// this at startup to assert handler coverage.
    fn registered_methods(&self) -> Vec<String>;

    /// Return the canonical `SchemaRegistry` snapshot for
    /// `rpc.schemas()`. **In-memory only** — must NOT re-read disk.
    fn get_registry_snapshot(&self) -> SchemaRegistry;
}

pub struct SchemaProvider {
    pub(crate) request_schemas: HashMap<String, Arc<CompiledJsonSchema>>,
    pub(crate) response_schemas: HashMap<String, Arc<CompiledJsonSchema>>,
    pub(crate) snapshot: SchemaRegistry,
}

/// Errors that can occur during startup load. Mapped by the daemon
/// startup harness to `LoomErrorCode::InternalError` with structured
/// `data` describing which method file failed.
#[derive(Debug)]
pub enum SchemaLoadError {
    DirectoryMissing { path: PathBuf },
    InvalidSchema { method: String, reason: String },
    EmptyDirectory { path: PathBuf },
}
