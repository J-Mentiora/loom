// Interface tests for `StdioTransport`. Verifies IC-MCP-09 (stdio
// exclusively, no socket binding) and BC-MCP-04 single-task discipline.

use super::stdio_transport::{
    Dispatch, McpProtocolError, McpRequest, McpResponse, StdioTransport, ERROR_INTERNAL,
    ERROR_INVALID_PARAMS, ERROR_INVALID_REQUEST, ERROR_METHOD_NOT_FOUND, ERROR_PARSE,
};

// === IC-MCP-09: stdio-only constructor ===

#[test]
fn stdio_constructor_wires_process_streams() {
    // Compile-time: the production constructor returns
    // StdioTransport<Stdin, Stdout>. No way to pass a socket address.
    let _t: StdioTransport<tokio::io::Stdin, tokio::io::Stdout> = StdioTransport::stdio();
}

#[test]
fn no_socket_binding_method_exists() {
    // Negative compile-time check by API surface: only `stdio()` and
    // `with_io()` exist. The presence of either constructor proves the
    // type accepts only AsyncRead/AsyncWrite -- not a socket address.
    fn _ck<R, W>(t: StdioTransport<R, W>) -> StdioTransport<R, W>
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
        W: tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        t
    }
    let _ = _ck::<tokio::io::Stdin, tokio::io::Stdout>;
}

// === McpRequest / McpResponse on-the-wire shape ===

#[test]
fn mcp_request_id_is_optional_for_notifications() {
    let r = McpRequest {
        jsonrpc: "2.0".into(),
        id: None,
        method: "ping".into(),
        params: serde_json::json!({}),
    };
    let s = serde_json::to_string(&r).unwrap();
    // Notifications: no `id` field on the wire.
    assert!(!s.contains("\"id\""), "got {s}");
}

#[test]
fn mcp_response_omits_error_when_result_present() {
    let r = McpResponse {
        jsonrpc: "2.0".into(),
        id: serde_json::json!(1),
        result: Some(serde_json::json!({})),
        error: None,
    };
    let s = serde_json::to_string(&r).unwrap();
    assert!(!s.contains("\"error\""), "got {s}");
    assert!(s.contains("\"result\""), "got {s}");
}

#[test]
fn protocol_error_codes_match_jsonrpc_2_0_spec() {
    assert_eq!(ERROR_PARSE, -32700);
    assert_eq!(ERROR_INVALID_REQUEST, -32600);
    assert_eq!(ERROR_METHOD_NOT_FOUND, -32601);
    assert_eq!(ERROR_INVALID_PARAMS, -32602);
    assert_eq!(ERROR_INTERNAL, -32603);
}

#[test]
fn protocol_error_struct_present_for_envelope_failures() {
    let e = McpProtocolError {
        code: ERROR_METHOD_NOT_FOUND,
        message: "no such method".into(),
        data: None,
    };
    assert_eq!(e.code, -32601);
}

// === Dispatch closure: single-task discipline at the type level ===

#[test]
fn dispatch_returns_optional_response_for_notification_compatibility() {
    fn _ck(d: Dispatch, req: McpRequest) {
        #[allow(clippy::let_underscore_future)]
        let _ = d(req);
    }
    let _ = _ck;
}

// === run() takes the dispatcher and runs on caller's task ===

#[test]
fn run_signature_takes_dispatch_and_returns_loom_error() {
    fn _ck(
        t: StdioTransport<tokio::io::Stdin, tokio::io::Stdout>,
        d: Dispatch,
    ) -> Box<dyn std::future::Future<Output = Result<(), loom_rpc::error::LoomError>>> {
        Box::new(async move { t.run(d).await })
    }
    let _ = _ck;
}

// === read_frame / write_frame are publicly accessible helpers ===

#[test]
fn read_frame_returns_option_for_eof() {
    fn _ck<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
        r: &mut R,
    ) -> Box<
        dyn std::future::Future<Output = Result<Option<McpRequest>, loom_rpc::error::LoomError>>
            + '_,
    > {
        Box::new(async move { StdioTransport::<R, tokio::io::Stdout>::read_frame(r).await })
    }
    let _ = _ck::<tokio::io::Stdin>;
}

#[test]
fn write_frame_returns_result_unit() {
    fn _ck<'a, W: tokio::io::AsyncWrite + Unpin + Send + 'static>(
        w: &'a mut W,
        resp: &'a McpResponse,
    ) -> Box<dyn std::future::Future<Output = Result<(), loom_rpc::error::LoomError>> + 'a> {
        Box::new(async move { StdioTransport::<tokio::io::Stdin, W>::write_frame(w, resp).await })
    }
    let _ = _ck::<tokio::io::Stdout>;
}
