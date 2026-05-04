// Re-export of the locked Phase 5.3 interface. DO NOT EDIT here.
// Edit `systems/loom-cli/modules/SchemaCache/interfaces.rs` instead.
// SchemaCache — in-memory `HashMap<method_name, JsonSchema>`.
//
// # Contract semantics
// - **AC-ARCH-12.** Loads WIT-derived JSON Schemas from
//   `~/Library/Application Support/loom/schemas/v1/` at startup; the
//   pipeline that emits these schemas is `loom-tools/wit-to-json-schema`.
// - **Immutable after load.** No reload path. Each CLI invocation
//   reloads from disk.
// - **Shared by 4 consumers.** `ActionCommands` (arg validation),
//   `HelpGenerator` (help-text), `PrettyRenderer` (column derivation),
//   `ConfigResolver` (config validation).
// - **Missing dir.** `load` returns
//   `CliError::Internal("schema dir missing — run loom postinstall")`
//   when the schemas directory is absent.

use std::collections::HashMap;
use std::path::Path;

use crate::CliError;

/// In-memory schema map. Construction is via `load`; mutation is
/// impossible (the inner map is private and there are no `&mut self`
/// methods).
pub struct SchemaCache {
    pub(crate) schemas: HashMap<String, serde_json::Value>,
}

impl SchemaCache {
    /// Construct an empty cache. Used by tests; production code uses
    /// `load`.
    pub fn empty() -> Self {
        Self {
            schemas: HashMap::new(),
        }
    }

    /// Load every `*.json` schema file under `schemas_dir`. The file
    /// stem is the method name (e.g. `session.create.json` →
    /// `session.create`). Returns `CliError::Internal` if the dir is
    /// absent.
    pub fn load(schemas_dir: &Path) -> Result<Self, CliError> {
        if !schemas_dir.exists() {
            return Err(CliError::Internal(format!(
                "schemas not found at {} — run loom postinstall",
                schemas_dir.display()
            )));
        }
        let mut schemas = HashMap::new();
        for entry in std::fs::read_dir(schemas_dir).map_err(|e| {
            CliError::Internal(format!("cannot read schema dir: {}", e))
        })? {
            let entry = entry
                .map_err(|e| CliError::Internal(format!("schema dir entry error: {}", e)))?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if stem.is_empty() {
                continue;
            }
            let content = std::fs::read_to_string(&path).map_err(|e| {
                CliError::Internal(format!("cannot read schema file {}: {}", path.display(), e))
            })?;
            let value: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
                CliError::Internal(format!(
                    "cannot parse schema file {}: {}",
                    path.display(),
                    e
                ))
            })?;
            schemas.insert(stem, value);
        }
        Ok(Self { schemas })
    }

    /// Look up the request schema for `method`. Returns `None` if the
    /// method is not in the cache. Aliases are resolved to their
    /// canonical name via `loom_shared::action_aliases::canonicalise`
    /// before lookup, so `web.type_text` finds the schema stored under
    /// `web.type`.
    pub fn request_schema(&self, method: &str) -> Option<&serde_json::Value> {
        let canonical = loom_shared::action_aliases::canonicalise(method);
        self.schemas.get(canonical).and_then(|v| v.get("request"))
    }

    /// Look up the response schema for `method`. Used by
    /// `PrettyRenderer` for column derivation. Alias-aware (see
    /// `request_schema`).
    pub fn response_schema(&self, method: &str) -> Option<&serde_json::Value> {
        let canonical = loom_shared::action_aliases::canonicalise(method);
        self.schemas.get(canonical).and_then(|v| v.get("response"))
    }

    /// Iterate every method name visible to clients — canonical names
    /// stored on disk plus every alias whose canonical is loaded.
    /// Used by `HelpGenerator` for the global `--help` listing and by
    /// `validate_args`'s "unknown method: X. Available: Y, Z" message
    /// (so `web.type-text` rejection surfaces both `web.type` and
    /// `web.type_text` as accepted forms — AC-CLIROUTE-03).
    pub fn methods(&self) -> impl Iterator<Item = &str> + '_ {
        let canonicals = self.schemas.keys().map(String::as_str);
        let aliases = loom_shared::action_aliases::METHOD_ALIASES
            .iter()
            .filter_map(|(alias, canonical)| {
                if self.schemas.contains_key(*canonical) {
                    Some(*alias)
                } else {
                    None
                }
            });
        canonicals.chain(aliases)
    }

    /// Cache size — number of methods loaded.
    pub fn len(&self) -> usize {
        self.schemas.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.schemas.is_empty()
    }
}
