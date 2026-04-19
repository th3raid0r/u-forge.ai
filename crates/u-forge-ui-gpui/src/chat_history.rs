//! SQLite-backed chat history storage.
//!
//! Stores chat sessions and their messages in `<db_path>/chat_history.db`.
//! Each session has a title (auto-derived from the first user message) and
//! an ordered list of messages preserving role, tool call metadata, etc.

use anyhow::{Context, Result};
use chrono::Utc;
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Arc;
use uuid::Uuid;

// ── Schema ───────────────────────────────────────────────────────────────────

const CHAT_SCHEMA: &str = r#"
PRAGMA journal_mode=WAL;

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
#[derive(Debug, Clone)]
pub(crate) struct ChatSessionSummary {
    pub id: String,
    pub title: String,
    #[allow(dead_code)]
    pub updated_at: String,
}

/// A stored chat message (mirrors `ChatEntry` fields).
#[derive(Debug, Clone)]
pub(crate) struct StoredMessage {
    pub role: String,
    pub text: String,
    pub tool_args: Option<String>,
    pub tool_result: Option<String>,
    pub tool_internal_id: Option<String>,
    pub collapsed: bool,
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
        std::fs::create_dir_all(db_path)
            .with_context(|| format!("creating db directory: {}", db_path.display()))?;

        let db_file = db_path.join("chat_history.db");
        let conn = Connection::open(&db_file)
            .with_context(|| format!("opening chat history db: {}", db_file.display()))?;
        conn.execute_batch(CHAT_SCHEMA)
            .context("initializing chat history schema")?;

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
        let mut stmt = conn.prepare(
            "SELECT id, title, updated_at FROM chat_sessions ORDER BY updated_at DESC",
        )?;
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
    pub fn load_messages(&self, session_id: &str) -> Result<Vec<StoredMessage>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT role, text, tool_args, tool_result, tool_internal_id, collapsed \
             FROM chat_messages WHERE session_id = ?1 ORDER BY ordering",
        )?;
        let rows = stmt
            .query_map(params![session_id], |row| {
                Ok(StoredMessage {
                    role: row.get(0)?,
                    text: row.get(1)?,
                    tool_args: row.get(2)?,
                    tool_result: row.get(3)?,
                    tool_internal_id: row.get(4)?,
                    collapsed: row.get::<_, i32>(5)? != 0,
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
        messages: &[StoredMessage],
    ) -> Result<()> {
        let conn = self.conn.lock();
        let now = Utc::now().to_rfc3339();

        conn.execute(
            "UPDATE chat_sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
            params![title, now, session_id],
        )?;

        // Replace all messages atomically.
        conn.execute(
            "DELETE FROM chat_messages WHERE session_id = ?1",
            params![session_id],
        )?;
        let mut insert = conn.prepare(
            "INSERT INTO chat_messages \
             (session_id, ordering, role, text, tool_args, tool_result, tool_internal_id, collapsed) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for (i, msg) in messages.iter().enumerate() {
            insert.execute(params![
                session_id,
                i as i64,
                msg.role,
                msg.text,
                msg.tool_args,
                msg.tool_result,
                msg.tool_internal_id,
                if msg.collapsed { 1 } else { 0 },
            ])?;
        }
        Ok(())
    }

    /// Delete a session and all its messages.
    #[allow(dead_code)]
    pub fn delete_session(&self, session_id: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "DELETE FROM chat_sessions WHERE id = ?1",
            params![session_id],
        )?;
        Ok(())
    }
}
