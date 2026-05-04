pub mod error_mapper;
pub use error_mapper::*;

#[cfg(test)]
mod interface_tests;

use loom_rpc::error::{LoomError, LoomErrorCode};

impl ErrorMapper {
    pub fn to_tool_result(err: LoomError) -> ToolResult {
        let msg = truncate_chars(&err.message, MAX_MESSAGE_CHARS);
        let receipt = TypedReceipt {
            code: err.code.as_wire().to_string(),
            message: msg,
            data: None,
            remediation: remediation_for(err.code),
        };
        ToolResult {
            is_error: true,
            content: vec![McpContent::Json {
                json: serde_json::to_value(&receipt).unwrap_or(serde_json::Value::Null),
            }],
        }
    }

    pub fn typed_receipt(code: LoomErrorCode) -> TypedReceipt {
        TypedReceipt {
            code: code.as_wire().to_string(),
            message: String::new(),
            data: None,
            remediation: remediation_for(code),
        }
    }

    pub fn from_rpc_io(detail: &str) -> LoomError {
        LoomError::new(LoomErrorCode::Io, detail)
    }

    pub fn from_hello_mismatch(detail: &str) -> LoomError {
        LoomError::new(LoomErrorCode::RpcAuthFailed, detail)
    }

    pub fn from_unknown_tool(tool_name: &str) -> LoomError {
        LoomError::new(
            LoomErrorCode::InvalidArgument,
            format!("unknown tool: {tool_name}"),
        )
    }

    pub fn from_schema_parse(detail: &str) -> LoomError {
        LoomError::new(LoomErrorCode::RpcInvalidRequest, detail)
    }
}

impl From<LoomError> for ToolResult {
    fn from(err: LoomError) -> Self {
        ErrorMapper::to_tool_result(err)
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    let mut chars = s.chars();
    let taken: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{taken}\u{2026}")
    } else {
        taken
    }
}

fn remediation_for(_code: LoomErrorCode) -> Option<String> {
    None
}
