//! Session history store abstraction: the `SessionStore` trait and its types.
//!
//! A session is an append-mostly sequence of [`Message`]s identified by a
//! session id. The trait is storage-agnostic; the default implementation is
//! [`crate::fs_session_store::FsSessionStore`] backed by `~/.arlo/sessions/`.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::SystemTime;
use thiserror::Error;

use crate::message::Message;

/// Summary metadata for a stored session, returned by [`SessionStore::list`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMeta {
    /// The session identifier.
    pub id: String,
    /// When the session was last written to.
    pub updated_at: SystemTime,
}

/// Errors returned by SessionStore operations.
#[derive(Error, Debug)]
pub enum SessionStoreError {
    /// The specified session id does not exist.
    #[error("session not found: {id}")]
    NotFound { id: String },

    /// The session id contains characters unsafe for the backing store
    /// (e.g. path separators for a filesystem store).
    #[error("invalid session id: {id}")]
    InvalidId { id: String },

    /// A message could not be (de)serialized.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// An underlying storage failure (I/O, network, ...).
    #[error("storage error: {0}")]
    Storage(#[from] std::io::Error),
}

/// Storage abstraction for persisting conversation history across runs.
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Append messages to a session, creating it if it does not exist.
    async fn append(&self, session_id: &str, messages: &[Message])
        -> Result<(), SessionStoreError>;

    /// Replace a session's entire history (e.g. after compaction),
    /// creating it if it does not exist.
    async fn save(&self, session_id: &str, messages: &[Message]) -> Result<(), SessionStoreError>;

    /// Load a session's full history. Returns `NotFound` for unknown ids.
    async fn load(&self, session_id: &str) -> Result<Vec<Message>, SessionStoreError>;

    /// List stored sessions, most recently updated first.
    async fn list(&self) -> Result<Vec<SessionMeta>, SessionStoreError>;

    /// Delete a session. Returns `NotFound` for unknown ids.
    async fn delete(&self, session_id: &str) -> Result<(), SessionStoreError>;
}
