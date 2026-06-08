//! Local IPC for the optional background service: a query-only named-pipe
//! server/client carrying the `application::ipc` protocol. The service host and
//! SCM control land in later phases; this phase provides the transport.
//!
//! Staged foundation: these items are exercised by the in-process round-trip
//! test and wired into the service host + GUI client in the next phase, so the
//! `dead_code` allow is removed then. (See the approved background-service plan.)
#![allow(dead_code)]

pub mod framing;
pub mod pipe;
mod security;
