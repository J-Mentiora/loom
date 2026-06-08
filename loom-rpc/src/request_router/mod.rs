//! `request_router` — see crate root.
pub mod request_router;
pub use request_router::*;

#[cfg(test)]
mod interface_tests;

use crate::error_translator::error_translator::{JsonRpcError, LoomErrorCode};
use crate::host_service_adapter::host_service_adapter::Action;
use crate::rpc_handlers::rpc_handlers::RpcHandlers;
use crate::schema_provider::schema_provider::SchemaProviderApi;
use crate::schema_validator::schema_validator::SchemaValidatorApi;
use jsonrpsee::RpcModule;
use std::sync::Arc;

impl RequestRouter {
    pub fn register_methods(
        handlers: Arc<RpcHandlers>,
        schemas: Arc<dyn SchemaProviderApi>,
        validator: Arc<dyn SchemaValidatorApi>,
    ) -> Result<Arc<Self>, RegistrationError> {
        let mut methods = schemas.registered_methods();
        if !methods.contains(&"rpc.schemas".to_string()) {
            methods.push("rpc.schemas".to_string());
        }
        // gc.run is a router-level (not schema-driven) method —
        // ensure it is enumerated by `methods()` so callers (CLI / MCP /
        // observability startup audit) see it.
        if !methods.contains(&"gc.run".to_string()) {
            methods.push("gc.run".to_string());
        }
        // import.playwright is router-level (not in the
        // schema-driven action enumeration) — ensure callers see it via
        // `rpc.schemas`.
        if !methods.contains(&"import.playwright".to_string()) {
            methods.push("import.playwright".to_string());
        }
        // session.kill, daemon.health, request.cancel — admin/control
        // RPCs added in the daemon-session-stall fix. Router-level
        // (no schema in registry) so enumerate explicitly.
        for m in [
            "session.kill",
            "session.reap",
            "daemon.health",
            "request.cancel",
        ] {
            if !methods.contains(&m.to_string()) {
                methods.push(m.to_string());
            }
        }
        methods.sort();

        let ctx = Arc::new(RouterContext {
            handlers: Arc::clone(&handlers),
            validator: Arc::clone(&validator),
        });
        let module = Arc::new(RpcModule::from_arc(Arc::clone(&ctx)));

        Ok(Arc::new(Self {
            module,
            ctx,
            methods,
        }))
    }
}

#[async_trait::async_trait]
impl RequestRouterApi for RequestRouter {
    async fn dispatch(&self, method: &str, params: serde_json::Value) -> Vec<u8> {
        use crate::core_service_adapter::core_service_adapter::{
            CreateSessionParams, GrantParams, VaultAddParams, VaultDeleteSecretParams,
            VaultListLabelsParams, VaultSetSecretParams,
        };
        match method {
            "health.ping" => canonical_bytes(&serde_json::json!({"status": "ok"})),
            "rpc.schemas" => match self.ctx.handlers.rpc_schemas().await {
                Ok(reg) => canonical_bytes(&reg),
                Err(e) => error_bytes(&e),
            },
            "session.create" => {
                let p: CreateSessionParams = match serde_json::from_value(params) {
                    Ok(v) => v,
                    Err(e) => {
                        let err = JsonRpcError {
                            code: LoomErrorCode::SchemaViolation,
                            message: format!("invalid params: {e}"),
                            data: None,
                        };
                        return error_bytes(&err);
                    }
                };
                match self.ctx.handlers.session_create(p).await {
                    Ok(v) => canonical_bytes(&v),
                    Err(e) => error_bytes(&e),
                }
            }
            "session.list" => match self.ctx.handlers.session_list().await {
                Ok(v) => canonical_bytes(&v),
                Err(e) => error_bytes(&e),
            },
            "session.inspect" => {
                let sid = params
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let at = params.get("at_action").and_then(|v| v.as_u64());
                match self.ctx.handlers.session_inspect(sid, at).await {
                    Ok(v) => canonical_bytes(&v),
                    Err(e) => error_bytes(&e),
                }
            }
            "session.close" => {
                let sid = params
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                match self.ctx.handlers.session_close(sid).await {
                    Ok(v) => canonical_bytes(&v),
                    Err(e) => error_bytes(&e),
                }
            }
            "session.abort" => {
                let sid = params
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let reason = params
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("user-initiated")
                    .to_string();
                match self.ctx.handlers.session_abort(sid, reason).await {
                    Ok(v) => canonical_bytes(&v),
                    Err(e) => error_bytes(&e),
                }
            }
            "session.kill" => {
                let sid = params
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                match self.ctx.handlers.session_kill(sid).await {
                    Ok(v) => canonical_bytes(&v),
                    Err(e) => error_bytes(&e),
                }
            }
            "daemon.health" => {
                let deep = params
                    .get("deep")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                match self.ctx.handlers.daemon_health(deep).await {
                    Ok(v) => canonical_bytes(&v),
                    Err(e) => error_bytes(&e),
                }
            }
            "session.replay" => {
                let sid = params
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let speed = params
                    .get("speed")
                    .and_then(|v| v.as_f64())
                    .map(|f| f as f32);
                let nm = params
                    .get("network_mode")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                match self.ctx.handlers.session_replay(sid, speed, nm).await {
                    Ok(v) => canonical_bytes(&v),
                    Err(e) => error_bytes(&e),
                }
            }
            "session.diff" => {
                let a = params
                    .get("a")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let b = params
                    .get("b")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let inc_sc = params
                    .get("include_screenshots")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let show_dom = params
                    .get("show_dom_diffs")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                match self.ctx.handlers.session_diff(a, b, inc_sc, show_dom).await {
                    Ok(v) => canonical_bytes(&v),
                    Err(e) => error_bytes(&e),
                }
            }
            "session.export" => {
                let sid = params
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let fmt = params
                    .get("format")
                    .and_then(|v| v.as_str())
                    .unwrap_or("json")
                    .to_string();
                match self.ctx.handlers.session_export(sid, fmt).await {
                    Ok(v) => canonical_bytes(&v),
                    Err(e) => error_bytes(&e),
                }
            }
            "session.validate" => {
                let sid = params
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                match self.ctx.handlers.session_validate(sid).await {
                    Ok(v) => canonical_bytes(&v),
                    Err(e) => error_bytes(&e),
                }
            }
            "import.playwright" => {
                let trace_hex = params
                    .get("trace_hex")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                match self.ctx.handlers.import_playwright(trace_hex).await {
                    Ok(v) => canonical_bytes(&v),
                    Err(e) => error_bytes(&e),
                }
            }
            "vault.grant" => {
                let p: GrantParams = match serde_json::from_value(params) {
                    Ok(v) => v,
                    Err(e) => {
                        let err = JsonRpcError {
                            code: LoomErrorCode::SchemaViolation,
                            message: format!("invalid params: {e}"),
                            data: None,
                        };
                        return error_bytes(&err);
                    }
                };
                match self.ctx.handlers.vault_grant(p).await {
                    Ok(v) => canonical_bytes(&v),
                    Err(e) => error_bytes(&e),
                }
            }
            "vault.revoke" => {
                let grant_id = match required_str(&params, "grant_id") {
                    Ok(v) => v,
                    Err(e) => return error_bytes(&e),
                };
                let reason = params
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("user-initiated")
                    .to_string();
                match self.ctx.handlers.vault_revoke(grant_id, reason).await {
                    Ok(v) => canonical_bytes(&v),
                    Err(e) => error_bytes(&e),
                }
            }
            "vault.list_grants" => {
                let sid = params
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                match self.ctx.handlers.vault_list_grants(sid).await {
                    Ok(v) => canonical_bytes(&v),
                    Err(e) => error_bytes(&e),
                }
            }
            "vault.add" => {
                let p: VaultAddParams = match serde_json::from_value(params) {
                    Ok(v) => v,
                    Err(e) => {
                        let err = JsonRpcError {
                            code: LoomErrorCode::SchemaViolation,
                            message: format!("invalid params: {e}"),
                            data: None,
                        };
                        return error_bytes(&err);
                    }
                };
                match self.ctx.handlers.vault_add(p).await {
                    Ok(v) => canonical_bytes(&v),
                    Err(e) => error_bytes(&e),
                }
            }
            "vault.set_secret" => {
                let p: VaultSetSecretParams = match serde_json::from_value(params) {
                    Ok(v) => v,
                    Err(e) => {
                        let err = JsonRpcError {
                            code: LoomErrorCode::SchemaViolation,
                            message: format!("invalid params: {e}"),
                            data: None,
                        };
                        return error_bytes(&err);
                    }
                };
                match self.ctx.handlers.vault_set_secret(p).await {
                    Ok(v) => canonical_bytes(&v),
                    Err(e) => error_bytes(&e),
                }
            }
            "vault.delete_secret" => {
                let p: VaultDeleteSecretParams = match serde_json::from_value(params) {
                    Ok(v) => v,
                    Err(e) => {
                        let err = JsonRpcError {
                            code: LoomErrorCode::SchemaViolation,
                            message: format!("invalid params: {e}"),
                            data: None,
                        };
                        return error_bytes(&err);
                    }
                };
                match self.ctx.handlers.vault_delete_secret(p).await {
                    Ok(v) => canonical_bytes(&v),
                    Err(e) => error_bytes(&e),
                }
            }
            "vault.list_labels" => {
                let p: VaultListLabelsParams = serde_json::from_value(params)
                    .unwrap_or(VaultListLabelsParams { session_id: None });
                match self.ctx.handlers.vault_list_labels(p).await {
                    Ok(v) => canonical_bytes(&v),
                    Err(e) => error_bytes(&e),
                }
            }
            "vault.diagnose" => match self.ctx.handlers.vault_diagnose().await {
                Ok(v) => canonical_bytes(&v),
                Err(e) => error_bytes(&e),
            },
            // v0.9.6 web-cookie-injection: session-context lookup for
            // `loom vault add --credential-type cookie` binding (D5).
            "vault.get_session_context" => {
                match self.ctx.handlers.vault_get_session_context().await {
                    Ok(v) => canonical_bytes(&v),
                    Err(e) => error_bytes(&e),
                }
            }
            "content.get" => {
                let artifact_ref = params
                    .get("artifact_ref")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                match self.ctx.handlers.content_get(artifact_ref).await {
                    Ok(v) => canonical_bytes(&v),
                    Err(e) => error_bytes(&e),
                }
            }
            "gc.run" => {
                let ttl_days = params.get("ttl_days").and_then(|v| v.as_u64());
                let store_max_bytes = params.get("store_max_bytes").and_then(|v| v.as_u64());
                match self.ctx.handlers.gc_run(ttl_days, store_max_bytes).await {
                    Ok(v) => canonical_bytes(&v),
                    Err(e) => error_bytes(&e),
                }
            }
            "session.reap" => {
                // Safe by default: preview unless the caller explicitly opts in.
                let dry_run = params
                    .get("dry_run")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                match self.ctx.handlers.session_reap(dry_run).await {
                    Ok(v) => canonical_bytes(&v),
                    Err(e) => error_bytes(&e),
                }
            }
            method if is_surface_verb(loom_shared::action_aliases::canonicalise(method)) => {
                // Canonicalise alias method names (e.g. `web.type_text` →
                // `web.type`) so the parse_action match arm and downstream
                // observability see only the canonical form
                // (matches the wire discipline for canonical method names).
                let canonical = loom_shared::action_aliases::canonicalise(method);
                match parse_action(canonical, params) {
                    Ok(action) => match self.ctx.handlers.action_dispatch(action).await {
                        Ok(receipt) => canonical_bytes(&receipt),
                        Err(e) => error_bytes(&e),
                    },
                    Err(e) => error_bytes(&e),
                }
            }
            other => {
                let err = JsonRpcError {
                    code: LoomErrorCode::MethodNotFound,
                    message: format!("method not found: {}", other),
                    data: None,
                };
                error_bytes(&err)
            }
        }
    }

    fn methods(&self) -> Vec<String> {
        self.methods.clone()
    }
}

fn canonical_bytes<T: serde::Serialize>(val: &T) -> Vec<u8> {
    match RpcHandlers::serialise_canonical(val) {
        Ok(s) => s.into_bytes(),
        Err(e) => error_bytes(&e),
    }
}

/// Encode a `JsonRpcError` as the wire bytes the connection handler
/// can detect and wrap in a JSON-RPC 2.0 error envelope. The error
/// is tagged with `"__loom_rpc_error": true` so `handle_request`
/// can distinguish it from a success payload without heuristics.
fn error_bytes(err: &JsonRpcError) -> Vec<u8> {
    let mut v = serde_json::to_value(err).unwrap_or_default();
    if let serde_json::Value::Object(ref mut m) = v {
        m.insert(
            "__loom_rpc_error".to_string(),
            serde_json::Value::Bool(true),
        );
    }
    serde_json::to_vec(&v).unwrap_or_default()
}

impl RequestRouter {
    /// Expose the inner `RpcModule` for McpAdapter reuse.
    pub fn module(&self) -> Arc<RpcModule<RouterContext>> {
        Arc::clone(&self.module)
    }
}

/// Returns true if `method` matches the `<surface>.<verb>` pattern
/// for a known action surface.
fn is_surface_verb(method: &str) -> bool {
    const SURFACES: &[&str] = &["web", "shell", "fs", "api", "native"];
    method
        .split_once('.')
        .map(|(s, _)| SURFACES.contains(&s))
        .unwrap_or(false)
}

/// Extract the session_id from params. Accepts both "session_id" (internal
/// convention) and "session" (postinstall schema convention) for compatibility.
fn session_id_from_params(params: &serde_json::Value) -> Result<String, JsonRpcError> {
    params
        .get("session_id")
        .or_else(|| params.get("session"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| JsonRpcError {
            code: LoomErrorCode::SchemaViolation,
            message: "missing field: session_id (or session)".to_string(),
            data: None,
        })
}

fn required_str(params: &serde_json::Value, field: &str) -> Result<String, JsonRpcError> {
    params
        .get(field)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| JsonRpcError {
            code: LoomErrorCode::SchemaViolation,
            message: format!("missing field: {field}"),
            data: None,
        })
}

/// settle-capture: the readiness modes accepted by `until`.
pub const SETTLE_MODES: &[&str] = &["load", "networkidle", "settled"];

/// Validate the optional `until` readiness mode. Absent → `Ok(None)` (the
/// daemon applies the `settled` default). Present but not one of
/// [`SETTLE_MODES`] → `SchemaViolation` (fail loud, don't silently coerce).
fn optional_settle_until(params: &serde_json::Value) -> Result<Option<String>, JsonRpcError> {
    match params.get("until") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) if SETTLE_MODES.contains(&s.as_str()) => {
            Ok(Some(s.clone()))
        }
        Some(other) => Err(JsonRpcError {
            code: LoomErrorCode::SchemaViolation,
            message: format!("invalid `until`: expected one of {SETTLE_MODES:?}, got {other}"),
            data: None,
        }),
    }
}

/// The set of parameters extracted via `required_str` / `required_*`
/// for each registered action — i.e. the params the router will reject
/// the request for if missing.
///
/// Mirror of the match-arm structure in [`parse_action`]; kept in this
/// file so the two cannot drift. The action registry's
/// `registry_required_flags_match_router` test asserts this set equals
/// the registry's required-param set for every method.
pub fn router_required_params(method: &str) -> Option<&'static [&'static str]> {
    Some(match method {
        "web.navigate" => &["session_id", "url"],
        "web.click" => &["session_id", "selector"],
        "web.type" => &["session_id", "selector", "text"],
        "web.select" => &["session_id", "selector", "value"],
        "web.hover" => &["session_id", "selector"],
        "web.scroll" => &["session_id", "selector"],
        "web.wait" => &["session_id", "selector"],
        "web.wait_for" => &["session_id"],
        "web.evaluate" => &["session_id", "expression"],
        "web.set_input_files" => &["session_id", "selector", "paths"],
        "web.screenshot" => &["session_id"],
        "web.snapshot" => &["session_id"],
        // v0.9.6 web-cookie-injection.
        "web.set_cookies" => &["session_id", "source"],
        "web.get_cookies" => &["session_id"],
        "web.clear_cookies" => &["session_id"],
        "web.delete_cookies" => &["session_id", "name"],
        _ => return None,
    })
}

/// All known router method names — the set parsable by [`parse_action`].
pub fn known_router_methods() -> &'static [&'static str] {
    &[
        "web.clear_cookies",
        "web.click",
        "web.delete_cookies",
        "web.evaluate",
        "web.get_cookies",
        "web.hover",
        "web.navigate",
        "web.screenshot",
        "web.scroll",
        "web.select",
        "web.set_cookies",
        "web.set_input_files",
        "web.snapshot",
        "web.type",
        "web.wait",
        "web.wait_for",
    ]
}

/// Parse a `<surface>.<verb>` method + JSON params into a typed `Action`
/// enum. Returns `JsonRpcError::SchemaViolation` with the field name on
/// parse failure.
///
/// Params arriving here are always in the flat schema shape — the
/// `connection_handler` runs `unwrap_sdk_envelope` before validation,
/// so the SDK envelope shape never reaches dispatch.
fn parse_action(method: &str, params: serde_json::Value) -> Result<Action, JsonRpcError> {
    match method {
        "web.navigate" => {
            let session_id = session_id_from_params(&params)?;
            let url = required_str(&params, "url")?;
            // settle-capture: optional readiness gate for the auto-capture.
            let until = optional_settle_until(&params)?;
            let timeout_ms = params.get("timeout_ms").and_then(|v| v.as_u64());
            Ok(Action::WebNavigate {
                session_id,
                url,
                until,
                timeout_ms,
            })
        }
        "web.click" => {
            let session_id = session_id_from_params(&params)?;
            let selector = required_str(&params, "selector")?;
            Ok(Action::WebClick {
                session_id,
                selector,
            })
        }
        // Canonical name `web.type` (was `web.type_text`); the legacy
        // spelling is rewritten upstream by `dispatch` via
        // `loom_shared::action_aliases::canonicalise`.
        "web.type" => {
            let session_id = session_id_from_params(&params)?;
            let selector = required_str(&params, "selector")?;
            let text = required_str(&params, "text")?;
            Ok(Action::WebType {
                session_id,
                selector,
                text,
            })
        }
        "web.select" => {
            let session_id = session_id_from_params(&params)?;
            let selector = required_str(&params, "selector")?;
            let value = required_str(&params, "value")?;
            Ok(Action::WebSelect {
                session_id,
                selector,
                value,
            })
        }
        "web.hover" => {
            let session_id = session_id_from_params(&params)?;
            let selector = required_str(&params, "selector")?;
            Ok(Action::WebHover {
                session_id,
                selector,
            })
        }
        "web.scroll" => {
            let session_id = session_id_from_params(&params)?;
            let selector = required_str(&params, "selector")?;
            let delta_x = params.get("delta_x").and_then(|v| v.as_i64());
            let delta_y = params.get("delta_y").and_then(|v| v.as_i64());
            Ok(Action::WebScroll {
                session_id,
                selector,
                delta_x,
                delta_y,
            })
        }
        "web.wait" => {
            let session_id = session_id_from_params(&params)?;
            let selector = required_str(&params, "selector")?;
            let timeout_ms = params.get("timeout_ms").and_then(|v| v.as_u64());
            Ok(Action::WebWait {
                session_id,
                selector,
                timeout_ms,
            })
        }
        "web.wait_for" => {
            let session_id = session_id_from_params(&params)?;
            // settle-capture: same validated readiness mode as web.navigate.
            let until = optional_settle_until(&params)?;
            let timeout_ms = params.get("timeout_ms").and_then(|v| v.as_u64());
            Ok(Action::WebWaitFor {
                session_id,
                until,
                timeout_ms,
            })
        }
        "web.evaluate" => {
            let session_id = session_id_from_params(&params)?;
            let expression = required_str(&params, "expression")?;
            Ok(Action::WebEvaluate {
                session_id,
                expression,
            })
        }
        "web.set_input_files" => {
            let session_id = session_id_from_params(&params)?;
            let selector = required_str(&params, "selector")?;
            // `paths` is a required JSON array of strings (absolute host paths).
            let paths_arr = params
                .get("paths")
                .and_then(|v| v.as_array())
                .ok_or_else(|| JsonRpcError {
                    code: LoomErrorCode::SchemaViolation,
                    message: "missing field: paths".to_string(),
                    data: None,
                })?;
            // Reject (don't silently drop) any non-string entry — a malformed
            // `paths` array is a schema violation, not "upload the strings I
            // could parse".
            let mut paths = Vec::with_capacity(paths_arr.len());
            for entry in paths_arr {
                match entry.as_str() {
                    Some(s) => paths.push(s.to_string()),
                    None => {
                        return Err(JsonRpcError {
                            code: LoomErrorCode::SchemaViolation,
                            message: "paths entries must be strings".to_string(),
                            data: None,
                        })
                    }
                }
            }
            Ok(Action::WebSetInputFiles {
                session_id,
                selector,
                paths,
            })
        }
        "web.screenshot" => {
            let session_id = session_id_from_params(&params)?;
            let selector = params
                .get("selector")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            Ok(Action::WebScreenshot {
                session_id,
                selector,
            })
        }
        "web.snapshot" => {
            let session_id = session_id_from_params(&params)?;
            Ok(Action::WebSnapshot { session_id })
        }
        // v0.9.6 web-cookie-injection.
        "web.set_cookies" => {
            let session_id = session_id_from_params(&params)?;
            let source = params.get("source").cloned().ok_or_else(|| JsonRpcError {
                code: LoomErrorCode::SchemaViolation,
                message: "missing field: source".to_string(),
                data: None,
            })?;
            Ok(Action::WebSetCookies { session_id, source })
        }
        "web.get_cookies" => {
            let session_id = session_id_from_params(&params)?;
            let urls = params.get("urls").and_then(|v| v.as_array()).map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            });
            Ok(Action::WebGetCookies { session_id, urls })
        }
        "web.clear_cookies" => {
            let session_id = session_id_from_params(&params)?;
            Ok(Action::WebClearCookies { session_id })
        }
        "web.delete_cookies" => {
            let session_id = session_id_from_params(&params)?;
            let name = required_str(&params, "name")?;
            let url = params
                .get("url")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let domain = params
                .get("domain")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let path = params
                .get("path")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            Ok(Action::WebDeleteCookies {
                session_id,
                name,
                url,
                domain,
                path,
            })
        }
        other => Err(JsonRpcError {
            code: LoomErrorCode::MethodNotFound,
            message: format!("method not found: {other}"),
            data: None,
        }),
    }
}
