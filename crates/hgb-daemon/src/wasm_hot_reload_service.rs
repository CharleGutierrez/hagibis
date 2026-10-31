//! # WebAssembly Plugin Hot-Reload Service (`wasm_hot_reload_service`)
//!
//! Production daemon service managing ambient WASM module watchers, dynamic capability hot-swapping,
//! and RPC management interfaces for the Hagibis resident daemon (`hgbd`).

use std::path::Path;
use std::sync::Arc;
use hgb_core::error::Result;
use hgb_core::wasm_plugin_hot_reloader::{HotReloadReport, WasmPluginHotReloader};

/// Resident Daemon Service wrapping `WasmPluginHotReloader`
#[derive(Clone)]
pub struct WasmHotReloadService {
    reloader: Arc<WasmPluginHotReloader>,
}

impl WasmHotReloadService {
    /// Initialize with an underlying `WasmPluginHotReloader`
    pub fn new(reloader: Arc<WasmPluginHotReloader>) -> Self {
        Self { reloader }
    }

    /// Access reference to the underlying reloader
    pub fn reloader(&self) -> &Arc<WasmPluginHotReloader> {
        &self.reloader
    }

    /// Start ambient background filesystem watcher
    pub fn start(&self) -> Result<tokio::task::JoinHandle<()>> {
        self.reloader.start_watcher()
    }

    /// Stop ambient background watcher
    pub fn stop(&self) {
        self.reloader.stop();
    }

    /// Fetch current hot reload telemetry status
    pub async fn status(&self) -> HotReloadReport {
        self.reloader.status().await
    }

    /// Manually trigger immediate scan or individual module reload
    pub async fn trigger_reload(&self, module_path: Option<&Path>) -> Result<HotReloadReport> {
        if let Some(path) = module_path {
            self.reloader.reload_file(path).await?;
        } else {
            self.reloader.scan_and_reload().await?;
        }
        Ok(self.status().await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hgb_core::wasm_hot_pool::WasmHotPool;

    #[tokio::test]
    async fn test_wasm_hot_reload_service_lifecycle() {
        let tmp = std::env::temp_dir().join(format!("hgb_test_hot_reload_svc_{}", uuid::Uuid::new_v4()));
        let plugin_dir = tmp.join("wasm_plugins");
        std::fs::create_dir_all(&plugin_dir).unwrap();

        let hot_pool = Arc::new(WasmHotPool::new(2).unwrap());
        let reloader = Arc::new(WasmPluginHotReloader::new(hot_pool, vec![plugin_dir.clone()]));
        let service = WasmHotReloadService::new(reloader);

        let initial_status = service.status().await;
        assert_eq!(initial_status.active_plugins, 0);

        let report = service.trigger_reload(None).await.unwrap();
        assert_eq!(report.active_plugins, 0);

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
