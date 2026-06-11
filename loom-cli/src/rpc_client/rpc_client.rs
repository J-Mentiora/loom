// RpcClient — jsonrpsee Unix-socket client owned by `loom-cli`.
//
// Separate from loom-mcp's RpcClient — the CLI carries its own client
// so the no-MCP-framing rule remains structural.
//
// # Contract semantics
// - **Wire protocol.** JSON-RPC 2.0 over Unix domain socket. HELLO
//   token handshake before any method dispatch.
// - **Fresh connection per invocation.** No pooling; the CLI is a
//   single-shot process. `call` opens, HELLOs, sends, awaits, closes.
// - **Auth via `AuthManager`.** `RpcClient::call` reads the HELLO
//   token from `AuthManager::read_hello_token()` (per-startup
//   artefact) at the resolved `auth_dir` (config.toml `auth_dir` /
//   `LOOM_AUTH_DIR` per `ConfigResolver` precedence). Token never
//   persisted across daemon restarts.
// - **Retry policy.** Default = no retry; one connect
//   retry on `ECONNREFUSED` after 100 ms (covers daemon
//   just-restarted race). Anything else fails fast through
//   `ErrorMapper`.
// - **`From<RpcError> for CliError`** mirrors `LoomErrorCode` 1:1;
//   enforced by `tools/lint-error-codes.py`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

use crate::CliError;

/// Configuration consumed by `RpcClient::connect`.
#[derive(Debug, Clone)]
pub struct RpcClientConfig {
    /// Resolved Unix socket path (per `ConfigResolver` precedence).
    pub socket_path: PathBuf,
    /// Resolved auth artefact dir (`hello.token` + `daemon.pid`), per
    /// `ConfigResolver` precedence. Must point where the daemon writes
    /// its `<data_root>/auth/` artefacts.
    pub auth_dir: PathBuf,
    /// Request timeout. Default 30s.
    pub request_timeout: Duration,
}

/// Typed wire error used by `From<RpcError> for CliError`.
/// 1:1 mirror of `loom-rpc::error::LoomErrorCode`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// The `loom-cli`-owned JSON-RPC client. Holds a fresh per-invocation
/// connection. Drop closes the socket.
pub struct RpcClient {
    pub(crate) config: RpcClientConfig,
}

impl RpcClient {
    /// Construct an unconnected client. Use `connect` to open the
    /// socket; use `call` for one-shot request/response flows.
    pub fn new(config: RpcClientConfig) -> Self {
        Self { config }
    }

    /// Open the Unix socket (one `ECONNREFUSED` retry) and complete the
    /// HELLO handshake. Returns the authenticated stream — `connect`
    /// discards it, `call` reuses it for the request/response exchange.
    ///
    /// HELLO semantics: timeout on the read-back = accepted; any frame or
    /// EOF before the timeout = rejected (`AuthFailed`).
    async fn open_and_hello(&self) -> Result<tokio::net::UnixStream, CliError> {
        // Resolved `auth_dir` (file + LOOM_AUTH_DIR env per ConfigResolver),
        // not the hardcoded platform default — custom `--data-root` daemon
        // setups read the token from where their daemon actually wrote it.
        let auth = crate::auth_manager::AuthManager::new(crate::auth_manager::auth_paths_in(
            &self.config.auth_dir,
        ));
        let token = auth.read_hello_token()?;
        let mut stream = match tokio::net::UnixStream::connect(&self.config.socket_path).await {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                tokio::net::UnixStream::connect(&self.config.socket_path)
                    .await
                    .map_err(|_| {
                        CliError::Connection(crate::error_mapper::ConnectionError::DaemonNotRunning)
                    })?
            }
            Err(_) => {
                return Err(CliError::Connection(
                    crate::error_mapper::ConnectionError::DaemonNotRunning,
                ))
            }
        };
        send_frame(&mut stream, format!("HELLO {token}").as_bytes())
            .await
            .map_err(|_| {
                CliError::Connection(crate::error_mapper::ConnectionError::DaemonNotRunning)
            })?;
        // Timeout = accepted; any frame or EOF before timeout = rejected.
        let hello = tokio::time::timeout(Duration::from_millis(50), recv_frame(&mut stream)).await;
        if hello.is_ok() {
            return Err(CliError::Connection(
                crate::error_mapper::ConnectionError::AuthFailed,
            ));
        }
        Ok(stream)
    }

    /// Open the Unix-socket connection and complete the HELLO
    /// handshake. Single connect retry on `ECONNREFUSED`.
    pub async fn connect(&self) -> Result<(), CliError> {
        self.open_and_hello().await.map(|_| ())
    }

    /// Issue exactly one JSON-RPC call. Opens a fresh connection,
    /// HELLOs, sends, awaits response, closes. Returns the raw
    /// `serde_json::Value` receipt for `OutputFormatter`.
    pub async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, CliError> {
        let mut stream = self.open_and_hello().await?;
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": 1,
        });
        let req_bytes =
            serde_json::to_vec(&request).map_err(|e| CliError::Internal(e.to_string()))?;
        send_frame(&mut stream, &req_bytes).await.map_err(|_| {
            CliError::Connection(crate::error_mapper::ConnectionError::ConnectionTimeout)
        })?;
        let resp_bytes = tokio::time::timeout(self.config.request_timeout, recv_frame(&mut stream))
            .await
            .map_err(|_| {
                CliError::Connection(crate::error_mapper::ConnectionError::ConnectionTimeout)
            })?
            .map_err(|_| {
                CliError::Connection(crate::error_mapper::ConnectionError::DaemonNotRunning)
            })?;
        let response: serde_json::Value =
            serde_json::from_slice(&resp_bytes).map_err(|e| CliError::Internal(e.to_string()))?;
        if let Some(error) = response.get("error") {
            let rpc_error: RpcError = serde_json::from_value(error.clone())
                .map_err(|e| CliError::Internal(e.to_string()))?;
            return Err(rpc_error.into());
        }
        Ok(response
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null))
    }

    /// Daemon-ping helper used by `DoctorRunner` (check 2).
    pub async fn ping(&self) -> Result<(), CliError> {
        self.call("health.ping", serde_json::json!({}))
            .await
            .map(|_| ())
    }
}

/// Maps the daemon's wire error `code` (snake_case, see
/// `loom_shared::LoomErrorCode::as_wire`) to a `CliError` for `?` ergonomics.
impl From<RpcError> for CliError {
    fn from(e: RpcError) -> Self {
        match e.code.as_str() {
            // Daemon HELLO/auth failure (auth_middleware emits
            // `LoomErrorCode::ProtocolAuthRequired` → wire `protocol_auth_required`).
            // Was the dead kebab literal `"rpc-auth-failed"`, which the daemon never
            // emits — fixed during error-code-consolidation.
            "protocol_auth_required" => {
                CliError::Connection(crate::error_mapper::ConnectionError::AuthFailed)
            }
            // TODO(error-codes): the daemon has no dedicated "RPC schema/version
            // skew" wire code today — the only producer is a test mock, and the
            // generic `schema_violation` also covers per-field validation, so
            // routing it here would mis-label those as "reinstall". Left matching
            // the (currently-unemitted) `rpc_schema_violation` pending a dedicated
            // version-skew code; tracked in the cleanup backlog.
            "rpc_schema_violation" => {
                CliError::Connection(crate::error_mapper::ConnectionError::SchemaVersionSkew)
            }
            "io" => CliError::Connection(crate::error_mapper::ConnectionError::DaemonNotRunning),
            // surface_unavailable is a distinct error class (exit 5).
            "surface_unavailable" => CliError::SurfaceUnavailable(e.message.clone()),
            // browser_not_found is a distinct error class (exit 1)
            // with a fixed actionable install message rendered by Display.
            // Wire string is snake_case per loom-rpc's rename_all; the shared
            // canonical kebab-case `"browser-not-found"` is for non-RPC sinks.
            "browser_not_found" => CliError::BrowserNotFound(e.message.clone()),
            _ => CliError::Receipt(serde_json::json!({
                "code": e.code,
                "message": e.message,
                "data": e.data,
            })),
        }
    }
}

/// Write a length-prefixed frame: 4-byte big-endian length + payload.
async fn send_frame(stream: &mut tokio::net::UnixStream, data: &[u8]) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let len = (data.len() as u32).to_be_bytes();
    stream.write_all(&len).await?;
    stream.write_all(data).await
}

/// Read a length-prefixed frame: 4-byte big-endian length, then payload.
async fn recv_frame(stream: &mut tokio::net::UnixStream) -> std::io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    Ok(buf)
}
