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

    pub async fn run(self, dispatch: Dispatch) -> Result<(), LoomError> {
        let mut buf_reader = BufReader::new(self.reader);
        let mut writer = self.writer;
        let mut line = String::new();

        loop {
            line.clear();
            let n = buf_reader
                .read_line(&mut line)
                .await
                .map_err(|e| ErrorMapper::from_rpc_io(&e.to_string()))?;

            if n == 0 {
                // EOF
                break;
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
                    Self::write_frame(&mut writer, &err_resp).await?;
                    continue;
                }
            };

            if let Some(resp) = dispatch(req).await {
                Self::write_frame(&mut writer, &resp).await?;
            }
        }
        Ok(())
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
