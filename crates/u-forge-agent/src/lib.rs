//! Rig-based agent tools for the u-forge knowledge graph.
//!
//! The crate exposes three search tools and two mutation tools, plus
//! [`GraphAgent`] for running them through Lemonade's OpenAI-compatible API.
//! Tool metadata and argument validation are defined once in the internal tool
//! catalog so prompt-visible definitions, budget accounting, and Rig
//! registration cannot drift independently.

mod agent;
mod budget;
mod stream;
mod tools;

pub use agent::{AgentParams, GraphAgent, HistoryMessage, select_history_window};
pub use budget::{
    BoundedSchemaSummary, SchemaPriorityContext, TokenEstimate, bounded_schema_summary,
    count_tokens, estimate_tokens,
};
pub use stream::AgentStreamEvent;
pub use tools::{
    FtsSearchArgs, FtsSearchTool, HybridSearchArgs, HybridSearchTool, SemanticSearchArgs,
    SemanticSearchTool, ToolError, UpsertEdgeArgs, UpsertEdgeTool, UpsertNodeArgs, UpsertNodeTool,
};

pub use rig;

#[cfg(test)]
mod tests;
