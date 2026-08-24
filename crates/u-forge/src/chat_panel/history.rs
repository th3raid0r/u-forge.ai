//! Chat session persistence and active-history list transitions.

use super::*;

impl ChatPanel {
    // ── Chat history methods ────────────────────────────────────────────────

    /// Title for the current session (for the header dropdown button).
    pub(super) fn session_title(&self) -> String {
        if let Some(sid) = &self.current_session_id {
            self.session_list
                .iter()
                .find(|s| s.id == *sid)
                .map(|s| s.title.clone())
                .unwrap_or_else(|| "Assistant".to_string())
        } else {
            "New Chat".to_string()
        }
    }

    /// Save the current messages to the active session (creating one if needed).
    pub(super) fn save_current_session(&mut self, cx: &mut Context<Self>) {
        let store = match &self.history_store {
            Some(s) => s.clone(),
            None => return,
        };

        // Derive a title from the first user message.
        let title = self
            .messages
            .iter()
            .find_map(|h| {
                let m = h.read(cx);
                if matches!(m.role, ChatMessageRole::User) {
                    Some(m.text().to_string())
                } else {
                    None
                }
            })
            .map(|t| {
                let t = t.trim();
                if t.len() > 60 {
                    format!("{}…", &t[..60])
                } else {
                    t.to_string()
                }
            })
            .unwrap_or_else(|| "New Chat".to_string());

        // Ensure we have a session ID.
        let session_id = match &self.current_session_id {
            Some(id) => id.clone(),
            None => match store.create_session(&title) {
                Ok(id) => {
                    self.current_session_id = Some(id.clone());
                    id
                }
                Err(e) => {
                    eprintln!("Failed to create chat session: {e}");
                    return;
                }
            },
        };

        let stored: Vec<StoredChatMessage> = self
            .messages
            .iter()
            .map(|h| h.read(cx).to_stored())
            .collect();
        if let Err(e) = store.save_session(&session_id, &title, &stored) {
            eprintln!("Failed to save chat session: {e}");
        }

        // Refresh the cached session list.
        self.session_list = store.list_sessions().unwrap_or_default();
        self.history_list_state.reset(self.session_list.len());
    }

    /// Start a new empty chat session.
    ///
    /// No-op while streaming: swapping `self.messages` mid-stream would cause
    /// in-flight `TextDelta` / `ReasoningDelta` events to append to the new
    /// session and terminal run cleanup to save the polluted list under the
    /// wrong session_id. The Send button is already gated on `streaming`.
    pub(super) fn new_session(&mut self, cx: &mut Context<Self>) {
        if self.controls_locked() {
            tracing::debug!("new_session suppressed: Assistant is busy");
            return;
        }
        // Save current session before switching.
        if !self.messages.is_empty() {
            self.save_current_session(cx);
        }

        self.messages.clear();
        self.current_session_id = None;
        self.history_dropdown_open = false;
        self.toolbar_menu_open = false;
        self.reset_list_state();
        cx.notify();
    }

    /// Switch to an existing session by ID.
    ///
    /// No-op while streaming — see `new_session` for the race it prevents.
    pub(super) fn load_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        if self.controls_locked() {
            tracing::debug!(%session_id, "load_session suppressed: Assistant is busy");
            return;
        }
        // Save current session before switching.
        if !self.messages.is_empty() {
            self.save_current_session(cx);
        }

        let store = match &self.history_store {
            Some(s) => s.clone(),
            None => return,
        };

        match store.load_messages(session_id) {
            Ok(msgs) => {
                self.messages = msgs
                    .into_iter()
                    .map(|m| cx.new(|cx| ChatMessageView::from_stored(m, cx)))
                    .collect();
                self.current_session_id = Some(session_id.to_string());
                self.history_dropdown_open = false;
                self.reset_list_state();
                cx.notify();
            }
            Err(e) => {
                eprintln!("Failed to load chat session: {e}");
            }
        }
    }

    /// Delete the message at `ix` from the current session.
    ///
    /// No-op while streaming — the last message during streaming is the
    /// in-flight assistant response; allowing delete mid-stream would race
    /// with `append_text`. The button is also not rendered when streaming.
    pub(super) fn delete_message_at(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.streaming {
            tracing::debug!(ix, "delete_message_at suppressed: stream in progress");
            return;
        }
        if ix >= self.messages.len() {
            return;
        }
        self.messages.remove(ix);
        self.list_state.reset(self.messages.len());
        self.save_current_session(cx);
        cx.notify();
    }

    /// Delete a session from history. If it's the current session, clear the chat.
    ///
    /// Deleting the **active** session while streaming is suppressed (it would
    /// clear `self.messages` out from under the in-flight stream). Deleting a
    /// different session is always safe.
    pub(super) fn delete_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        if self.streaming && self.current_session_id.as_deref() == Some(session_id) {
            tracing::debug!(%session_id, "delete_session suppressed: active session is streaming");
            return;
        }
        let store = match &self.history_store {
            Some(s) => s.clone(),
            None => return,
        };

        if let Err(e) = store.delete_session(session_id) {
            eprintln!("Failed to delete chat session: {e}");
            return;
        }

        // If we just deleted the active session, clear the chat.
        if self.current_session_id.as_deref() == Some(session_id) {
            self.messages.clear();
            self.current_session_id = None;
            self.reset_list_state();
        }

        // Refresh the cached session list.
        self.session_list = store.list_sessions().unwrap_or_default();
        self.history_list_state.reset(self.session_list.len());
        cx.notify();
    }
}
