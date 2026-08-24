use std::sync::Arc;

use rig::client::AgentClientExt;

use rig::providers::openai::CompletionsClient;
use u_forge_core::queue::{CancellationToken, InferenceQueue};
use u_forge_core::{EffectiveAgentBudget, KnowledgeGraph};

use crate::budget::{self, TokenEstimate};
use crate::tools::{
    FtsSearchTool, HybridSearchTool, SemanticSearchTool, UpsertEdgeTool, UpsertNodeTool, validation,
};

/// A single prior conversation turn for context injection.
///
/// `role` is `"user"` or `"assistant"`.
#[derive(Debug, Clone)]
pub struct HistoryMessage {
    pub role: String,
    pub content: String,
}

/// Return the subset of `history` that fits inside the available token budget.
///
/// The active model context is the only ceiling. If older messages do not fit,
/// they are replaced by an explicit notice that the visible messages are the
/// most recent portion of a longer conversation.
///
/// Messages are evaluated newest-first; the returned `Vec` is in chronological
/// order (oldest first), ready to pass directly to `history()`.
pub fn select_history_window(
    history: &[HistoryMessage],
    system_prompt: &str,
    current_msg: &str,
    max_context_tokens: usize,
) -> Vec<HistoryMessage> {
    budget::select_history_window(history, system_prompt, current_msg, 0, max_context_tokens)
}

/// Sampling and generation parameters for the agent, built from
/// [`ChatDeviceConfig`] + [`ChatConfig`].
#[derive(Debug, Clone)]
pub struct AgentParams {
    /// Sampling temperature. `None` defers to the server/model default.
    pub temperature: Option<f64>,
    /// Maximum generation tokens.
    pub max_tokens: Option<u64>,
    /// Nucleus sampling threshold (0.0–1.0).
    pub top_p: Option<f64>,
    /// Top-k sampling (llama.cpp).
    pub top_k: Option<u32>,
    /// Min-p sampling (llama.cpp).
    pub min_p: Option<f64>,
    /// Penalise repeated tokens by frequency (-2.0–2.0).
    pub frequency_penalty: Option<f64>,
    /// Penalise tokens that appeared at all (-2.0–2.0).
    pub presence_penalty: Option<f64>,
    /// Repetition penalty (llama.cpp, typically 1.0–1.5).
    pub repetition_penalty: Option<f64>,
    /// RNG seed for reproducible generation.
    pub seed: Option<u64>,
    /// Stop sequences.
    pub stop: Option<Vec<String>>,
    /// Maximum tool-call round-trips per user message.
    pub max_tool_turns: usize,
    /// Active-model-safe context, cumulative, output, and repeat limits.
    pub budget: EffectiveAgentBudget,
}

impl Default for AgentParams {
    fn default() -> Self {
        Self {
            temperature: None,
            max_tokens: None,
            top_p: None,
            top_k: None,
            min_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            repetition_penalty: None,
            seed: None,
            stop: None,
            max_tool_turns: 5,
            budget: EffectiveAgentBudget::default(),
        }
    }
}

/// A configured agent backed by the three graph search tools.
///
/// Wraps a rig `CompletionsClient` pointed at Lemonade's OpenAI-compatible
/// endpoint. Each cancellable streaming operation builds a fresh Rig agent and
/// runs the multi-turn search and mutation tool loop.
///
/// `Clone` is cheap — the inner client and Arc handles are reference-counted.
#[derive(Clone)]
pub struct GraphAgent {
    client: CompletionsClient,
    graph: Arc<KnowledgeGraph>,
    queue: Arc<InferenceQueue>,
    hq_queue: Option<Arc<InferenceQueue>>,
    base_prompt: String,
    tool_guidance: String,
    schema: Option<u_forge_core::SchemaDefinition>,
    tool_definition_tokens: TokenEstimate,
    pub(crate) gpu: Option<Arc<u_forge_core::GpuResourceManager>>,
}

impl GraphAgent {
    /// Build a `GraphAgent` connected to the given Lemonade base URL,
    /// e.g. `http://localhost:13305/api/v1`.
    pub fn new(
        lemonade_url: &str,
        graph: Arc<KnowledgeGraph>,
        queue: Arc<InferenceQueue>,
        hq_queue: Option<Arc<InferenceQueue>>,
        system_prompt: impl Into<String>,
    ) -> anyhow::Result<Self> {
        let connection = Arc::new(u_forge_core::lemonade::LemonadeConnection::external(
            lemonade_url,
        )?);
        Self::new_with_connection(connection, graph, queue, hq_queue, system_prompt)
    }

    pub fn new_with_connection(
        connection: Arc<u_forge_core::lemonade::LemonadeConnection>,
        graph: Arc<KnowledgeGraph>,
        queue: Arc<InferenceQueue>,
        hq_queue: Option<Arc<InferenceQueue>>,
        system_prompt: impl Into<String>,
    ) -> anyhow::Result<Self> {
        Self::new_with_connection_and_gpu(connection, graph, queue, hq_queue, system_prompt, None)
    }

    pub fn new_with_connection_and_gpu(
        connection: Arc<u_forge_core::lemonade::LemonadeConnection>,
        graph: Arc<KnowledgeGraph>,
        queue: Arc<InferenceQueue>,
        hq_queue: Option<Arc<InferenceQueue>>,
        system_prompt: impl Into<String>,
        gpu: Option<Arc<u_forge_core::GpuResourceManager>>,
    ) -> anyhow::Result<Self> {
        let client = CompletionsClient::builder()
            .api_key(connection.api_credential().unwrap_or("lemonade"))
            .base_url(connection.api_base())
            .http_client(connection.completion_http_client())
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build rig client: {e}"))?;
        let base_prompt: String = system_prompt.into();
        let schema = graph.merged_schema_definition()?;

        let tool_guidance = "\
## Tool-use guidelines

1. **Search before writing.** Before creating a node, search to check it doesn't already exist.
2. **One call per node.** Include name, object_type, and all known properties in a single \
   upsert_node call. Never create a blank node and fill properties afterwards.
3. **Refer to the schema below** for valid object_type values and their properties. \
   Use the property names and types exactly as listed.
4. **Stop when done.** After a successful tool call, report the result to the user. \
   Do not re-call a tool for the same node unless asked."
            .to_string();

        let tool_definitions = validation::serialized_tool_definitions()?;

        Ok(Self {
            client,
            graph,
            queue,
            hq_queue,
            base_prompt,
            tool_guidance,
            schema,
            tool_definition_tokens: budget::estimate_tool_definitions(&tool_definitions),
            gpu,
        })
    }

    /// Compute Rig's flattened `additional_params` JSON from sampling knobs.
    ///
    /// Rig's OpenAI provider flattens this into the request body, so keys like
    /// `frequency_penalty`, `top_p`, `seed`, etc. end up as top-level fields
    /// in the OpenAI-compatible `/chat/completions` request.
    pub(crate) fn build_additional_params(params: &AgentParams) -> Option<serde_json::Value> {
        let mut map = serde_json::Map::new();
        if let Some(v) = params.top_p {
            map.insert("top_p".into(), serde_json::json!(v));
        }
        if let Some(v) = params.top_k {
            map.insert("top_k".into(), serde_json::json!(v));
        }
        if let Some(v) = params.min_p {
            map.insert("min_p".into(), serde_json::json!(v));
        }
        if let Some(v) = params.frequency_penalty {
            map.insert("frequency_penalty".into(), serde_json::json!(v));
        }
        if let Some(v) = params.presence_penalty {
            map.insert("presence_penalty".into(), serde_json::json!(v));
        }
        if let Some(v) = params.repetition_penalty {
            map.insert("repeat_penalty".into(), serde_json::json!(v));
        }
        if let Some(v) = params.seed {
            map.insert("seed".into(), serde_json::json!(v));
        }
        if let Some(ref v) = params.stop {
            map.insert("stop".into(), serde_json::json!(v));
        }
        if map.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(map))
        }
    }

    pub(crate) fn build_request_additional_params(
        params: &AgentParams,
        reasoning: u_forge_core::ReasoningPolicy,
    ) -> serde_json::Value {
        let mut value = Self::build_additional_params(params)
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
        if let (serde_json::Value::Object(values), Some(enabled)) =
            (&mut value, reasoning.request_hint())
        {
            values.insert("enable_thinking".into(), serde_json::json!(enabled));
        }
        value
    }

    pub(crate) fn prepare_budget(
        &self,
        user_message: &str,
        history: &[HistoryMessage],
        params: &AgentParams,
    ) -> (budget::BudgetController, Vec<HistoryMessage>) {
        let history_text = history
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let controller = budget::BudgetController::new(
            params.budget.clone(),
            self.schema.clone(),
            self.base_prompt.clone(),
            self.tool_guidance.clone(),
            user_message.to_string(),
            history_text,
            self.tool_definition_tokens,
        );
        (controller, history.to_vec())
    }

    pub(crate) fn build_agent_with_params(
        &self,
        model_id: &str,
        reasoning: u_forge_core::ReasoningPolicy,
        params: &AgentParams,
        cancellation: CancellationToken,
        budget: budget::BudgetController,
    ) -> rig::agent::Agent<rig::providers::openai::CompletionModel> {
        let initial_preamble = budget.initial_preamble();
        let mut builder = self
            .client
            .agent(model_id)
            .preamble(&initial_preamble)
            .add_hook(budget);
        if let Some(temp) = params.temperature {
            builder = builder.temperature(temp);
        }

        if let Some(max_tokens) = params.max_tokens {
            builder = builder.max_tokens(max_tokens);
        }
        let additional = Self::build_request_additional_params(params, reasoning);
        if !additional.as_object().is_none_or(serde_json::Map::is_empty) {
            builder = builder.additional_params(additional);
        }

        builder
            .tool(
                HybridSearchTool::new(self.graph.clone(), self.queue.clone())
                    .with_hq_queue(self.hq_queue.clone())
                    .with_cancellation(cancellation.clone()),
            )
            .tool(FtsSearchTool::new(self.graph.clone()))
            .tool(
                SemanticSearchTool::new(self.graph.clone(), self.queue.clone())
                    .with_hq_queue(self.hq_queue.clone())
                    .with_cancellation(cancellation.clone()),
            )
            .tool(
                UpsertNodeTool::new(
                    self.graph.clone(),
                    self.queue.clone(),
                    self.hq_queue.clone(),
                )
                .with_cancellation(cancellation.clone()),
            )
            .tool(
                UpsertEdgeTool::new(
                    self.graph.clone(),
                    self.queue.clone(),
                    self.hq_queue.clone(),
                )
                .with_cancellation(cancellation),
            )
            .build()
    }
}
