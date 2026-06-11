pub mod stdio_transport;
pub use stdio_transport::*;

#[cfg(test)]
mod interface_tests;

use crate::error_mapper::ErrorMapper;
use loom_rpc::error::LoomError;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

impl StdioTransport<tokio::io::Stdin, tokio::io::Stdout> {
    pub fn stdio() -> Self {
        Self {
            reader: tokio::io::stdin(),
            writer: tokio::io::stdout(),
        }
    }
}

impl<R, W> StdioTransport<R, W>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    pub fn with_io(reader: R, writer: W) -> Self {
        Self { reader, writer }
    }

    /// Drive the NDJSON request loop until stdin EOF.
    ///
    /// Requests are dispatched CONCURRENTLY — one task per frame, bounded
    /// by `MAX_CONCURRENT_REQUESTS` — so a long `tools/call` (a slow page
    /// navigate) no longer head-of-line blocks `ping`/`initialize`
    /// (audit 2026-06-10). MCP correlates responses by `id`, so
    /// out-of-order responses are legal. Stdout stays well-formed because
    /// a single writer task owns the writer: dispatch tasks hand finished
    /// responses to it over an mpsc channel and frames are written whole,
    /// never interleaved.
    ///
    /// On EOF the loop stops reading, drops its channel sender, and awaits
    /// the writer task — which drains every already-dispatched response
    /// (the in-flight tasks hold sender clones) before the transport
    /// returns. Cancellation via `mcp_main::serve_until_shutdown` is
    /// unchanged: dropping this future abandons the loop immediately while
    /// the detached tasks die with the process.
    pub async fn run(self, dispatch: Dispatch) -> Result<(), LoomError> {
        let mut buf_reader = BufReader::new(self.reader);
        let mut writer = self.writer;
        let mut line = String::new();

        let (tx, mut rx) = tokio::sync::mpsc::channel::<McpResponse>(MAX_CONCURRENT_REQUESTS);
        let writer_task: tokio::task::JoinHandle<Result<(), LoomError>> =
            tokio::spawn(async move {
                while let Some(resp) = rx.recv().await {
                    Self::write_frame(&mut writer, &resp).await?;
                }
                Ok(())
            });
        let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_REQUESTS));

        let read_result: Result<(), LoomError> = loop {
            line.clear();
            let n = match buf_reader.read_line(&mut line).await {
                Ok(n) => n,
                Err(e) => break Err(ErrorMapper::from_rpc_io(&e.to_string())),
            };

            if n == 0 {
                // EOF
                break Ok(());
            }
            if n == 1 && line == "\n" {
                tracing::warn!("blank MCP frame, skipping");
                continue;
            }

            let req = match serde_json::from_str::<McpRequest>(&line) {
                Ok(r) => r,
                Err(e) => {
                    let err_resp = McpResponse {
                        jsonrpc: "2.0".into(),
                        id: serde_json::Value::Null,
                        result: None,
                        error: Some(McpProtocolError {
                            code: ERROR_PARSE,
                            message: format!("parse error: {e}"),
                            data: None,
                        }),
                    };
                    // A send failure means the writer task died (stdout
                    // closed/broken): the client is gone, stop serving.
                    if tx.send(err_resp).await.is_err() {
                        break Err(ErrorMapper::from_rpc_io("stdout writer closed"));
                    }
                    continue;
                }
            };

            // Backpressure: at MAX_CONCURRENT_REQUESTS in-flight dispatches,
            // pause reading until one completes. The semaphore is never
            // closed, so acquire_owned can only fail if it were — treat
            // that defensively as shutdown.
            let Ok(permit) = semaphore.clone().acquire_owned().await else {
                break Ok(());
            };
            let dispatch = dispatch.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                let _permit = permit;
                if let Some(resp) = dispatch(req).await {
                    // Writer gone (stdout closed) — nothing to do but drop
                    // the response; the read loop notices on its next send.
                    let _ = tx.send(resp).await;
                }
            });
        };

        // Stop accepting new frames but drain in-flight ones: the writer
        // task exits once every sender clone (loop + dispatch tasks) drops.
        drop(tx);
        match writer_task.await {
            Ok(write_result) => read_result.and(write_result),
            Err(join_err) => read_result.and(Err(ErrorMapper::from_rpc_io(&join_err.to_string()))),
        }
    }

    pub async fn write_frame(writer: &mut W, response: &McpResponse) -> Result<(), LoomError> {
        let mut json = serde_json::to_string(response)
            .map_err(|e| ErrorMapper::from_rpc_io(&e.to_string()))?;
        json.push('\n');
        writer
            .write_all(json.as_bytes())
            .await
            .map_err(|e| ErrorMapper::from_rpc_io(&e.to_string()))?;
        Ok(())
    }

    pub async fn read_frame(reader: &mut R) -> Result<Option<McpRequest>, LoomError> {
        let mut buf_reader = BufReader::new(&mut *reader);
        let mut line = String::new();
        let n = buf_reader
            .read_line(&mut line)
            .await
            .map_err(|e| ErrorMapper::from_rpc_io(&e.to_string()))?;
        if n == 0 {
            return Ok(None); // EOF
        }
        if n == 1 && line == "\n" {
            return Ok(None); // blank line treated as EOF in single-frame helper
        }
        let req = serde_json::from_str(&line)
            .map_err(|e| ErrorMapper::from_schema_parse(&e.to_string()))?;
        Ok(Some(req))
    }
}
