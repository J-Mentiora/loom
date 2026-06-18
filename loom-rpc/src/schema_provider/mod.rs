//! `schema_provider` — see crate root.
pub mod schema_provider;
pub use schema_provider::*;

#[cfg(test)]
mod interface_tests;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

impl SchemaProvider {
    /// Build the registry from the binary's embedded `BUILTIN_SCHEMAS` —
    /// no disk involved. Binary version == schema version, always: a
    /// v0.11.x daemon can never validate `web.navigate` against a v0.9.x
    /// schema (the stale-mirror regression this replaced disk-first
    /// loading over). A builtin that fails to compile is a programming
    /// error surfaced as the same typed fail-closed `SchemaLoadError`.
    pub fn load_embedded() -> Result<Arc<Self>, SchemaLoadError> {
        Self::load_embedded_with_overlay(None).map(|(provider, _)| provider)
    }

    /// Embedded baseline + optional disk overlay.
    ///
    /// - Builtin methods ALWAYS validate against the embedded schema.
    ///   A disk file for a builtin whose content differs is recorded as a
    ///   [`StaleMirror`] and ignored (the caller logs the remediation hint —
    ///   `loom postinstall` refreshes mirrors).
    /// - Disk files for methods NOT in the embedded set are compiled
    ///   fail-closed and added (operator-extension escape hatch, unchanged
    ///   from the disk-first loader).
    pub fn load_embedded_with_overlay(
        schema_dir: Option<&std::path::Path>,
    ) -> Result<(Arc<Self>, Vec<StaleMirror>), SchemaLoadError> {
        let mut request_schemas: HashMap<String, Arc<CompiledJsonSchema>> = HashMap::new();
        let mut response_schemas: HashMap<String, Arc<CompiledJsonSchema>> = HashMap::new();
        let mut method_schemas: Vec<MethodSchema> = Vec::new();
        let mut hasher_input = Vec::<u8>::new();
        let mut stale: Vec<StaleMirror> = Vec::new();

        // Embedded baseline. BUILTIN_SCHEMAS is method-sorted at the source;
        // hash input follows that order so `source_wit_sha256` is identical
        // across machines for stock installs.
        for (method, json_str) in loom_shared::builtin_schemas::BUILTIN_SCHEMAS {
            hasher_input.extend_from_slice(json_str.as_bytes());
            let doc: serde_json::Value =
                serde_json::from_str(json_str).map_err(|e| SchemaLoadError::InvalidSchema {
                    method: method.to_string(),
                    reason: format!("embedded schema is invalid JSON: {e}"),
                })?;
            let embedded_path = std::path::Path::new("<embedded>");
            register_method_schema(
                method,
                &doc,
                embedded_path,
                &mut request_schemas,
                &mut response_schemas,
                &mut method_schemas,
            )?;
        }

        // Disk overlay: extras compile fail-closed; builtin mismatches are
        // detected, recorded, and ignored. A missing/empty dir is normal
        // (fresh machine before postinstall) — the embedded baseline already
        // covers every builtin method.
        if let Some(dir) = schema_dir {
            let mut entries: Vec<_> = std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
                .collect();
            entries.sort_by_key(|e| e.path());

            for entry in entries {
                let path = entry.path();
                let contents =
                    std::fs::read_to_string(&path).map_err(|e| SchemaLoadError::InvalidSchema {
                        method: path.display().to_string(),
                        reason: e.to_string(),
                    })?;
                let method = match path.file_stem().and_then(|s| s.to_str()) {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                if let Some((_, builtin)) = loom_shared::builtin_schemas::BUILTIN_SCHEMAS
                    .iter()
                    .find(|(m, _)| *m == method)
                {
                    if contents != *builtin {
                        stale.push(StaleMirror {
                            method: method.clone(),
                            path: path.clone(),
                        });
                    }
                    continue;
                }
                hasher_input.extend_from_slice(contents.as_bytes());
                let doc: serde_json::Value = serde_json::from_str(&contents).map_err(|e| {
                    SchemaLoadError::InvalidSchema {
                        method: path.display().to_string(),
                        reason: e.to_string(),
                    }
                })?;
                register_method_schema(
                    &method,
                    &doc,
                    &path,
                    &mut request_schemas,
                    &mut response_schemas,
                    &mut method_schemas,
                )?;
            }
        }

        method_schemas.sort_by(|a, b| a.method.cmp(&b.method));
        let source_wit_sha256 = loom_core::content_store::sha256_hex(&hasher_input);
        let snapshot = SchemaRegistry {
            methods: method_schemas,
            source_wit_sha256,
        };

        Ok((
            Arc::new(Self {
                request_schemas,
                response_schemas,
                snapshot,
            }),
            stale,
        ))
    }

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

            // Compile once, here, at load/SIGHUP-reload time. A schema
            // that does not compile fails the whole load (fail CLOSED):
            // a corrupted postinstall file must surface as a typed
            // startup error, not silently disable validation for its
            // method on the request hot path.
            register_method_schema(
                &method,
                &doc,
                &path,
                &mut request_schemas,
                &mut response_schemas,
                &mut method_schemas,
            )?;
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

/// Compile + register one method's request/response schemas into the
/// in-progress registry maps — the shared tail of all three load paths
/// (embedded baseline, disk overlay, `load_at_startup`). Each caller owns
/// method derivation, hashing, and stale-mirror handling; this helper is
/// deliberately divergence-free so it cannot perturb `hasher_input`
/// (NFR-DET-01: the `source_wit_sha256` byte stream is caller-controlled).
fn register_method_schema(
    method: &str,
    doc: &serde_json::Value,
    path: &std::path::Path,
    request_schemas: &mut HashMap<String, Arc<CompiledJsonSchema>>,
    response_schemas: &mut HashMap<String, Arc<CompiledJsonSchema>>,
    method_schemas: &mut Vec<MethodSchema>,
) -> Result<(), SchemaLoadError> {
    let req = doc["request"].clone();
    let resp = doc["response"].clone();
    let compiled_req = compile_or_fail("request", method, path, req.clone())?;
    let compiled_resp = compile_or_fail("response", method, path, resp.clone())?;
    request_schemas.insert(method.to_string(), Arc::new(compiled_req));
    response_schemas.insert(method.to_string(), Arc::new(compiled_resp));
    let aliases: Vec<String> = loom_shared::action_aliases::aliases_of(method)
        .into_iter()
        .map(|s| s.to_string())
        .collect();
    method_schemas.push(MethodSchema {
        method: method.to_string(),
        request: req,
        response: resp,
        aliases,
    });
    Ok(())
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
