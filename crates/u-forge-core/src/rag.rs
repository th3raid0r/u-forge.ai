//! RAG (Retrieval-Augmented Generation) utilities.
//!
//! Shared helpers for assembling LLM-ready context from knowledge graph search
//! results.  Both the [`cli_chat`](../../examples/cli_chat.rs) demo and the
//! future TypeScript agentic sandbox (`u-forge-ts-runtime`) consume this module
//! — the Rust implementation serves as the reference design for the `.d.ts`
//! contract.
//!
//! # Pattern
//!
//! ```text
//! search_hybrid(graph, queue, ...)
//!     → Vec<NodeSearchResult>
//!     → format_search_context(&results)   // renders nodes into a text block
//!     → build_rag_messages(system, ctx, history, max_turns, query)
//!     → InferenceQueue::generate(ChatRequest::new(messages))
//! ```

use crate::lemonade::ChatMessage;
use crate::search::NodeSearchResult;

// ── Public types ──────────────────────────────────────────────────────────────

/// Formatted context block ready to be injected into an LLM system prompt.
///
/// Produced by [`format_search_context`] from a slice of [`NodeSearchResult`]s.
pub struct RagContext {
    /// The rendered text block, suitable for embedding in a system message.
    ///
    /// Contains each node's name, type, chunk text, and connected node names,
    /// separated by clear delimiters.
    pub formatted_context: String,

    /// Number of source nodes that contributed to the context.
    pub source_count: usize,
}

// ── Public functions ──────────────────────────────────────────────────────────

/// Render a slice of [`NodeSearchResult`] items into an LLM-ready context block.
///
/// Each node is formatted as:
///
/// ```text
/// ### <name> (<type>)
/// <chunk text 1>
/// <chunk text 2>
/// Connected: <node a>, <node b>, …
/// ```
///
/// The resulting [`RagContext`] is intended for injection into a system message
/// via [`build_rag_messages`].  When `results` is empty, `formatted_context`
/// is an empty string and `source_count` is 0.
pub fn format_search_context(results: &[NodeSearchResult]) -> RagContext {
    if results.is_empty() {
        return RagContext {
            formatted_context: String::new(),
            source_count: 0,
        };
    }

    let mut parts = Vec::with_capacity(results.len());

    for result in results {
        let node = &result.node;
        let mut node_text = format!("### {} ({})\n", node.name, node.object_type);

        for chunk in &result.chunks {
            node_text.push_str(&chunk.content);
            node_text.push('\n');
        }

        // Collect names of directly connected nodes (de-duplicated, sorted for
        // deterministic output).
        if !result.connected_node_names.is_empty() {
            let mut connected: Vec<&str> = result
                .connected_node_names
                .values()
                .map(|c| c.name.as_str())
                .collect();
            connected.sort_unstable();
            connected.dedup();
            node_text.push_str("Connected: ");
            node_text.push_str(&connected.join(", "));
            node_text.push('\n');
        }

        parts.push(node_text);
    }

    RagContext {
        formatted_context: parts.join("\n"),
        source_count: results.len(),
    }
}

/// Assemble the full [`ChatMessage`] array for a single RAG conversation turn.
///
/// Message order:
/// 1. **System** — `system_base` appended with the retrieved context block
///    (omitted when `ctx.formatted_context` is empty).
/// 2. **History** — the last `min(history.len(), max_history_turns * 2)` messages
///    from `history`, preserving alternating user/assistant pairs.
/// 3. **User** — the current `user_query`.
///
/// `max_history_turns` counts *turn pairs* (one user + one assistant message
/// each), so `max_history_turns = 10` retains up to 20 history messages.
pub fn build_rag_messages(
    system_base: &str,
    ctx: &RagContext,
    history: &[ChatMessage],
    max_history_turns: usize,
    user_query: &str,
) -> Vec<ChatMessage> {
    let mut messages = Vec::new();

    // System message: base instructions + optional context block.
    let system_content = if ctx.formatted_context.is_empty() {
        system_base.to_string()
    } else {
        format!(
            "{}\n\n## Relevant Knowledge Graph Context\n\n{}",
            system_base, ctx.formatted_context
        )
    };
    messages.push(ChatMessage::system(system_content));

    // History: keep the last max_history_turns turn-pairs (user + assistant).
    // Each turn pair is 2 messages, so we take the tail of `history` up to
    // `max_history_turns * 2` entries.
    let max_history_msgs = max_history_turns.saturating_mul(2);
    let history_slice = if history.len() > max_history_msgs {
        &history[history.len() - max_history_msgs..]
    } else {
        history
    };
    messages.extend_from_slice(history_slice);

    // Current user turn.
    messages.push(ChatMessage::user(user_query));

    messages
}
