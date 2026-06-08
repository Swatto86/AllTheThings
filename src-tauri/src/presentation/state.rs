//! Tauri-managed application state: the session's [`SearchBackend`] (the
//! background service if one is running, else in-process indexing), with search
//! and status routed through it.

use crate::application::{IndexStatus, SearchOptions, SearchResult};
use crate::presentation::backend::SearchBackend;

/// Shared state managed by Tauri and read by command handlers.
pub struct AppState {
    backend: SearchBackend,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    /// Detect the backend (probe for a service, else start in-process indexing).
    pub fn new() -> Self {
        Self {
            backend: SearchBackend::detect(),
        }
    }

    /// Run a search against the active backend.
    pub fn search(&self, options: &SearchOptions) -> SearchResult {
        self.backend.search(options)
    }

    /// Current index status for the UI.
    pub fn status(&self) -> IndexStatus {
        self.backend.status()
    }

    /// Whether searches are served by the background service (vs. in-process).
    pub fn uses_service(&self) -> bool {
        self.backend.uses_service()
    }
}
