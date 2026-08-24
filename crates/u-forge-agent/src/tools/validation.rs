use std::sync::{LazyLock, OnceLock};

use rig::completion::ToolDefinition;
use schemars::schema_for;
use serde::de::DeserializeOwned;

use super::{
    FtsSearchArgs, HybridSearchArgs, SemanticSearchArgs, ToolError, UpsertEdgeArgs, UpsertNodeArgs,
};

pub(crate) const FTS_NAME: &str = "search_fts";
pub(crate) const SEMANTIC_NAME: &str = "search_semantic";
pub(crate) const HYBRID_NAME: &str = "search_hybrid";
pub(crate) const UPSERT_NODE_NAME: &str = "upsert_node";
pub(crate) const UPSERT_EDGE_NAME: &str = "upsert_edge";

const FTS_DESCRIPTION: &str = "Full-text keyword search over the knowledge graph using SQLite FTS5. Fast and exact — good for specific names, terms, or known phrases. Returns nodes that contain matching text, with the matching snippets.";
const SEMANTIC_DESCRIPTION: &str = "Semantic (embedding-based) search over the knowledge graph. Finds conceptually related nodes even when exact keywords don't match. Use for exploratory queries, related concepts, or when FTS returns nothing.";
const HYBRID_DESCRIPTION: &str = "Hybrid search over the knowledge graph: combines FTS5 keyword matching with semantic embedding search using Reciprocal Rank Fusion, then optionally reranks results with a cross-encoder. Returns fully hydrated node results with metadata, relationships, and content. Recommended as the default search tool.";
const UPSERT_NODE_DESCRIPTION: &str = "Create or update a knowledge graph node. Always search first to avoid duplicates. Populate name, object_type, and all known properties in one call. On update (node_id set), only changed keys are needed — omitted keys are kept.";
const UPSERT_EDGE_DESCRIPTION: &str = "Create or update a relationship (edge) between two nodes in the knowledge graph. Nodes can be specified by exact name or UUID. Use an edge_type from the active schema and only a source/target object-type pair that edge permits. Both endpoint nodes are re-indexed after the edge is saved.";

type SchemaFn = fn() -> &'static serde_json::Value;
type ValidatorFn = fn() -> &'static jsonschema::Validator;

pub(crate) struct ToolSpec {
    pub(crate) name: &'static str,
    pub(crate) description: &'static str,
    pub(crate) mutates_graph: bool,
    schema: SchemaFn,
    validator: ValidatorFn,
}

impl ToolSpec {
    pub(crate) fn parameters(&self) -> serde_json::Value {
        (self.schema)().clone()
    }

    fn validator(&self) -> &'static jsonschema::Validator {
        (self.validator)()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.to_string(),
            description: self.description.to_string(),
            parameters: self.parameters(),
        }
    }
}

macro_rules! schema_accessors {
    ($schema_fn:ident, $validator_fn:ident, $args:ty) => {
        fn $schema_fn() -> &'static serde_json::Value {
            static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
            SCHEMA.get_or_init(|| {
                serde_json::to_value(schema_for!($args))
                    .expect(concat!(stringify!($args), " schema is valid JSON"))
            })
        }

        fn $validator_fn() -> &'static jsonschema::Validator {
            static VALIDATOR: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
                jsonschema::validator_for($schema_fn())
                    .expect(concat!(stringify!($args), " validator compiles"))
            });
            &VALIDATOR
        }
    };
}

schema_accessors!(fts_schema, fts_validator, FtsSearchArgs);
schema_accessors!(semantic_schema, semantic_validator, SemanticSearchArgs);
schema_accessors!(hybrid_schema, hybrid_validator, HybridSearchArgs);
schema_accessors!(upsert_node_schema, upsert_node_validator, UpsertNodeArgs);
schema_accessors!(upsert_edge_schema, upsert_edge_validator, UpsertEdgeArgs);

static TOOL_CATALOG: [ToolSpec; 5] = [
    ToolSpec {
        name: HYBRID_NAME,
        description: HYBRID_DESCRIPTION,
        mutates_graph: false,
        schema: hybrid_schema,
        validator: hybrid_validator,
    },
    ToolSpec {
        name: FTS_NAME,
        description: FTS_DESCRIPTION,
        mutates_graph: false,
        schema: fts_schema,
        validator: fts_validator,
    },
    ToolSpec {
        name: SEMANTIC_NAME,
        description: SEMANTIC_DESCRIPTION,
        mutates_graph: false,
        schema: semantic_schema,
        validator: semantic_validator,
    },
    ToolSpec {
        name: UPSERT_NODE_NAME,
        description: UPSERT_NODE_DESCRIPTION,
        mutates_graph: true,
        schema: upsert_node_schema,
        validator: upsert_node_validator,
    },
    ToolSpec {
        name: UPSERT_EDGE_NAME,
        description: UPSERT_EDGE_DESCRIPTION,
        mutates_graph: true,
        schema: upsert_edge_schema,
        validator: upsert_edge_validator,
    },
];

pub(crate) fn catalog() -> &'static [ToolSpec] {
    &TOOL_CATALOG
}

pub(crate) fn find(tool_name: &str) -> Option<&'static ToolSpec> {
    catalog().iter().find(|spec| spec.name == tool_name)
}

pub(crate) fn tool_definitions() -> Vec<ToolDefinition> {
    catalog().iter().map(ToolSpec::definition).collect()
}

pub(crate) fn serialized_tool_definitions() -> Result<Vec<String>, serde_json::Error> {
    tool_definitions()
        .into_iter()
        .map(|definition| {
            serde_json::to_string(&serde_json::json!({
                "type": "function",
                "function": definition,
            }))
        })
        .collect()
}

/// Validate `raw` JSON args against the tool's declared JSON Schema.
///
/// On failure, the diagnostic names up to three offending field paths so the
/// model can correct its next call without receiving an unbounded error.
pub(crate) fn validate_tool_args(
    tool_name: &'static str,
    raw: &serde_json::Value,
) -> Result<(), ToolError> {
    let spec = find(tool_name)
        .ok_or_else(|| ToolError(format!("no validator registered for tool '{tool_name}'")))?;
    let validator = spec.validator();
    if validator.is_valid(raw) {
        return Ok(());
    }

    let formatted = validator
        .iter_errors(raw)
        .take(3)
        .map(|error| format!("{} — {}", error.instance_path(), error))
        .collect::<Vec<_>>()
        .join("; ");
    tracing::warn!(tool = tool_name, errors = %formatted, "tool arg validation failed");
    Err(ToolError(format!(
        "Tool args invalid for {tool_name}: {formatted}"
    )))
}

pub(crate) fn decode<T: DeserializeOwned>(
    tool_name: &'static str,
    raw: serde_json::Value,
) -> Result<T, ToolError> {
    validate_tool_args(tool_name, &raw)?;
    serde_json::from_value(raw).map_err(|error| {
        ToolError(format!(
            "deserialization failed after validation (bug): {error}"
        ))
    })
}

pub(crate) fn is_mutation(tool_name: &str) -> bool {
    find(tool_name).is_some_and(|spec| spec.mutates_graph)
}
