//! `schema_provider` — see crate root.
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

        let mut entries: Vec<_> = std::fs::read_dir(schema_dir)
            .map_err(|_e| SchemaLoadError::DirectoryMissing {
                path: schema_dir.clone(),
            })?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
            .collect();

        // Deterministic load order: read_dir iteration order is
        // filesystem-dependent, and `hasher_input` accumulates file
        // bytes in this order — sort by path so `source_wit_sha256` is
        // stable across machines, filesystems, and reloads.
        entries.sort_by_key(|e| e.path());

        if entries.is_empty() {
            return Err(SchemaLoadError::EmptyDirectory {
                path: schema_dir.clone(),
            });
        }

        for entry in entries {
            let path = entry.path();
            let contents =
                std::fs::read_to_string(&path).map_err(|e| SchemaLoadError::InvalidSchema {
                    method: path.display().to_string(),
                    reason: e.to_string(),
                })?;
            hasher_input.extend_from_slice(contents.as_bytes());

            let doc: serde_json::Value =
                serde_json::from_str(&contents).map_err(|e| SchemaLoadError::InvalidSchema {
                    method: path.display().to_string(),
                    reason: e.to_string(),
                })?;

            // Derive method name from the filename when the file body
            // doesn't carry an explicit `method` key.
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

            // Compile once, here, at load/SIGHUP-reload time. A schema
            // that does not compile fails the whole load (fail CLOSED):
            // a corrupted postinstall file must surface as a typed
            // startup error, not silently disable validation for its
            // method on the request hot path.
            let compiled_req = compile_or_fail("request", &method, &path, req.clone())?;
            let compiled_resp = compile_or_fail("response", &method, &path, resp.clone())?;

            request_schemas.insert(method.clone(), Arc::new(compiled_req));
            response_schemas.insert(method.clone(), Arc::new(compiled_resp));
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

        // Real SHA-256 (64 lowercase hex chars) of all schema file
        // bytes concatenated in path-sorted order — honest to the
        // field's `source_wit_sha256` wire name, and deterministic
        // across hosts and Rust releases (the previous 16-hex
        // DefaultHasher value was neither: SipHash is unspecified and
        // unstable, and read_dir order is filesystem-dependent). Uses
        // the workspace's content-addressing helper.
        let source_wit_sha256 = loom_core::content_store::sha256_hex(&hasher_input);

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

/// Compile one side (`request` / `response`) of a method's schema file,
/// mapping a compile failure to a logged, typed `SchemaLoadError` so the
/// daemon refuses to start (or refuses the SIGHUP reload) instead of
/// serving requests with validation silently disabled.
fn compile_or_fail(
    side: &str,
    method: &str,
    path: &std::path::Path,
    doc: serde_json::Value,
) -> Result<CompiledJsonSchema, SchemaLoadError> {
    CompiledJsonSchema::compile(doc).map_err(|reason| {
        tracing::error!(
            method,
            side,
            path = %path.display(),
            %reason,
            "stored JSON Schema does not compile — failing schema load (fail closed)"
        );
        SchemaLoadError::InvalidSchema {
            method: method.to_string(),
            reason: format!("{side} schema failed to compile: {reason}"),
        }
    })
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
