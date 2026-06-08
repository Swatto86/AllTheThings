//! Index lifecycle status, surfaced to the UI via a Tauri command.

use serde::{Deserialize, Serialize};

/// A snapshot of the index's state for the frontend status bar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexStatus {
    /// One of `"indexing"`, `"ready"`, `"error"`.
    pub state: String,
    pub count: usize,
    pub volume: String,
    pub message: String,
}

impl IndexStatus {
    pub fn indexing(volume: impl Into<String>, count: usize) -> Self {
        Self {
            state: "indexing".into(),
            count,
            volume: volume.into(),
            message: String::new(),
        }
    }

    pub fn ready(volume: impl Into<String>, count: usize) -> Self {
        Self {
            state: "ready".into(),
            count,
            volume: volume.into(),
            message: String::new(),
        }
    }

    pub fn error(volume: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            state: "error".into(),
            count: 0,
            volume: volume.into(),
            message: message.into(),
        }
    }
}
