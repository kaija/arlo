//! Default filesystem-backed [`SessionStore`] storing sessions under `~/.arlo`.
//!
//! Each session is one JSONL file at `<root>/<session_id>.jsonl` with one
//! [`Message`] per line. Appends are O(1) file appends; full rewrites go
//! through a temp file + atomic rename.

use std::path::PathBuf;

use async_trait::async_trait;

use crate::message::Message;
use crate::session_store::{SessionMeta, SessionStore, SessionStoreError};

/// Filesystem-backed session store. Default root is `~/.arlo/sessions/`.
#[derive(Debug, Clone)]
pub struct FsSessionStore {
    root: PathBuf,
}

impl FsSessionStore {
    /// Create a store rooted at `~/.arlo/sessions/`.
    ///
    /// Falls back to `.arlo/sessions` relative to the current directory if
    /// the home directory cannot be determined.
    pub fn new() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self::with_root(home.join(".arlo").join("sessions"))
    }

    /// Create a store rooted at an arbitrary directory (used in tests).
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Resolve and validate the file path for a session id.
    ///
    /// Rejects ids that could escape the root directory: empty ids, ids with
    /// path separators, or `.`/`..` components.
    fn path_for(&self, session_id: &str) -> Result<PathBuf, SessionStoreError> {
        let valid = !session_id.is_empty()
            && session_id != "."
            && session_id != ".."
            && !session_id.contains(['/', '\\'])
            && !session_id.contains('\0');
        if !valid {
            return Err(SessionStoreError::InvalidId {
                id: session_id.to_string(),
            });
        }
        Ok(self.root.join(format!("{session_id}.jsonl")))
    }

    fn serialize_lines(messages: &[Message]) -> Result<Vec<u8>, SessionStoreError> {
        let mut buf = Vec::new();
        for msg in messages {
            serde_json::to_writer(&mut buf, msg)?;
            buf.push(b'\n');
        }
        Ok(buf)
    }

    async fn ensure_root(&self) -> Result<(), SessionStoreError> {
        tokio::fs::create_dir_all(&self.root).await?;
        Ok(())
    }
}

impl Default for FsSessionStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Map io::ErrorKind::NotFound to SessionStoreError::NotFound for a given id.
fn map_not_found(err: std::io::Error, id: &str) -> SessionStoreError {
    if err.kind() == std::io::ErrorKind::NotFound {
        SessionStoreError::NotFound { id: id.to_string() }
    } else {
        SessionStoreError::Storage(err)
    }
}

#[async_trait]
impl SessionStore for FsSessionStore {
    async fn append(
        &self,
        session_id: &str,
        messages: &[Message],
    ) -> Result<(), SessionStoreError> {
        let path = self.path_for(session_id)?;
        self.ensure_root().await?;
        let buf = Self::serialize_lines(messages)?;
        let mut options = tokio::fs::OpenOptions::new();
        options.create(true).append(true);
        let mut file = options.open(&path).await?;
        tokio::io::AsyncWriteExt::write_all(&mut file, &buf).await?;
        Ok(())
    }

    async fn save(&self, session_id: &str, messages: &[Message]) -> Result<(), SessionStoreError> {
        let path = self.path_for(session_id)?;
        self.ensure_root().await?;
        let buf = Self::serialize_lines(messages)?;
        // Write to a temp file then rename so readers never see a partial file.
        let tmp = path.with_extension("jsonl.tmp");
        tokio::fs::write(&tmp, &buf).await?;
        tokio::fs::rename(&tmp, &path).await?;
        Ok(())
    }

    async fn load(&self, session_id: &str) -> Result<Vec<Message>, SessionStoreError> {
        let path = self.path_for(session_id)?;
        let data = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| map_not_found(e, session_id))?;
        let mut messages = Vec::new();
        for line in data.lines() {
            if line.trim().is_empty() {
                continue;
            }
            messages.push(serde_json::from_str(line)?);
        }
        Ok(messages)
    }

    async fn list(&self) -> Result<Vec<SessionMeta>, SessionStoreError> {
        let mut sessions = Vec::new();
        let mut entries = match tokio::fs::read_dir(&self.root).await {
            Ok(entries) => entries,
            // No root directory yet means no sessions.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(sessions),
            Err(e) => return Err(e.into()),
        };
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let updated_at = entry
                .metadata()
                .await?
                .modified()
                .unwrap_or(std::time::UNIX_EPOCH);
            sessions.push(SessionMeta {
                id: id.to_string(),
                updated_at,
            });
        }
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(sessions)
    }

    async fn delete(&self, session_id: &str) -> Result<(), SessionStoreError> {
        let path = self.path_for(session_id)?;
        tokio::fs::remove_file(&path)
            .await
            .map_err(|e| map_not_found(e, session_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::ContentBlock;

    fn user_msg(text: &str) -> Message {
        Message::User {
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    fn store() -> (tempfile::TempDir, FsSessionStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = FsSessionStore::with_root(dir.path());
        (dir, store)
    }

    #[tokio::test]
    async fn append_then_load_roundtrip() {
        let (_dir, store) = store();
        let msgs = vec![user_msg("hello"), user_msg("world")];
        store.append("s1", &msgs).await.unwrap();
        store.append("s1", &[user_msg("again")]).await.unwrap();
        let loaded = store.load("s1").await.unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0], msgs[0]);
        assert_eq!(loaded[2], user_msg("again"));
    }

    #[tokio::test]
    async fn save_overwrites() {
        let (_dir, store) = store();
        store
            .append("s1", &[user_msg("a"), user_msg("b")])
            .await
            .unwrap();
        store.save("s1", &[user_msg("compacted")]).await.unwrap();
        let loaded = store.load("s1").await.unwrap();
        assert_eq!(loaded, vec![user_msg("compacted")]);
    }

    #[tokio::test]
    async fn load_missing_is_not_found() {
        let (_dir, store) = store();
        assert!(matches!(
            store.load("nope").await,
            Err(SessionStoreError::NotFound { .. })
        ));
    }

    #[tokio::test]
    async fn delete_removes_session() {
        let (_dir, store) = store();
        store.append("s1", &[user_msg("x")]).await.unwrap();
        store.delete("s1").await.unwrap();
        assert!(matches!(
            store.load("s1").await,
            Err(SessionStoreError::NotFound { .. })
        ));
        assert!(matches!(
            store.delete("s1").await,
            Err(SessionStoreError::NotFound { .. })
        ));
    }

    #[tokio::test]
    async fn list_returns_sessions_newest_first() {
        let (_dir, store) = store();
        assert!(store.list().await.unwrap().is_empty());
        store.append("older", &[user_msg("x")]).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        store.append("newer", &[user_msg("y")]).await.unwrap();
        let sessions = store.list().await.unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, "newer");
        assert_eq!(sessions[1].id, "older");
    }

    #[tokio::test]
    async fn rejects_path_traversal_ids() {
        let (_dir, store) = store();
        for bad in ["", ".", "..", "../evil", "a/b", "a\\b", "x\0y"] {
            assert!(
                matches!(
                    store.append(bad, &[user_msg("x")]).await,
                    Err(SessionStoreError::InvalidId { .. })
                ),
                "id {bad:?} should be rejected"
            );
        }
    }
}
