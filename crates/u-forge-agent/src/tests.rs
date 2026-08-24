use super::tools::validation::{self, validate_tool_args};
use super::tools::{
    first_matched_search_content, format_search_response, preflight_edge_contract,
    preflight_node_properties, resolve_node,
};
use super::{AgentParams, GraphAgent};
use rig::tool::{Tool, ToolContext};
use serde_json::json;
use std::collections::HashSet;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tempfile::TempDir;
use u_forge_core::ai::embeddings::{EmbeddingModelInfo, EmbeddingProvider, EmbeddingProviderType};
use u_forge_core::lemonade::{BuiltProvider, Capability, ProviderSlot};
use u_forge_core::queue::{CancellationToken, InferenceQueue, InferenceQueueBuilder};
use u_forge_core::schema::ValidationRule;
use u_forge_core::search::{SearchStageOutcome, SearchStageOutcomes, SearchStageStatus};
use u_forge_core::{
    KnowledgeGraph, ObjectId, ObjectMetadata, ObjectTypeSchema, PropertySchema, SchemaDefinition,
};

#[tokio::test]
async fn tool_catalog_matches_validation_budget_and_rig_registration() {
    let expected_names = [
        "search_hybrid",
        "search_fts",
        "search_semantic",
        "upsert_node",
        "upsert_edge",
    ];
    let catalog = validation::catalog();
    let catalog_names = catalog.iter().map(|spec| spec.name).collect::<Vec<_>>();
    assert_eq!(catalog_names, expected_names);
    assert_eq!(
        catalog_names.iter().copied().collect::<HashSet<_>>().len(),
        catalog_names.len(),
        "tool names must be unique"
    );
    for spec in catalog {
        assert!(validation::find(spec.name).is_some());
    }
    assert!(validate_tool_args("missing_tool", &json!({})).is_err());

    let definitions = validation::tool_definitions();
    let serialized = validation::serialized_tool_definitions().unwrap();
    assert_eq!(serialized.len(), definitions.len());
    for ((spec, definition), serialized) in catalog.iter().zip(&definitions).zip(&serialized) {
        assert_eq!(definition.name, spec.name);
        assert_eq!(definition.description, spec.description);
        assert_eq!(definition.parameters, spec.parameters());
        let wrapper: serde_json::Value = serde_json::from_str(serialized).unwrap();
        assert_eq!(wrapper["type"], "function");
        assert_eq!(
            wrapper["function"],
            serde_json::to_value(definition).unwrap()
        );
    }
    assert!(super::budget::estimate_tool_definitions(&serialized).tokens > 0);

    let temp = TempDir::new().unwrap();
    let graph = Arc::new(KnowledgeGraph::new(temp.path()).unwrap());
    let queue = Arc::new(InferenceQueueBuilder::new().build());
    let params = AgentParams::default();
    let graph_agent =
        GraphAgent::new("http://127.0.0.1:13305/api/v1", graph, queue, None, "test").unwrap();
    let (budget, _) = graph_agent.prepare_budget("test", &[], &params);
    let rig_agent = graph_agent.build_agent_with_params(
        "test-model",
        u_forge_core::ReasoningPolicy::Default,
        &params,
        CancellationToken::new(),
        budget,
    );
    assert_eq!(rig_agent.tool_definitions(None).await.unwrap(), definitions);
}

struct CountingEmbeddingProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl EmbeddingProvider for CountingEmbeddingProvider {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let seed = text.len() as f32;
        Ok((0..768)
            .map(|index| ((seed + index as f32) % 1000.0) / 1000.0)
            .collect())
    }

    async fn embed_batch(&self, texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
        let mut embeddings = Vec::with_capacity(texts.len());
        for text in texts {
            embeddings.push(self.embed(&text).await?);
        }
        Ok(embeddings)
    }

    fn dimensions(&self) -> anyhow::Result<usize> {
        Ok(768)
    }

    fn max_tokens(&self) -> anyhow::Result<usize> {
        Ok(512)
    }

    fn provider_type(&self) -> EmbeddingProviderType {
        EmbeddingProviderType::Lemonade
    }

    fn model_info(&self) -> Option<EmbeddingModelInfo> {
        Some(EmbeddingModelInfo {
            name: "agent-edge-test".to_string(),
            dimensions: 768,
            description: None,
        })
    }
}

fn counting_embedding_queue() -> (Arc<InferenceQueue>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = CountingEmbeddingProvider {
        calls: calls.clone(),
    };
    let queue = InferenceQueueBuilder::new()
        .with_provider(BuiltProvider {
            name: "agent-edge-test".to_string(),
            capability: Capability::Embedding,
            provider: ProviderSlot::Embedding(Arc::new(provider)),
            weight: 100,
        })
        .build();
    (Arc::new(queue), calls)
}

fn graph_with_edge_contract() -> (Arc<KnowledgeGraph>, TempDir, ObjectId, ObjectId, ObjectId) {
    let temp = TempDir::new().unwrap();
    let graph = Arc::new(KnowledgeGraph::new(temp.path()).unwrap());
    let mut schema = SchemaDefinition::new(
        "imported_schemas".to_string(),
        "1.0.0".to_string(),
        "test".to_string(),
    );
    schema.add_object_type(
        "character".to_string(),
        ObjectTypeSchema::new("character".to_string(), "Character".to_string()),
    );
    schema.add_object_type(
        "location".to_string(),
        ObjectTypeSchema::new("location".to_string(), "Location".to_string()),
    );
    schema.add_edge_type(
        "protects".to_string(),
        u_forge_core::EdgeTypeSchema::new("protects".to_string(), "Guards a place".to_string())
            .with_source_types(vec!["character".to_string()])
            .with_target_types(vec!["location".to_string()]),
    );
    schema.add_edge_type(
        "knows".to_string(),
        u_forge_core::EdgeTypeSchema::new(
            "knows".to_string(),
            "Knows another character".to_string(),
        )
        .with_source_types(vec!["character".to_string()])
        .with_target_types(vec!["character".to_string()]),
    );
    graph.get_schema_manager().save_schema(&schema).unwrap();

    let guardian = graph
        .add_object(ObjectMetadata::new(
            "character".to_string(),
            "Guardian".to_string(),
        ))
        .unwrap();
    let sanctum = graph
        .add_object(ObjectMetadata::new(
            "location".to_string(),
            "Sanctum".to_string(),
        ))
        .unwrap();
    let intruder = graph
        .add_object(ObjectMetadata::new(
            "character".to_string(),
            "Intruder".to_string(),
        ))
        .unwrap();
    (graph, temp, guardian, sanctum, intruder)
}

fn stage(status: SearchStageStatus, diagnostic: Option<&str>) -> SearchStageOutcome {
    SearchStageOutcome {
        status,
        diagnostic: diagnostic.map(str::to_string),
    }
}

#[test]
fn semantic_search_failure_explains_unavailable_lanes_and_recovery() {
    let outcomes = SearchStageOutcomes {
        fts: stage(SearchStageStatus::IntentionallySkipped, None),
        standard_semantic: stage(
            SearchStageStatus::Failed,
            Some("embedding provider rejected the request"),
        ),
        high_quality_semantic: stage(
            SearchStageStatus::Unavailable,
            Some("HQ embedding queue is not configured"),
        ),
        reranking: stage(SearchStageStatus::IntentionallySkipped, None),
    };

    let message = format_search_response("Semantic", "Z-Rho", Vec::new(), &outcomes)
        .expect("expected search unavailability is a normal tool response");
    assert!(message.contains("Semantic search is unavailable"));
    assert!(message.contains("embedding provider rejected the request"));
    assert!(message.contains("HQ embedding queue is not configured"));
    assert!(message.contains("keyword search"));
    assert!(message.contains("rebuild the semantic index from Settings"));
}

#[test]
fn search_content_is_not_discarded_at_128_tokens() {
    let content = "Z-Rho lore detail ".repeat(180);
    assert!(super::count_tokens(&content) > 128);
    assert_eq!(
        first_matched_search_content(&[content.as_str()]),
        Some(content)
    );
}

#[test]
fn agent_sampling_uses_current_lemonade_wire_names() {
    let params = AgentParams {
        top_p: Some(0.8),
        top_k: Some(40),
        min_p: Some(0.05),
        frequency_penalty: Some(0.1),
        presence_penalty: Some(0.2),
        repetition_penalty: Some(1.1),
        seed: Some(7),
        stop: Some(vec!["END".into()]),
        ..AgentParams::default()
    };
    let value = GraphAgent::build_additional_params(&params).unwrap();
    assert_eq!(value["repeat_penalty"], 1.1);
    assert!(value.get("repetition_penalty").is_none());
    assert_eq!(value["top_p"], 0.8);
    assert_eq!(value["top_k"], 40);
    assert_eq!(value["stop"], json!(["END"]));
}

#[test]
fn agent_reasoning_policy_omits_default_and_sends_explicit_states() {
    let params = AgentParams::default();
    let default = GraphAgent::build_request_additional_params(
        &params,
        u_forge_core::ReasoningPolicy::Default,
    );
    assert!(default.get("enable_thinking").is_none());
    let enabled = GraphAgent::build_request_additional_params(
        &params,
        u_forge_core::ReasoningPolicy::Enabled,
    );
    assert_eq!(enabled["enable_thinking"], true);
    let disabled = GraphAgent::build_request_additional_params(
        &params,
        u_forge_core::ReasoningPolicy::Disabled,
    );
    assert_eq!(disabled["enable_thinking"], false);
}

// FtsSearchTool validation

#[test]
fn fts_rejects_type_mismatch() {
    let raw = json!({"query": "test", "limit": "ten"});
    let err =
        validate_tool_args("search_fts", &raw).expect_err("should reject string for numeric field");
    let msg = err.to_string();
    assert!(
        msg.contains("limit") || msg.contains("/limit"),
        "error should name the offending field: {msg}"
    );
}

#[test]
fn fts_rejects_missing_required() {
    let raw = json!({"limit": 5});
    let err = validate_tool_args("search_fts", &raw)
        .expect_err("should reject missing required 'query' field");
    let msg = err.to_string();
    assert!(
        msg.contains("query"),
        "error should name missing field: {msg}"
    );
}

#[test]
fn fts_rejects_unknown_field() {
    let raw = json!({"query": "test", "qury": "typo"});
    let err = validate_tool_args("search_fts", &raw)
        .expect_err("should reject unknown field with additionalProperties: false");
    let msg = err.to_string();
    assert!(
        msg.contains("qury") || msg.to_lowercase().contains("additional"),
        "error should signal unknown field: {msg}"
    );
}

#[test]
fn fts_accepts_valid_args() {
    validate_tool_args("search_fts", &json!({"query": "Gandalf", "limit": 5}))
        .expect("valid args should pass");
    validate_tool_args("search_fts", &json!({"query": "Aragorn"}))
        .expect("optional limit omitted should pass");
}

// UpsertNodeTool validation (write path with the most complex schema)

#[test]
fn upsert_node_rejects_missing_required() {
    // both `name` and `object_type` are required
    let raw = json!({"object_type": "character"});
    let err =
        validate_tool_args("upsert_node", &raw).expect_err("should reject missing required 'name'");
    let msg = err.to_string();
    assert!(
        msg.contains("name"),
        "error should name missing field: {msg}"
    );
}

#[test]
fn upsert_node_rejects_unknown_field() {
    let raw = json!({"name": "Gandalf", "object_type": "character", "typo_field": "oops"});
    let err = validate_tool_args("upsert_node", &raw).expect_err("should reject unknown field");
    let msg = err.to_string();
    assert!(
        msg.contains("typo_field") || msg.to_lowercase().contains("additional"),
        "error should signal unknown field: {msg}"
    );
}

#[test]
fn upsert_node_accepts_valid_args() {
    validate_tool_args(
        "upsert_node",
        &json!({"name": "Gandalf", "object_type": "character"}),
    )
    .expect("minimal valid args should pass");

    validate_tool_args(
        "upsert_node",
        &json!({
            "name": "Gandalf",
            "object_type": "character",
            "node_id": "00000000-0000-0000-0000-000000000001",
            "properties": {"description": "A wizard"}
        }),
    )
    .expect("full valid args should pass");
}

#[test]
fn upsert_node_preflight_reports_rules_and_applies_coercion() {
    let temp = TempDir::new().unwrap();
    let graph = KnowledgeGraph::new(temp.path()).unwrap();
    let mut schema = SchemaDefinition::new(
        "imported_schemas".to_string(),
        "1.0.0".to_string(),
        "test".to_string(),
    );
    schema.add_object_type(
        "spell".to_string(),
        ObjectTypeSchema::new("spell".to_string(), "Spell".to_string())
            .with_property(
                "level".to_string(),
                PropertySchema::number("level")
                    .with_validation(ValidationRule::new().with_value_range(Some(1.0), Some(5.0))),
            )
            .with_required_property("level".to_string()),
    );
    graph.get_schema_manager().save_schema(&schema).unwrap();

    let mut invalid = ObjectMetadata::new("spell".to_string(), "Impossible".to_string())
        .with_property("level".to_string(), "9".to_string());
    let error = preflight_node_properties(&graph, &mut invalid)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("property 'level': maximum value is 5"),
        "{error}"
    );

    let mut valid = ObjectMetadata::new("spell".to_string(), "Shield".to_string())
        .with_property("level".to_string(), "3".to_string());
    preflight_node_properties(&graph, &mut valid).unwrap();
    assert_eq!(valid.get_json_property("level"), Some(&json!(3.0)));
}

#[test]
fn upsert_edge_preflight_reports_sorted_schema_choices() {
    let (graph, _temp, guardian, sanctum, _intruder) = graph_with_edge_contract();

    let error = preflight_edge_contract(&graph, guardian, sanctum, "invented")
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("Valid edge types: knows, protects"),
        "{error}"
    );
    assert!(graph.get_relationships(guardian).unwrap().is_empty());
}

#[test]
fn upsert_edge_preflight_reports_invalid_endpoint_pair() {
    let (graph, _temp, guardian, _sanctum, intruder) = graph_with_edge_contract();

    let error = preflight_edge_contract(&graph, guardian, intruder, "protects")
        .unwrap_err()
        .to_string();

    assert!(error.contains("character -> character"), "{error}");
    assert!(
        error.contains("does not allow target type 'character'"),
        "{error}"
    );
    assert!(graph.get_relationships(guardian).unwrap().is_empty());
}

#[tokio::test]
async fn upsert_edge_persists_and_reembeds_both_endpoints() {
    let (graph, _temp, guardian, sanctum, _intruder) = graph_with_edge_contract();
    let (queue, calls) = counting_embedding_queue();
    let tool = super::UpsertEdgeTool::new(graph.clone(), queue, None);
    let mut context = ToolContext::new();

    let output = tool
        .call(
            &mut context,
            json!({
                "source": guardian.to_string(),
                "target": sanctum.to_string(),
                "edge_type": "protects"
            }),
        )
        .await
        .unwrap();

    assert!(
        output.contains("Guardian -[protects]-> Sanctum"),
        "{output}"
    );
    assert_eq!(graph.get_relationships(guardian).unwrap().len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn upsert_self_edge_reembeds_endpoint_once() {
    let (graph, _temp, guardian, _sanctum, _intruder) = graph_with_edge_contract();
    let (queue, calls) = counting_embedding_queue();
    let tool = super::UpsertEdgeTool::new(graph.clone(), queue, None);
    let mut context = ToolContext::new();

    tool.call(
        &mut context,
        json!({
            "source": guardian.to_string(),
            "target": guardian.to_string(),
            "edge_type": "knows"
        }),
    )
    .await
    .unwrap();

    assert_eq!(graph.get_relationships(guardian).unwrap().len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn upsert_edge_cancellation_before_persistence_prevents_write() {
    let (graph, _temp, guardian, sanctum, _intruder) = graph_with_edge_contract();
    let queue = Arc::new(InferenceQueueBuilder::new().build());
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let tool =
        super::UpsertEdgeTool::new(graph.clone(), queue, None).with_cancellation(cancellation);
    let mut context = ToolContext::new();

    let error = tool
        .call(
            &mut context,
            json!({
                "source": guardian.to_string(),
                "target": sanctum.to_string(),
                "edge_type": "protects"
            }),
        )
        .await
        .unwrap_err()
        .to_string();

    assert!(error.to_lowercase().contains("cancel"), "{error}");
    assert!(graph.get_relationships(guardian).unwrap().is_empty());
}

#[test]
fn unknown_tool_name_returns_error() {
    let err = validate_tool_args("nonexistent_tool", &json!({}))
        .expect_err("unregistered tool name should return error");
    assert!(err.to_string().contains("nonexistent_tool"));
}

#[test]
fn ambiguous_node_diagnostic_groups_every_candidate_by_type() {
    let temp = TempDir::new().unwrap();
    let graph = KnowledgeGraph::new(temp.path()).unwrap();
    let candidates = (0..7)
        .map(|index| {
            ObjectMetadata::new(
                if index % 2 == 0 { "npc" } else { "location" }.to_string(),
                "Echo".to_string(),
            )
        })
        .collect::<Vec<_>>();
    for candidate in &candidates {
        graph.add_object(candidate.clone()).unwrap();
    }

    let error = resolve_node(&graph, "Echo").expect_err("name should be ambiguous");
    let message = error.to_string();
    assert!(message.contains("location (3)"));
    assert!(message.contains("npc (4)"));
    assert!(message.contains("complete UUID"));
    for candidate in candidates {
        assert!(
            message.contains(&candidate.id.to_string()),
            "diagnostic omitted {}: {message}",
            candidate.id
        );
    }
}
