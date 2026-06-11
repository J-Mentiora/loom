//! `host_service_adapter` — see crate root.
pub mod host_service_adapter;
pub use host_service_adapter::*;

pub mod wire_capture;

#[cfg(test)]
mod interface_tests;

use crate::error_translator::error_translator::LoomErrorCode;
use std::sync::Arc;

#[async_trait::async_trait]
impl HostServiceAdapterApi for HostServiceAdapter {
    async fn dispatch_action(&self, action: Action) -> Result<Receipt, AdapterError> {
        // Run the blocking bridge call on tokio's blocking pool so this
        // future stays poll-able: the per-request timeout/cancel arms in
        // `handle_request`'s select! can preempt a slow or wedged
        // dispatch. (The previous `block_in_place` ran the whole action
        // inside this future's poll, so the select! could never fire —
        // the 30 s server deadline was unreachable for the exact hang
        // case it was built for.)
        //
        // If the select! abandons this future (timeout/cancel), the
        // blocking work keeps running DETACHED until it completes; the
        // per-session dispatch slot inside the daemon's
        // `dispatch_action_blocking` fences later requests on the same
        // session (typed `too_many_requests`) until the abandoned work
        // has actually finished, so its late side effects can't
        // interleave with a newer action's.
        let host = Arc::clone(&self.host);
        match tokio::task::spawn_blocking(move || host.dispatch_action_blocking(action)).await {
            Ok(result) => result,
            // JoinError: the blocking task panicked (or the runtime is
            // shutting down). Surface a typed internal error rather
            // than propagating the panic into the connection task.
            Err(join_err) => {
                tracing::error!(
                    error = %join_err,
                    "host dispatch blocking task failed to join"
                );
                Err(LoomErrorCode::InternalError)
            }
        }
    }

    fn has_chromium(&self) -> bool {
        self.host.has_chromium()
    }
}
