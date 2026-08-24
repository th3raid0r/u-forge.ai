mod mutation;
mod search;
pub(crate) mod validation;

pub use mutation::{UpsertEdgeArgs, UpsertEdgeTool, UpsertNodeArgs, UpsertNodeTool};
pub use search::{
    FtsSearchArgs, FtsSearchTool, HybridSearchArgs, HybridSearchTool, SemanticSearchArgs,
    SemanticSearchTool,
};

#[cfg(test)]
pub(crate) use mutation::{preflight_edge_contract, preflight_node_properties, resolve_node};
#[cfg(test)]
pub(crate) use search::{first_matched_search_content, format_search_response};

/// Error returned by all agent tools (search and write).
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ToolError(String);

impl From<anyhow::Error> for ToolError {
    fn from(error: anyhow::Error) -> Self {
        Self(format!("{error:#}"))
    }
}
