//! Per-request token accounting, bounded schema selection, and Rig hook guards.

use std::collections::VecDeque;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, LazyLock, Mutex};

use rig::agent::{
    AgentHook, CompletionCallAction, CompletionCallEvent, HookContext, ObservationAction,
    RequestPatch, StepEventKind, StreamResponseFinish, ToolCall, ToolCallAction, ToolResultAction,
    ToolResultEvent,
};
use rig::completion::message::{Message, UserContent};
use tiktoken_rs::CoreBPE;
use u_forge_core::lemonade::AgentBudgetDiagnostics;
use u_forge_core::{EffectiveAgentBudget, SchemaDefinition};

use crate::{HistoryMessage, tool_validation};

const TOKENS_PER_MESSAGE: usize = 4;
const REPLY_PRIMING_TOKENS: usize = 3;
const REQUEST_ENVELOPE_TOKENS: usize = 32;
const MIN_TOOL_RESULT_TOKENS: usize = 16;
const MAX_RECENT_TOOL_RESULTS: usize = 4;

static O200K_BPE: LazyLock<Result<CoreBPE, String>> = LazyLock::new(|| {
    tiktoken_rs::o200k_harmony().map_err(|error| format!("o200k tokenizer unavailable: {error}"))
});

/// A token count plus whether the conservative byte-count fallback was used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenEstimate {
    pub tokens: usize,
    pub used_fallback: bool,
}

/// Estimate text with the single cached tokenizer policy used by the agent.
pub fn estimate_tokens(text: &str) -> TokenEstimate {
    match &*O200K_BPE {
        Ok(tokenizer) => TokenEstimate {
            tokens: tokenizer.encode_with_special_tokens(text).len(),
            used_fallback: false,
        },
        Err(error) => {
            tracing::warn!(%error, "agent token estimation fallback active");
            TokenEstimate {
                // A byte cannot encode more than one ordinary tokenizer token;
                // this is intentionally conservative for multibyte text.
                tokens: text.len(),
                used_fallback: true,
            }
        }
    }
}

pub fn count_tokens(text: &str) -> usize {
    estimate_tokens(text).tokens
}

fn add_estimate(total: &mut usize, fallback: &mut bool, text: &str) {
    let estimate = estimate_tokens(text);
    *total = total.saturating_add(estimate.tokens);
    *fallback |= estimate.used_fallback;
}

/// Inputs used to rank complete schema records.
#[derive(Debug, Clone, Copy, Default)]
pub struct SchemaPriorityContext<'a> {
    pub current_request: &'a str,
    pub retained_history: &'a str,
    pub recent_tool_results: &'a str,
}

/// Result metadata for a bounded schema prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedSchemaSummary {
    pub text: String,
    pub included_object_types: usize,
    pub included_edge_types: usize,
    pub omitted_object_types: usize,
    pub omitted_edge_types: usize,
    pub estimated_tokens: usize,
    pub estimation_fallback: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SchemaRecordKind {
    Object,
    Edge,
}

struct SchemaRecord {
    kind: SchemaRecordKind,
    name: String,
    text: String,
    priority: u8,
}

fn contains_type_name(haystack: &str, type_name: &str) -> bool {
    let haystack = haystack.to_lowercase();
    let needle = type_name.to_lowercase();
    haystack.match_indices(&needle).any(|(start, matched)| {
        let end = start + matched.len();
        let left_ok = start == 0
            || !haystack[..start]
                .chars()
                .next_back()
                .is_some_and(|ch| ch.is_alphanumeric() || ch == '_');
        let right_ok = end == haystack.len()
            || !haystack[end..]
                .chars()
                .next()
                .is_some_and(|ch| ch.is_alphanumeric() || ch == '_');
        left_ok && right_ok
    })
}

fn omission_notice(objects: usize, edges: usize) -> String {
    format!(
        "\n[Schema summary omitted {objects} object type(s) and {edges} edge type(s). \
         Search the graph or ask for clarification before using an omitted type.]\n"
    )
}

/// Select whole object/edge records in deterministic priority order.
pub fn bounded_schema_summary(
    schema: &SchemaDefinition,
    context: SchemaPriorityContext<'_>,
    token_budget: usize,
) -> BoundedSchemaSummary {
    let primary = format!("{}\n{}", context.current_request, context.retained_history);
    let mut records = Vec::new();

    for (name, object) in &schema.object_types {
        let mut properties = object.properties.iter().collect::<Vec<_>>();
        properties.sort_by_key(|(name, _)| name.as_str());
        let property_text = properties
            .into_iter()
            .map(|(property_name, property)| {
                let required = if object.required_properties.contains(property_name) {
                    ", required"
                } else {
                    ""
                };
                format!(
                    "`{property_name}` ({kind}{required}): {description}",
                    kind = property.property_type.name(),
                    description = property.description
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let text = if property_text.is_empty() {
            format!("- object `{name}`: {}\n", object.description)
        } else {
            format!(
                "- object `{name}`: {}\n  properties: {property_text}\n",
                object.description
            )
        };
        let priority = if contains_type_name(&primary, name) {
            0
        } else if contains_type_name(context.recent_tool_results, name) {
            1
        } else {
            2
        };
        records.push(SchemaRecord {
            kind: SchemaRecordKind::Object,
            name: name.clone(),
            text,
            priority,
        });
    }

    for (name, edge) in &schema.edge_types {
        let mut sources = edge.allowed_source_types.clone();
        let mut targets = edge.allowed_target_types.clone();
        sources.sort();
        targets.sort();
        let sources = if sources.is_empty() {
            "any".to_string()
        } else {
            sources.join(", ")
        };
        let targets = if targets.is_empty() {
            "any".to_string()
        } else {
            targets.join(", ")
        };
        let text = format!(
            "- edge `{name}`: {}; sources: [{sources}]; targets: [{targets}]; bidirectional: {}\n",
            edge.description, edge.bidirectional
        );
        let priority = if contains_type_name(&primary, name) {
            0
        } else if contains_type_name(context.recent_tool_results, name) {
            1
        } else {
            2
        };
        records.push(SchemaRecord {
            kind: SchemaRecordKind::Edge,
            name: name.clone(),
            text,
            priority,
        });
    }

    records.sort_by(|left, right| {
        (left.priority, left.name.as_str(), left.kind as u8).cmp(&(
            right.priority,
            right.name.as_str(),
            right.kind as u8,
        ))
    });

    let total_objects = schema.object_types.len();
    let total_edges = schema.edge_types.len();
    let header = "## Knowledge Graph Schema\n\n### Available Types\n";
    let mut selected = Vec::new();
    let mut raw_text = header.to_string();
    for record in records {
        let mut trial = raw_text.clone();
        trial.push_str(&record.text);
        if count_tokens(&trial) <= token_budget {
            raw_text = trial;
            selected.push(record);
        }
    }

    let (mut text, mut included_objects, mut included_edges) = loop {
        let included_objects = selected
            .iter()
            .filter(|record| record.kind == SchemaRecordKind::Object)
            .count();
        let included_edges = selected
            .iter()
            .filter(|record| record.kind == SchemaRecordKind::Edge)
            .count();
        let omitted_objects = total_objects - included_objects;
        let omitted_edges = total_edges - included_edges;
        let mut candidate = header.to_string();
        for record in &selected {
            candidate.push_str(&record.text);
        }
        if omitted_objects + omitted_edges > 0 {
            candidate.push_str(&omission_notice(omitted_objects, omitted_edges));
        }
        if count_tokens(&candidate) <= token_budget || selected.is_empty() {
            break (candidate, included_objects, included_edges);
        }
        selected.pop();
    };

    let mut estimate = estimate_tokens(&text);
    if estimate.tokens > token_budget {
        let notice = omission_notice(total_objects, total_edges);
        let notice_estimate = estimate_tokens(&notice);
        if notice_estimate.tokens <= token_budget {
            text = notice;
            estimate = notice_estimate;
            included_objects = 0;
            included_edges = 0;
        } else {
            text.clear();
            estimate = estimate_tokens("");
            included_objects = 0;
            included_edges = 0;
        }
    }

    BoundedSchemaSummary {
        text,
        included_object_types: included_objects,
        included_edge_types: included_edges,
        omitted_object_types: total_objects - included_objects,
        omitted_edge_types: total_edges - included_edges,
        estimated_tokens: estimate.tokens,
        estimation_fallback: estimate.used_fallback,
    }
}

pub(crate) fn compose_preamble(base: &str, guidance: &str, schema: &str) -> String {
    match (base.is_empty(), schema.is_empty()) {
        (true, true) => guidance.to_string(),
        (true, false) => format!("{guidance}\n\n{schema}"),
        (false, true) => format!("{base}\n\n{guidance}"),
        (false, false) => format!("{base}\n\n{guidance}\n\n{schema}"),
    }
}

pub(crate) fn select_history_window(
    history: &[HistoryMessage],
    preamble: &str,
    current_message: &str,
    tool_definition_tokens: usize,
    context_tokens: usize,
    response_reserve_tokens: usize,
) -> Vec<HistoryMessage> {
    let current_message = serialize_message(&Message::user(current_message));
    let fixed = count_tokens(preamble)
        .saturating_add(count_tokens(&current_message))
        .saturating_add(tool_definition_tokens)
        .saturating_add(TOKENS_PER_MESSAGE * 2)
        .saturating_add(REPLY_PRIMING_TOKENS)
        .saturating_add(REQUEST_ENVELOPE_TOKENS);
    let available = context_tokens
        .saturating_sub(response_reserve_tokens)
        .saturating_sub(fixed);
    let mut selected = Vec::new();
    let mut used = 0usize;
    for message in history.iter().rev() {
        let rig_message = if message.role == "assistant" {
            Message::assistant(&message.content)
        } else {
            Message::user(&message.content)
        };
        let cost =
            count_tokens(&serialize_message(&rig_message)).saturating_add(TOKENS_PER_MESSAGE);
        if used.saturating_add(cost) > available {
            break;
        }
        used += cost;
        selected.push(message.clone());
    }
    selected.reverse();
    selected
}

pub(crate) fn estimate_tool_definitions(definitions: &[String]) -> TokenEstimate {
    let mut tokens = 0usize;
    let mut fallback = false;
    for definition in definitions {
        add_estimate(&mut tokens, &mut fallback, definition);
    }
    TokenEstimate {
        tokens,
        used_fallback: fallback,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BudgetTermination {
    Budget(String),
    Repeat(String),
}

#[derive(Clone)]
pub(crate) struct BudgetController {
    inner: Arc<Mutex<BudgetState>>,
    schema: Option<SchemaDefinition>,
    base_prompt: String,
    tool_guidance: String,
    current_request: String,
    retained_history: String,
    tool_definition_tokens: usize,
}

#[derive(Debug, Clone)]
struct TrackedCall {
    fingerprint: String,
    result_hash: Option<u64>,
    unchanged_repeats: usize,
}

struct BudgetState {
    limits: EffectiveAgentBudget,
    diagnostics: AgentBudgetDiagnostics,
    termination: Option<BudgetTermination>,
    tracked_call: Option<TrackedCall>,
    recent_tool_results: VecDeque<String>,
}

fn admit_completion(
    state: &mut BudgetState,
    request_tokens: usize,
    turn: usize,
) -> Result<(), String> {
    let reserve = state.limits.response_reserve_tokens;
    if request_tokens.saturating_add(reserve) > state.limits.context_tokens {
        return Err(format!(
            "Agent stopped before model call {turn}: the estimated request ({request_tokens} input + \
             {reserve} reserved response tokens) exceeds the {}-token context window. \
             Shorten the request/history or reduce schema and response budgets.",
            state.limits.context_tokens
        ));
    }
    state.diagnostics.model_calls += 1;
    state.diagnostics.request_tokens += request_tokens;
    state.diagnostics.reserved_response_tokens += reserve;
    Ok(())
}

impl BudgetController {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        mut limits: EffectiveAgentBudget,
        response_reserve_tokens: usize,
        schema: Option<SchemaDefinition>,
        base_prompt: String,
        tool_guidance: String,
        current_request: String,
        retained_history: String,
        tool_definition_tokens: TokenEstimate,
    ) -> Self {
        limits.response_reserve_tokens = limits
            .response_reserve_tokens
            .min(response_reserve_tokens.max(1));
        let diagnostics = AgentBudgetDiagnostics {
            estimation_fallback: tool_definition_tokens.used_fallback,
            ..AgentBudgetDiagnostics::default()
        };
        Self {
            inner: Arc::new(Mutex::new(BudgetState {
                limits,
                diagnostics,
                termination: None,
                tracked_call: None,
                recent_tool_results: VecDeque::new(),
            })),
            schema,
            base_prompt,
            tool_guidance,
            current_request,
            retained_history,
            tool_definition_tokens: tool_definition_tokens.tokens,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BudgetState> {
        self.inner.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub(crate) fn initial_preamble(&self) -> String {
        self.preamble("")
    }

    fn preamble(&self, recent_tool_results: &str) -> String {
        let budget = self.lock().limits.schema_summary_tokens;
        self.preamble_with_schema_budget(recent_tool_results, budget)
    }

    fn preamble_with_schema_budget(
        &self,
        recent_tool_results: &str,
        schema_budget: usize,
    ) -> String {
        let schema = self.schema.as_ref().map_or_else(String::new, |schema| {
            bounded_schema_summary(
                schema,
                SchemaPriorityContext {
                    current_request: &self.current_request,
                    retained_history: &self.retained_history,
                    recent_tool_results,
                },
                schema_budget,
            )
            .text
        });
        compose_preamble(&self.base_prompt, &self.tool_guidance, &schema)
    }

    pub(crate) fn diagnostics(&self) -> AgentBudgetDiagnostics {
        self.lock().diagnostics.clone()
    }

    pub(crate) fn termination(&self) -> Option<BudgetTermination> {
        self.lock().termination.clone()
    }

    fn set_termination(state: &mut BudgetState, termination: BudgetTermination) {
        if state.termination.is_none() {
            state.termination = Some(termination);
        }
    }
}

fn serialize_message(message: &Message) -> String {
    serde_json::to_string(message).unwrap_or_else(|_| format!("{message:?}"))
}

fn request_tokens(
    preamble: &str,
    prompt: &Message,
    history: &[Message],
    tool_definition_tokens: usize,
) -> TokenEstimate {
    let mut tokens = tool_definition_tokens;
    let mut fallback = false;
    add_estimate(&mut tokens, &mut fallback, preamble);
    add_estimate(&mut tokens, &mut fallback, &serialize_message(prompt));
    for message in history {
        add_estimate(&mut tokens, &mut fallback, &serialize_message(message));
        tokens = tokens.saturating_add(TOKENS_PER_MESSAGE);
    }
    tokens = tokens
        .saturating_add(TOKENS_PER_MESSAGE)
        .saturating_add(REPLY_PRIMING_TOKENS)
        .saturating_add(REQUEST_ENVELOPE_TOKENS);
    TokenEstimate {
        tokens,
        used_fallback: fallback,
    }
}

fn is_leading_orphan(message: &Message) -> bool {
    match message {
        Message::Assistant { .. } => true,
        Message::User { content } => content
            .iter()
            .all(|item| matches!(item, UserContent::ToolResult(_))),
        Message::System { .. } => false,
    }
}

/// Retain the newest valid history suffix that fits one model request.
///
/// Removing leading assistant/tool-result messages avoids splitting a tool
/// interaction when an older prefix is discarded.
fn fit_history(
    preamble: &str,
    prompt: &Message,
    history: &[Message],
    tool_definition_tokens: usize,
    context_tokens: usize,
    response_reserve_tokens: usize,
) -> (Vec<Message>, TokenEstimate) {
    let mut fitted = history.to_vec();
    loop {
        while fitted.first().is_some_and(is_leading_orphan) {
            fitted.remove(0);
        }
        let estimate = request_tokens(preamble, prompt, &fitted, tool_definition_tokens);
        if estimate.tokens.saturating_add(response_reserve_tokens) <= context_tokens
            || fitted.is_empty()
        {
            return (fitted, estimate);
        }
        fitted.remove(0);
    }
}

fn canonical_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort();
            let mut canonical = serde_json::Map::new();
            for key in keys {
                canonical.insert(key.clone(), canonical_json(&values[key]));
            }
            serde_json::Value::Object(canonical)
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(canonical_json).collect())
        }
        value => value.clone(),
    }
}

fn fingerprint(tool_name: &str, args: &str) -> Option<String> {
    let validated_name = match tool_name {
        "search_fts" => "search_fts",
        "search_semantic" => "search_semantic",
        "search_hybrid" => "search_hybrid",
        "upsert_node" => "upsert_node",
        "upsert_edge" => "upsert_edge",
        _ => return None,
    };
    let value: serde_json::Value = serde_json::from_str(args).ok()?;
    tool_validation::validate_tool_args(validated_name, &value).ok()?;
    Some(format!("{tool_name}:{}", canonical_json(&value)))
}

fn stable_hash(value: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn admit_fingerprint(
    tracked_call: &mut Option<TrackedCall>,
    call_fingerprint: String,
    repeated_call_limit: usize,
) -> bool {
    if let Some(tracked) = tracked_call.as_ref()
        && tracked.fingerprint == call_fingerprint
        && tracked.result_hash.is_some()
        && tracked.unchanged_repeats >= repeated_call_limit
    {
        return false;
    }
    if tracked_call
        .as_ref()
        .is_none_or(|tracked| tracked.fingerprint != call_fingerprint)
    {
        *tracked_call = Some(TrackedCall {
            fingerprint: call_fingerprint,
            result_hash: None,
            unchanged_repeats: 0,
        });
    }
    true
}

fn record_fingerprint_result(
    tracked_call: &mut Option<TrackedCall>,
    call_fingerprint: &str,
    result_hash: u64,
    mutation_progress: bool,
    repeated_call_limit: usize,
) -> bool {
    let Some(tracked) = tracked_call
        .as_mut()
        .filter(|tracked| tracked.fingerprint == call_fingerprint)
    else {
        return false;
    };
    if mutation_progress || tracked.result_hash.is_some_and(|hash| hash != result_hash) {
        tracked.unchanged_repeats = 0;
    } else if tracked.result_hash == Some(result_hash) {
        tracked.unchanged_repeats = tracked.unchanged_repeats.saturating_add(1);
    }
    tracked.result_hash = Some(result_hash);
    !mutation_progress
        && tracked.unchanged_repeats > 0
        && tracked.unchanged_repeats >= repeated_call_limit
}

fn bounded_tool_output(tool_name: &str, text: &str, budget: usize) -> Option<String> {
    if count_tokens(text) <= budget {
        return Some(text.to_string());
    }

    let records = text
        .split("\n\n")
        .filter(|record| !record.trim().is_empty())
        .collect::<Vec<_>>();
    if tool_name.starts_with("search_") && records.len() > 1 {
        let result_count = records.len() - 1;
        let mut kept = Vec::new();
        for record in &records {
            let omitted = records.len() - kept.len() - 1;
            let notice = format!(
                "\n\n[Tool output truncated at record boundaries: omitted {omitted} record(s) \
                 of {result_count}. Refine the query or request continuation using the returned IDs.]"
            );
            let mut trial = kept.join("\n\n");
            if !trial.is_empty() {
                trial.push_str("\n\n");
            }
            trial.push_str(record);
            if omitted > 0 {
                trial.push_str(&notice);
            }
            if count_tokens(&trial) <= budget {
                kept.push(*record);
            }
        }
        let omitted = records.len().saturating_sub(kept.len());
        let mut output = kept.join("\n\n");
        if omitted > 0 {
            output.push_str(&format!(
                "\n\n[Tool output truncated at record boundaries: omitted {omitted} record(s) \
                 of {result_count}. Refine the query or request continuation using the returned IDs.]"
            ));
        }
        if !output.is_empty() && count_tokens(&output) <= budget {
            return Some(output);
        }
    }

    let notice = "[Tool output omitted because its complete record exceeds the remaining output \
                  budget. Narrow the query or request fewer records, then continue.]";
    (count_tokens(notice) <= budget).then(|| notice.to_string())
}

impl AgentHook for BudgetController {
    async fn on_completion_call(
        &self,
        _context: &HookContext,
        event: CompletionCallEvent<'_>,
    ) -> CompletionCallAction {
        let recent = {
            let state = self.lock();
            if let Some(termination) = &state.termination {
                return CompletionCallAction::stop(match termination {
                    BudgetTermination::Budget(reason) | BudgetTermination::Repeat(reason) => {
                        reason.clone()
                    }
                });
            }
            state
                .recent_tool_results
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        };
        let (context_tokens, reserve, schema_limit) = {
            let state = self.lock();
            (
                state.limits.context_tokens,
                state.limits.response_reserve_tokens,
                state.limits.schema_summary_tokens,
            )
        };
        let base_preamble = self.preamble_with_schema_budget(&recent, 0);
        let base = request_tokens(
            &base_preamble,
            event.prompt,
            &[],
            self.tool_definition_tokens,
        );
        let available_schema = context_tokens
            .saturating_sub(reserve)
            .saturating_sub(base.tokens);
        let preamble =
            self.preamble_with_schema_budget(&recent, schema_limit.min(available_schema));
        let (history, estimate) = fit_history(
            &preamble,
            event.prompt,
            event.history,
            self.tool_definition_tokens,
            context_tokens,
            reserve,
        );

        let mut state = self.lock();
        state.diagnostics.estimation_fallback |= base.used_fallback || estimate.used_fallback;
        if let Err(reason) = admit_completion(&mut state, estimate.tokens, event.turn) {
            Self::set_termination(&mut state, BudgetTermination::Budget(reason.clone()));
            return CompletionCallAction::stop(reason);
        }
        let mut patch = RequestPatch::new()
            .preamble(preamble)
            .max_tokens(reserve.min(u64::MAX as usize) as u64);
        if history.len() != event.history.len() {
            patch = patch.history(history);
        }
        CompletionCallAction::patch(patch)
    }

    async fn on_tool_call(&self, _context: &HookContext, event: ToolCall<'_>) -> ToolCallAction {
        let estimate = estimate_tokens(event.args);
        let mut state = self.lock();
        state.diagnostics.tool_argument_tokens = state
            .diagnostics
            .tool_argument_tokens
            .saturating_add(estimate.tokens);
        state.diagnostics.estimation_fallback |= estimate.used_fallback;

        let Some(call_fingerprint) = fingerprint(event.tool_name, event.args) else {
            return ToolCallAction::run();
        };
        let repeated_call_limit = state.limits.repeated_call_limit;
        if !admit_fingerprint(
            &mut state.tracked_call,
            call_fingerprint,
            repeated_call_limit,
        ) {
            let reason = format!(
                "Agent stopped unchanged `{}` calls after {} allowed repeat(s). \
                 Change the arguments or continue with a different approach.",
                event.tool_name, state.limits.repeated_call_limit
            );
            Self::set_termination(&mut state, BudgetTermination::Repeat(reason.clone()));
            return ToolCallAction::stop(reason);
        }
        ToolCallAction::run()
    }

    async fn on_tool_result(
        &self,
        _context: &HookContext,
        event: ToolResultEvent<'_>,
    ) -> ToolResultAction {
        let raw_text = event.presentation.render();
        let raw_hash = stable_hash(&raw_text);
        let call_fingerprint = fingerprint(event.tool_name, event.args);
        let mutation_progress =
            event.tool_name.starts_with("upsert_") && event.raw_result.is_success();

        let mut state = self.lock();
        let repeated_call_limit = state.limits.repeated_call_limit;
        let repeated_without_progress = call_fingerprint.as_deref().is_some_and(|fingerprint| {
            record_fingerprint_result(
                &mut state.tracked_call,
                fingerprint,
                raw_hash,
                mutation_progress,
                repeated_call_limit,
            )
        });

        if repeated_without_progress {
            let reason = format!(
                "Agent stopped after `{}` returned the same result for {} allowed repeat(s). \
                 Change the arguments or continue with a different approach.",
                event.tool_name, state.limits.repeated_call_limit
            );
            Self::set_termination(&mut state, BudgetTermination::Repeat(reason));
        }

        let prompt_window = state
            .limits
            .context_tokens
            .saturating_sub(state.limits.response_reserve_tokens);
        let per_result_budget = (prompt_window / 2)
            .max(MIN_TOOL_RESULT_TOKENS)
            .min(prompt_window)
            .min(8_192);
        let Some(output) = bounded_tool_output(event.tool_name, &raw_text, per_result_budget)
        else {
            let reason = format!(
                "Agent stopped after `{}` because its smallest complete result cannot fit this \
                 model's context window. Narrow the request or request fewer records.",
                event.tool_name
            );
            Self::set_termination(&mut state, BudgetTermination::Budget(reason.clone()));
            return ToolResultAction::stop(reason);
        };
        let estimate = estimate_tokens(&output);
        state.diagnostics.tool_output_tokens = state
            .diagnostics
            .tool_output_tokens
            .saturating_add(estimate.tokens);
        state.diagnostics.estimation_fallback |= estimate.used_fallback;
        state.recent_tool_results.push_back(output.clone());
        if state.recent_tool_results.len() > MAX_RECENT_TOOL_RESULTS {
            state.recent_tool_results.pop_front();
        }

        if output == raw_text {
            ToolResultAction::keep()
        } else {
            ToolResultAction::rewrite(output)
        }
    }

    async fn on_stream_response_finish(
        &self,
        _context: &HookContext,
        event: StreamResponseFinish<'_>,
    ) -> ObservationAction {
        let rendered =
            serde_json::to_string(event.content).unwrap_or_else(|_| format!("{:?}", event.content));
        let estimate = estimate_tokens(&rendered);
        let mut state = self.lock();
        state.diagnostics.assistant_output_tokens = state
            .diagnostics
            .assistant_output_tokens
            .saturating_add(estimate.tokens);
        state.diagnostics.estimation_fallback |= estimate.used_fallback;
        ObservationAction::continue_run()
    }

    fn observes(&self, kind: StepEventKind) -> bool {
        matches!(
            kind,
            StepEventKind::CompletionCall
                | StepEventKind::ToolCall
                | StepEventKind::ToolResult
                | StepEventKind::StreamResponseFinish
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use u_forge_core::{EdgeTypeSchema, ObjectTypeSchema, PropertySchema};

    fn schema() -> SchemaDefinition {
        let mut schema = SchemaDefinition::new("test".into(), "1".into(), String::new());
        schema.add_object_type(
            "character".into(),
            ObjectTypeSchema::new("character".into(), "A named person".into())
                .with_property("description".into(), PropertySchema::text("Biography")),
        );
        schema.add_object_type(
            "location".into(),
            ObjectTypeSchema::new("location".into(), "A place".into()),
        );
        schema.add_object_type(
            "魔法".into(),
            ObjectTypeSchema::new("魔法".into(), "多字节描述".into()),
        );
        schema.add_edge_type(
            "located_in".into(),
            EdgeTypeSchema::new("located_in".into(), "Placement".into())
                .with_source_types(vec!["character".into()])
                .with_target_types(vec!["location".into()]),
        );
        schema
    }

    #[test]
    fn bounded_schema_is_deterministic_prioritized_and_honest() {
        let schema = schema();
        let full = bounded_schema_summary(&schema, SchemaPriorityContext::default(), usize::MAX);
        assert_eq!(full.omitted_object_types, 0);
        assert_eq!(full.omitted_edge_types, 0);

        let budget = count_tokens("## Knowledge Graph Schema\n\n### Available Types\n") + 45;
        let first = bounded_schema_summary(
            &schema,
            SchemaPriorityContext {
                current_request: "Tell me about location",
                retained_history: "",
                recent_tool_results: "character",
            },
            budget,
        );
        let second = bounded_schema_summary(
            &schema,
            SchemaPriorityContext {
                current_request: "Tell me about location",
                retained_history: "",
                recent_tool_results: "character",
            },
            budget,
        );
        assert_eq!(first, second);
        assert!(first.estimated_tokens <= budget);
        assert!(first.text.contains("`location`"));
        assert!(first.text.contains("omitted"));
        assert!(
            !first.text.contains("`魔法`") || first.text.contains("- object `魔法`: 多字节描述\n")
        );
    }

    #[test]
    fn schema_priority_orders_request_history_then_tool_results() {
        let summary = bounded_schema_summary(
            &schema(),
            SchemaPriorityContext {
                current_request: "location",
                retained_history: "",
                recent_tool_results: "character",
            },
            usize::MAX,
        );
        let location = summary.text.find("`location`").unwrap();
        let character = summary.text.find("`character`").unwrap();
        let remaining = summary.text.find("`located_in`").unwrap();
        assert!(location < character);
        assert!(character < remaining);
    }

    #[test]
    fn schema_at_exact_budget_keeps_complete_records() {
        let schema = schema();
        let full = bounded_schema_summary(&schema, SchemaPriorityContext::default(), usize::MAX);
        let exact = bounded_schema_summary(
            &schema,
            SchemaPriorityContext::default(),
            full.estimated_tokens,
        );
        assert_eq!(exact.text, full.text);
        let below = bounded_schema_summary(
            &schema,
            SchemaPriorityContext::default(),
            full.estimated_tokens - 1,
        );
        assert!(below.estimated_tokens < full.estimated_tokens);
        assert!(below.omitted_object_types + below.omitted_edge_types > 0);
    }

    #[test]
    fn output_truncation_uses_record_boundaries_and_guidance() {
        let long_body = "details ".repeat(100);
        let output =
            format!("results\n\nrecord one\n\nrecord two {long_body}\n\nrecord three {long_body}");
        let budget = count_tokens("results\n\nrecord one") + 35;
        let bounded = bounded_tool_output("search_fts", &output, budget).unwrap();
        assert!(count_tokens(&bounded) <= budget);
        assert!(bounded.contains("record boundaries"));
        assert!(!bounded.contains("record two details"));
    }

    #[test]
    fn canonical_fingerprint_ignores_object_key_order() {
        let left = fingerprint("search_fts", r#"{"query":"x","limit":2}"#).unwrap();
        let right = fingerprint("search_fts", r#"{"limit":2,"query":"x"}"#).unwrap();
        assert_eq!(left, right);
        let changed = fingerprint("search_fts", r#"{"query":"y","limit":2}"#).unwrap();
        assert_ne!(left, changed);
    }

    #[test]
    fn repeat_state_allows_correction_and_detects_unchanged_reads() {
        let first = fingerprint("search_fts", r#"{"query":"x","limit":2}"#).unwrap();
        let corrected = fingerprint("search_fts", r#"{"query":"y","limit":2}"#).unwrap();
        let mut tracked = None;
        assert!(admit_fingerprint(&mut tracked, first.clone(), 1));
        assert!(!record_fingerprint_result(
            &mut tracked,
            &first,
            stable_hash("validation error"),
            false,
            1,
        ));

        // A changed argument fingerprint is progress and replaces the repeat chain.
        assert!(admit_fingerprint(&mut tracked, corrected.clone(), 1));
        assert!(!record_fingerprint_result(
            &mut tracked,
            &corrected,
            stable_hash("result A"),
            false,
            1,
        ));
        assert!(admit_fingerprint(&mut tracked, corrected.clone(), 1));
        assert!(record_fingerprint_result(
            &mut tracked,
            &corrected,
            stable_hash("result A"),
            false,
            1,
        ));
        assert!(!admit_fingerprint(&mut tracked, corrected, 1));
    }

    #[test]
    fn changing_results_and_mutations_are_progress() {
        let call = fingerprint("search_fts", r#"{"query":"x"}"#).unwrap();
        let mut tracked = None;
        assert!(admit_fingerprint(&mut tracked, call.clone(), 1));
        assert!(!record_fingerprint_result(
            &mut tracked,
            &call,
            stable_hash("first"),
            false,
            1,
        ));
        assert!(!record_fingerprint_result(
            &mut tracked,
            &call,
            stable_hash("changed"),
            false,
            1,
        ));
        assert!(!record_fingerprint_result(
            &mut tracked,
            &call,
            stable_hash("changed"),
            true,
            1,
        ));
    }

    #[test]
    fn zero_repeat_allowance_stops_before_second_execution() {
        let call = fingerprint("search_fts", r#"{"query":"x"}"#).unwrap();
        let mut tracked = None;
        assert!(admit_fingerprint(&mut tracked, call.clone(), 0));
        assert!(!record_fingerprint_result(
            &mut tracked,
            &call,
            stable_hash("same"),
            false,
            0,
        ));
        assert!(!admit_fingerprint(&mut tracked, call, 0));
    }

    #[test]
    fn valid_model_calls_are_not_stopped_by_a_cumulative_cap() {
        let mut state = BudgetState {
            limits: EffectiveAgentBudget {
                context_tokens: 200,
                response_reserve_tokens: 40,
                schema_summary_tokens: 20,
                repeated_call_limit: 1,
                diagnostics: Vec::new(),
            },
            diagnostics: AgentBudgetDiagnostics::default(),
            termination: None,
            tracked_call: None,
            recent_tool_results: VecDeque::new(),
        };
        assert!(admit_completion(&mut state, 80, 1).is_ok());
        assert!(admit_completion(&mut state, 100, 2).is_ok());
        assert_eq!(state.diagnostics.model_calls, 2);
        assert_eq!(state.diagnostics.request_tokens, 180);
    }

    #[test]
    fn per_turn_history_fitting_keeps_a_valid_suffix() {
        let history = vec![
            Message::user("old user ".repeat(60)),
            Message::assistant("old assistant ".repeat(60)),
            Message::user("recent user"),
            Message::assistant("recent assistant"),
        ];
        let (fitted, estimate) =
            fit_history("system", &Message::user("now"), &history, 10, 180, 40);
        assert!(fitted.len() < history.len());
        assert_eq!(fitted.first(), Some(&Message::user("recent user")));
        assert!(estimate.tokens + 40 <= 180);
    }

    #[test]
    fn selected_history_keeps_the_final_request_inside_context() {
        let history = (0..20)
            .map(|index| HistoryMessage {
                role: if index % 2 == 0 { "user" } else { "assistant" }.into(),
                content: format!("history {index} {}", "detail ".repeat(30)),
            })
            .collect::<Vec<_>>();
        let context = 300;
        let reserve = 60;
        let preamble = "system prompt and schema";
        let current = "current request";
        let tool_tokens = 40;
        let selected =
            select_history_window(&history, preamble, current, tool_tokens, context, reserve);
        let estimated = count_tokens(preamble)
            + count_tokens(&serialize_message(&Message::user(current)))
            + tool_tokens
            + selected
                .iter()
                .map(|message| {
                    let rig_message = if message.role == "assistant" {
                        Message::assistant(&message.content)
                    } else {
                        Message::user(&message.content)
                    };
                    count_tokens(&serialize_message(&rig_message)) + TOKENS_PER_MESSAGE
                })
                .sum::<usize>()
            + TOKENS_PER_MESSAGE * 2
            + REPLY_PRIMING_TOKENS
            + REQUEST_ENVELOPE_TOKENS;
        assert!(estimated + reserve <= context);
        assert!(selected.len() < history.len());
    }
}
