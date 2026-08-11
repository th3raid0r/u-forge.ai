//! SQLite-backed chat history storage.
//!
//! Stores chat sessions and their messages in `<db_path>/chat_history.db`.
//! Each session has a title (auto-derived from the first user message) and
//! an ordered list of messages preserving role, tool call metadata, etc.

use anyhow::{Context, Result};
use chrono::Utc;
use parking_lot::Mutex;
use rusqlite::{Connection, params};
use std::path::Path;
use std::sync::Arc;
use uuid::Uuid;

// ── Schema ───────────────────────────────────────────────────────────────────

const CHAT_SCHEMA: &str = r#"
PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;

CREATE TABLE IF NOT EXISTS chat_sessions (
    id         TEXT PRIMARY KEY,
    title      TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS chat_messages (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id       TEXT NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
    ordering         INTEGER NOT NULL,
    role             TEXT NOT NULL,
    text             TEXT NOT NULL,
    tool_args        TEXT,
    tool_result      TEXT,
    tool_internal_id TEXT,
    collapsed        INTEGER NOT NULL DEFAULT 1
);

CREATE INDEX IF NOT EXISTS idx_messages_session ON chat_messages(session_id, ordering);
"#;

// ── Types ────────────────────────────────────────────────────────────────────

/// Summary of a chat session for the history list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChatSessionSummary {
    pub id: String,
    pub title: String,
    #[allow(dead_code)]
    pub updated_at: String,
}

/// Message role — typed counterpart to the `role` column in `chat_messages`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StoredRole {
    User,
    Assistant,
    Thinking,
    ToolCall,
}

impl StoredRole {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            StoredRole::User => "user",
            StoredRole::Assistant => "assistant",
            StoredRole::Thinking => "thinking",
            StoredRole::ToolCall => "tool_call",
        }
    }

    pub(crate) fn from_str(s: &str) -> Self {
        match s {
            "user" => StoredRole::User,
            "thinking" => StoredRole::Thinking,
            "tool_call" => StoredRole::ToolCall,
            _ => StoredRole::Assistant,
        }
    }
}

/// Tool call metadata — present only when `StoredChatMessage.role == ToolCall`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredToolCall {
    /// Stable correlation ID matching the `ToolCallStart` event.
    pub internal_id: String,
    /// Pretty-printed JSON arguments.
    pub args: String,
    /// Result text, filled in when the tool returns.
    pub result: Option<String>,
    /// Whether the tool call body is collapsed in the UI.
    pub collapsed: bool,
}

/// A stored chat message — the persistence-layer source of truth.
///
/// The UI layer (`ChatMessageView`) converts to/from this type at the
/// persistence boundary only (`from_stored` / `to_stored`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredChatMessage {
    pub role: StoredRole,
    pub text: String,
    /// `Some` only when `role == StoredRole::ToolCall`.
    pub tool_call: Option<StoredToolCall>,
}

// ── ChatHistoryStore ─────────────────────────────────────────────────────────

/// Thread-safe handle to the chat history database.
#[derive(Clone)]
pub(crate) struct ChatHistoryStore {
    conn: Arc<Mutex<Connection>>,
}

impl ChatHistoryStore {
    /// Open (or create) the chat history database at `<db_path>/chat_history.db`.
    pub fn open(db_path: &Path) -> Result<Self> {
        let total_start = std::time::Instant::now();
        let directory_start = std::time::Instant::now();
        std::fs::create_dir_all(db_path)
            .with_context(|| format!("creating db directory: {}", db_path.display()))?;
        let directory_duration_us = directory_start.elapsed().as_micros() as u64;

        let db_file = db_path.join("chat_history.db");
        let open_start = std::time::Instant::now();
        let conn = Connection::open(&db_file)
            .with_context(|| format!("opening chat history db: {}", db_file.display()))?;
        let open_duration_us = open_start.elapsed().as_micros() as u64;
        let schema_start = std::time::Instant::now();
        conn.execute_batch(CHAT_SCHEMA)
            .context("initializing chat history schema")?;
        tracing::info!(
            db_path = %db_file.display(),
            directory_duration_us,
            open_duration_us,
            schema_duration_us = schema_start.elapsed().as_micros() as u64,
            duration_us = total_start.elapsed().as_micros() as u64,
            "Chat history store opened"
        );

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Create a new empty session. Returns the session ID.
    pub fn create_session(&self, title: &str) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO chat_sessions (id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?4)",
            params![id, title, now, now],
        )?;
        Ok(id)
    }

    /// List all sessions, most-recently-updated first.
    pub fn list_sessions(&self) -> Result<Vec<ChatSessionSummary>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT id, title, updated_at FROM chat_sessions ORDER BY updated_at DESC")?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ChatSessionSummary {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    updated_at: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Load all messages for a session, ordered.
    pub fn load_messages(&self, session_id: &str) -> Result<Vec<StoredChatMessage>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT role, text, tool_args, tool_result, tool_internal_id, collapsed \
             FROM chat_messages WHERE session_id = ?1 ORDER BY ordering",
        )?;
        let rows = stmt
            .query_map(params![session_id], |row| {
                let role_str: String = row.get(0)?;
                let role = StoredRole::from_str(&role_str);
                let tool_call = if role == StoredRole::ToolCall {
                    let internal_id: Option<String> = row.get(4)?;
                    Some(StoredToolCall {
                        internal_id: internal_id.unwrap_or_default(),
                        args: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        result: row.get(3)?,
                        collapsed: row.get::<_, i32>(5)? != 0,
                    })
                } else {
                    None
                };
                Ok(StoredChatMessage {
                    role,
                    text: row.get(1)?,
                    tool_call,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Save (replace) all messages for a session and update its title/timestamp.
    pub fn save_session(
        &self,
        session_id: &str,
        title: &str,
        messages: &[StoredChatMessage],
    ) -> Result<()> {
        let mut conn = self.conn.lock();
        let now = Utc::now().to_rfc3339();
        let transaction = conn.transaction()?;

        transaction.execute(
            "UPDATE chat_sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
            params![title, now, session_id],
        )?;

        // Replace all messages atomically.
        transaction.execute(
            "DELETE FROM chat_messages WHERE session_id = ?1",
            params![session_id],
        )?;
        let mut insert = transaction.prepare(
            "INSERT INTO chat_messages \
             (session_id, ordering, role, text, tool_args, tool_result, tool_internal_id, collapsed) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for (i, msg) in messages.iter().enumerate() {
            let (tool_args, tool_result, tool_internal_id, collapsed) =
                if let Some(tc) = &msg.tool_call {
                    (
                        Some(tc.args.clone()),
                        tc.result.clone(),
                        Some(tc.internal_id.clone()),
                        tc.collapsed,
                    )
                } else {
                    (None, None, None, false)
                };
            insert.execute(params![
                session_id,
                i as i64,
                msg.role.as_str(),
                msg.text,
                tool_args,
                tool_result,
                tool_internal_id,
                if collapsed { 1i32 } else { 0i32 },
            ])?;
        }
        drop(insert);
        transaction.commit()?;
        Ok(())
    }

    /// Delete a session and all its messages.
    pub fn delete_session(&self, session_id: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "DELETE FROM chat_sessions WHERE id = ?1",
            params![session_id],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn session_round_trip_preserves_message_order_and_tool_state() {
        let temp_dir = TempDir::new().unwrap();
        let store = ChatHistoryStore::open(temp_dir.path()).unwrap();
        let session_id = store.create_session("First title").unwrap();
        let messages = vec![
            StoredChatMessage {
                role: StoredRole::User,
                text: "Tell me about the observatory.".into(),
                tool_call: None,
            },
            StoredChatMessage {
                role: StoredRole::Thinking,
                text: "Find the location before answering.".into(),
                tool_call: None,
            },
            StoredChatMessage {
                role: StoredRole::ToolCall,
                text: "search_world".into(),
                tool_call: Some(StoredToolCall {
                    internal_id: "tool-1".into(),
                    args: r#"{"query":"observatory"}"#.into(),
                    result: Some("The old observatory".into()),
                    collapsed: false,
                }),
            },
            StoredChatMessage {
                role: StoredRole::Assistant,
                text: "The old observatory overlooks the northern pass.".into(),
                tool_call: None,
            },
        ];

        store
            .save_session(&session_id, "The old observatory", &messages)
            .unwrap();

        assert_eq!(store.load_messages(&session_id).unwrap(), messages);
        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, session_id);
        assert_eq!(sessions[0].title, "The old observatory");
    }

    #[test]
    fn deleting_a_session_cascades_to_its_messages() {
        let temp_dir = TempDir::new().unwrap();
        let store = ChatHistoryStore::open(temp_dir.path()).unwrap();
        let session_id = store.create_session("Temporary").unwrap();
        store
            .save_session(
                &session_id,
                "Temporary",
                &[StoredChatMessage {
                    role: StoredRole::Assistant,
                    text: "This will be removed.".into(),
                    tool_call: None,
                }],
            )
            .unwrap();

        store.delete_session(&session_id).unwrap();

        assert!(store.list_sessions().unwrap().is_empty());
        assert!(store.load_messages(&session_id).unwrap().is_empty());
    }
}
