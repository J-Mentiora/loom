//! `host_service_adapter` — see crate root.
pub mod host_service_adapter;
pub use host_service_adapter::*;

pub mod wire_capture;

#[cfg(test)]
mod interface_tests;

#[async_trait::async_trait]
impl HostServiceAdapterApi for HostServiceAdapter {
    async fn dispatch_action(&self, action: Action) -> Result<Receipt, AdapterError> {
        // `WasmHostBridge::dispatch_action_blocking` uses block_in_place internally;
        // safe to call from an async context on a multi-thread tokio runtime.
        tokio::task::block_in_place(|| self.host.dispatch_action_blocking(action))
    }

    fn has_chromium(&self) -> bool {
        self.host.has_chromium()
    }
}
