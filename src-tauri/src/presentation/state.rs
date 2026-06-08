//! Tauri-managed application state: the session's [`SearchBackend`] (the
//! background service if one is running, else in-process indexing), with search
//! and status routed through it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::application::{IndexStatus, SearchOptions, SearchResult};
use crate::presentation::backend::{SearchBackend, SearchHandle};

/// Shared state managed by Tauri and read by command handlers.
pub struct AppState {
    backend: SearchBackend,
    /// Monotonic generation for content searches; bumping it cancels any
    /// in-flight content scan so a superseded search stops reading files.
    content_gen: Arc<AtomicU64>,
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
            content_gen: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Start a new content-search generation, invalidating any in-flight one.
    pub fn next_content_gen(&self) -> u64 {
        self.content_gen.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// A handle to test (off-thread) whether a captured generation is current.
    pub fn content_gen_handle(&self) -> Arc<AtomicU64> {
        self.content_gen.clone()
    }

    /// Run a search against the active backend.
    pub fn search(&self, options: &SearchOptions) -> SearchResult {
        self.backend.search(options)
    }

    /// A detached handle to run a search off the async worker (content search).
    pub fn search_handle(&self) -> SearchHandle {
        self.backend.handle()
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
