//! Tauri-managed application state: a thin wrapper over the platform-level
//! [`Indexer`], exposing the shared catalog and a status snapshot to commands.

use std::sync::Arc;

use parking_lot::RwLock;

use crate::application::{Catalog, IndexStatus};
use crate::infrastructure::indexing::Indexer;

/// Shared state managed by Tauri and read by command handlers.
pub struct AppState {
    /// Shared catalog handle (searched by the `search`/`export_results` commands).
    pub catalog: Arc<RwLock<Catalog>>,
    indexer: Indexer,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn new() -> Self {
        let indexer = Indexer::new();
        let catalog = indexer.catalog();
        Self { catalog, indexer }
    }

    /// Start indexing every fixed NTFS volume in the background.
    pub fn start_indexing(&self) {
        self.indexer.start();
    }

    /// Current status for the UI.
    pub fn status(&self) -> IndexStatus {
        self.indexer.status()
    }
}
