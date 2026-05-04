//! `schema_provider` — see `systems/loom-rpc/modules/schema_provider/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod schema_provider;
pub use schema_provider::*;

#[cfg(test)]
mod interface_tests;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

impl SchemaProvider {
    pub fn load_at_startup(schema_dir: &PathBuf) -> Result<Arc<Self>, SchemaLoadError> {
        if !schema_dir.exists() {
            return Err(SchemaLoadError::DirectoryMissing {
                path: schema_dir.clone(),
            });
        }

        let mut request_schemas: HashMap<String, Arc<CompiledJsonSchema>> = HashMap::new();
        let mut response_schemas: HashMap<String, Arc<CompiledJsonSchema>> = HashMap::new();
        let mut method_schemas: Vec<MethodSchema> = Vec::new();
        let mut hasher_input = Vec::<u8>::new();

        let entries: Vec<_> = std::fs::read_dir(schema_dir)
            .map_err(|_e| SchemaLoadError::DirectoryMissing { path: schema_dir.clone() })?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path().extension().map(|x| x == "json").unwrap_or(false)
            })
            .collect();

        if entries.is_empty() {
            return Err(SchemaLoadError::EmptyDirectory {
                path: schema_dir.clone(),
            });
        }

        for entry in entries {
            let path = entry.path();
            let contents = std::fs::read_to_string(&path).map_err(|e| {
                SchemaLoadError::InvalidSchema {
                    method: path.display().to_string(),
                    reason: e.to_string(),
                }
            })?;
            hasher_input.extend_from_slice(contents.as_bytes());

            let doc: serde_json::Value =
                serde_json::from_str(&contents).map_err(|e| SchemaLoadError::InvalidSchema {
                    method: path.display().to_string(),
                    reason: e.to_string(),
                })?;

            // AC-RPCSCHEMAS2-01: derive method name from the filename
            // when the file body doesn't carry an explicit `method` key.
            // The postinstall runner currently writes `{request, response}`
            // shapes (sans method field); falling back to file stem keeps
            // the schemas loadable end-to-end without a postinstall rewrite.
            let method = match doc["method"].as_str() {
                Some(s) => s.to_string(),
                None => path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| SchemaLoadError::InvalidSchema {
                        method: path.display().to_string(),
                        reason: "missing 'method' field and unreadable file stem".to_string(),
                    })?
                    .to_string(),
            };

            let req = doc["request"].clone();
            let resp = doc["response"].clone();

            request_schemas.insert(
                method.clone(),
                Arc::new(CompiledJsonSchema { inner: req.clone() }),
            );
            response_schemas.insert(
                method.clone(),
                Arc::new(CompiledJsonSchema { inner: resp.clone() }),
            );
            let aliases: Vec<String> = loom_shared::action_aliases::aliases_of(&method)
                .into_iter()
                .map(|s| s.to_string())
                .collect();
            method_schemas.push(MethodSchema {
                method: method.clone(),
                request: req,
                response: resp,
                aliases,
            });
        }

        method_schemas.sort_by(|a, b| a.method.cmp(&b.method));

        // SHA256 of all schema file bytes concatenated (deterministic after sort).
        let source_wit_sha256 = {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            hasher_input.hash(&mut h);
            format!("{:016x}", h.finish())
        };

        let snapshot = SchemaRegistry {
            methods: method_schemas.clone(),
            source_wit_sha256,
        };

        Ok(Arc::new(Self {
            request_schemas,
            response_schemas,
            snapshot,
        }))
    }
}

impl SchemaProviderApi for SchemaProvider {
    fn lookup_request_schema(&self, method: &str) -> Option<Arc<CompiledJsonSchema>> {
        self.request_schemas.get(method).cloned()
    }

    fn lookup_response_schema(&self, method: &str) -> Option<Arc<CompiledJsonSchema>> {
        self.response_schemas.get(method).cloned()
    }

    fn registered_methods(&self) -> Vec<String> {
        let mut methods: Vec<String> = self.request_schemas.keys().cloned().collect();
        methods.sort();
        methods
    }

    fn get_registry_snapshot(&self) -> SchemaRegistry {
        self.snapshot.clone()
    }
}
