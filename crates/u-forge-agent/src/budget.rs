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
use rig::completion::message::{AssistantContent, Message, UserContent};
use tiktoken_rs::CoreBPE;
use u_forge_core::lemonade::AgentBudgetDiagnostics;
use u_forge_core::{EffectiveAgentBudget, SchemaDefinition};

use crate::{HistoryMessage, tools::validation};

const TOKENS_PER_MESSAGE: usize = 4;
const REPLY_PRIMING_TOKENS: usize = 3;
const REQUEST_ENVELOPE_TOKENS: usize = 32;
const SCHEMA_HEADER: &str = "## Knowledge Graph Schema\n\n### Available Types\n";
const HISTORY_OMISSION_NOTICE: &str = "[Conversation context: earlier messages were omitted because this request reached the active model's context window. Treat the visible messages as the most recent portion of a longer conversation.]";

static O200K_BPE: LazyLock<Result<CoreBPE, String>> = LazyLock::new(|| {
    tiktoken_rs::o200k_harmony().map_err(|error| format!("o200k tokenizer unavailable: {error}"))
});

/// A token count plus whether the conservative byte-count fallback was used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenEstimate {
    pub tokens: usize,
    pub used_fallback: bool,
}

impl TokenEstimate {
    fn plus(self, other: Self) -> Self {
        Self {
            tokens: self.tokens.saturating_add(other.tokens),
            used_fallback: self.used_fallback || other.used_fallback,
        }
    }
}

#[cfg(test)]
#[derive(Default, Clone, Copy, Debug)]
struct FitWork {
    serializations: usize,
    schema_records: usize,
    estimates: usize,
    estimator_bytes: usize,
}

#[cfg(test)]
thread_local! {
    static FIT_WORK: std::cell::Cell<FitWork> = std::cell::Cell::new(FitWork::default());
}

#[cfg(test)]
fn record_work(update: impl FnOnce(&mut FitWork)) {
    FIT_WORK.with(|work| {
        let mut counts = work.get();
        update(&mut counts);
        work.set(counts);
    });
}

/// Estimate text with the single cached tokenizer policy used by the agent.
pub fn estimate_tokens(text: &str) -> TokenEstimate {
    #[cfg(test)]
    record_work(|work| {
        work.estimates += 1;
        work.estimator_bytes += text.len();
    });
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

fn contains_type_name(haystack: &str, needle: &str) -> bool {
    haystack.match_indices(needle).any(|(start, matched)| {
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
    let total_objects = schema.object_types.len();
    let total_edges = schema.edge_types.len();
    if token_budget == 0 {
        return empty_schema_summary(total_objects, total_edges);
    }

    let primary =
        format!("{}\n{}", context.current_request, context.retained_history).to_lowercase();
    let recent = context.recent_tool_results.to_lowercase();
    let priority = |name: &str| {
        let needle = name.to_lowercase();
        if contains_type_name(&primary, &needle) {
            0
        } else if contains_type_name(&recent, &needle) {
            1
        } else {
            2
        }
    };
    let mut records = Vec::new();

    for (name, object) in &schema.object_types {
        #[cfg(test)]
        record_work(|work| work.schema_records += 1);
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
        records.push(SchemaRecord {
            kind: SchemaRecordKind::Object,
            name: name.clone(),
            text,
            priority: priority(name),
        });
    }

    for (name, edge) in &schema.edge_types {
        #[cfg(test)]
        record_work(|work| work.schema_records += 1);
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
        records.push(SchemaRecord {
            kind: SchemaRecordKind::Edge,
            name: name.clone(),
            text,
            priority: priority(name),
        });
    }

    records.sort_by(|left, right| {
        (left.priority, left.name.as_str(), left.kind as u8).cmp(&(
            right.priority,
            right.name.as_str(),
            right.kind as u8,
        ))
    });

    // Measure the complete text first: the common all-fit case takes one estimate,
    // including at the exact boundary where separate token counts can disagree.
    let full = render_schema_summary(records.iter(), total_objects, total_edges);
    if full.estimated_tokens <= token_budget {
        return full;
    }

    // Under pressure, use standalone record costs to propose a priority-ordered
    // selection, skipping records too large to fit. Only a verified joined
    // candidate (including its omission notice) may be returned.
    let mut remaining = token_budget.saturating_sub(count_tokens(SCHEMA_HEADER));
    let selected: Vec<_> = records
        .iter()
        .filter(|record| {
            let cost = count_tokens(&record.text);
            if cost > remaining {
                return false;
            }
            remaining -= cost;
            true
        })
        .collect();
    if selected.len() != records.len() {
        let candidate = render_schema_summary(selected.iter().copied(), total_objects, total_edges);
        if candidate.estimated_tokens <= token_budget {
            return candidate;
        }
    }

    // Reclaim space from the lowest-priority selected records. Use logarithmically
    // many probes rather than estimating every shorter prefix. Token counts need
    // not be monotonic across joins, so keep only a candidate we actually checked.
    let mut best = None;
    let (mut lo, mut hi) = (0, selected.len());
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let candidate =
            render_schema_summary(selected[..mid].iter().copied(), total_objects, total_edges);
        if candidate.estimated_tokens <= token_budget {
            best = Some(candidate);
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    best.unwrap_or_else(|| {
        let text = omission_notice(total_objects, total_edges);
        let estimate = estimate_tokens(&text);
        let mut summary = empty_schema_summary(total_objects, total_edges);
        summary.estimation_fallback = estimate.used_fallback;
        if estimate.tokens <= token_budget {
            summary.text = text;
            summary.estimated_tokens = estimate.tokens;
        }
        summary
    })
}

fn empty_schema_summary(objects: usize, edges: usize) -> BoundedSchemaSummary {
    BoundedSchemaSummary {
        text: String::new(),
        included_object_types: 0,
        included_edge_types: 0,
        omitted_object_types: objects,
        omitted_edge_types: edges,
        estimated_tokens: 0,
        estimation_fallback: false,
    }
}

fn render_schema_summary<'a>(
    records: impl Iterator<Item = &'a SchemaRecord>,
    total_objects: usize,
    total_edges: usize,
) -> BoundedSchemaSummary {
    let mut summary = empty_schema_summary(total_objects, total_edges);
    summary.text.push_str(SCHEMA_HEADER);
    for record in records {
        summary.text.push_str(&record.text);
        match record.kind {
            SchemaRecordKind::Object => summary.included_object_types += 1,
            SchemaRecordKind::Edge => summary.included_edge_types += 1,
        }
    }
    summary.omitted_object_types -= summary.included_object_types;
    summary.omitted_edge_types -= summary.included_edge_types;
    if summary.omitted_object_types + summary.omitted_edge_types > 0 {
        summary.text.push_str(&omission_notice(
            summary.omitted_object_types,
            summary.omitted_edge_types,
        ));
    }
    let estimate = estimate_tokens(&summary.text);
    summary.estimated_tokens = estimate.tokens;
    summary.estimation_fallback = estimate.used_fallback;
    summary
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
) -> Vec<HistoryMessage> {
    let current_message = serialize_message(&Message::user(current_message));
    let fixed = count_tokens(preamble)
        .saturating_add(count_tokens(&current_message))
        .saturating_add(tool_definition_tokens)
        .saturating_add(TOKENS_PER_MESSAGE * 2)
        .saturating_add(REPLY_PRIMING_TOKENS)
        .saturating_add(REQUEST_ENVELOPE_TOKENS);
    let available = context_tokens.saturating_sub(fixed);
    let select = |budget: usize| {
        let mut selected = Vec::new();
        let mut used = 0usize;
        for message in history.iter().rev() {
            let rig_message = match message.role.as_str() {
                "assistant" => Message::assistant(&message.content),
                "system" => Message::system(&message.content),
                _ => Message::user(&message.content),
            };
            let cost =
                count_tokens(&serialize_message(&rig_message)).saturating_add(TOKENS_PER_MESSAGE);
            if used.saturating_add(cost) > budget {
                break;
            }
            used += cost;
            selected.push(message.clone());
        }
        selected.reverse();
        while selected
            .first()
            .is_some_and(|message| message.role == "assistant")
        {
            selected.remove(0);
        }
        selected
    };

    let selected = select(available);
    if selected.len() == history.len() {
        return selected;
    }

    let notice_cost = count_tokens(&serialize_message(&Message::system(
        HISTORY_OMISSION_NOTICE,
    )))
    .saturating_add(TOKENS_PER_MESSAGE);
    if notice_cost > available {
        return Vec::new();
    }
    let mut selected = select(available - notice_cost);
    selected.insert(
        0,
        HistoryMessage {
            role: "system".into(),
            content: HISTORY_OMISSION_NOTICE.into(),
        },
    );
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
    if request_tokens > state.limits.context_tokens {
        return Err(format!(
            "Agent stopped before model call {turn}: the estimated {request_tokens}-token input \
             exceeds the active model's {}-token context window even after older history was \
             removed.",
            state.limits.context_tokens
        ));
    }
    state.diagnostics.model_calls += 1;
    state.diagnostics.request_tokens += request_tokens;
    Ok(())
}

impl BudgetController {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        limits: EffectiveAgentBudget,
        schema: Option<SchemaDefinition>,
        base_prompt: String,
        tool_guidance: String,
        current_request: String,
        retained_history: String,
        tool_definition_tokens: TokenEstimate,
    ) -> Self {
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

    pub(crate) fn base_preamble(&self) -> String {
        compose_preamble(&self.base_prompt, &self.tool_guidance, "")
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
    #[cfg(test)]
    record_work(|work| work.serializations += 1);
    serde_json::to_string(message).unwrap_or_else(|_| format!("{message:?}"))
}

// Independent reference for verifying actual patched requests in tests.
#[cfg(test)]
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

/// Locate the assistant tool-call message required to interpret a tool-result
/// prompt. Rig moves the newly committed tool result into `prompt` and leaves
/// its matching call in `history` for the following model turn.
fn required_tool_call_history_start(prompt: &Message, history: &[Message]) -> Option<usize> {
    let Message::User { content } = prompt else {
        return None;
    };
    let result_ids = content
        .iter()
        .filter_map(|item| match item {
            UserContent::ToolResult(result) => Some(result.id.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if result_ids.is_empty() {
        return None;
    }

    history.iter().rposition(|message| {
        let Message::Assistant { content, .. } = message else {
            return false;
        };
        result_ids.iter().all(|result_id| {
            content.iter().any(
                |item| matches!(item, AssistantContent::ToolCall(call) if call.id == *result_id),
            )
        })
    })
}

/// Costs shared by both fits of one completion. A suffix table makes trimming
/// independent of message size and keeps fallback tracking on the retained suffix.
struct CompletionInput<'a> {
    history: &'a [Message],
    suffix_costs: Vec<TokenEstimate>,
    fixed_cost: TokenEstimate,
    required_start: Option<usize>,
}

struct FittedRequest {
    preamble: String,
    history_start: usize,
    estimate: TokenEstimate,
}

impl<'a> CompletionInput<'a> {
    fn new(prompt: &Message, history: &'a [Message], tool_definition_tokens: usize) -> Self {
        let message_cost = |message| {
            let mut estimate = estimate_tokens(&serialize_message(message));
            estimate.tokens = estimate.tokens.saturating_add(TOKENS_PER_MESSAGE);
            estimate
        };
        let mut suffix_costs = vec![
            TokenEstimate {
                tokens: 0,
                used_fallback: false
            };
            history.len() + 1
        ];
        for (index, message) in history.iter().enumerate().rev() {
            suffix_costs[index] = suffix_costs[index + 1].plus(message_cost(message));
        }
        let fixed_cost = message_cost(prompt).plus(TokenEstimate {
            tokens: tool_definition_tokens
                .saturating_add(REPLY_PRIMING_TOKENS + REQUEST_ENVELOPE_TOKENS),
            used_fallback: false,
        });
        Self {
            history,
            suffix_costs,
            fixed_cost,
            required_start: required_tool_call_history_start(prompt, history),
        }
    }

    fn skip_orphans(&self, mut start: usize) -> usize {
        while start < self.history.len()
            && Some(start) != self.required_start
            && is_leading_orphan(&self.history[start])
        {
            start += 1;
        }
        start
    }

    // At the shortest valid suffix, return the real estimate even if it is
    // over budget so the caller reports overflow instead of dropping a call.
    fn fits_or_cannot_trim(&self, start: usize, estimate: TokenEstimate, limit: usize) -> bool {
        estimate.tokens <= limit
            || start == self.history.len()
            || Some(start) == self.required_start
    }

    fn fit(&self, mut preamble: String, min_start: usize, limit: usize) -> FittedRequest {
        let mut start = self.skip_orphans(min_start);
        let fixed_cost = |preamble: &str| self.fixed_cost.plus(estimate_tokens(preamble));
        if start == 0 {
            let estimate = fixed_cost(&preamble).plus(self.suffix_costs[0]);
            if self.fits_or_cannot_trim(0, estimate, limit) {
                return FittedRequest {
                    preamble,
                    history_start: 0,
                    estimate,
                };
            }
            start = self.skip_orphans(1);
        }

        // Estimate the joined preamble once and reuse it for all shortened suffixes.
        preamble.push_str("\n\n");
        preamble.push_str(HISTORY_OMISSION_NOTICE);
        let fixed = fixed_cost(&preamble);
        loop {
            let estimate = fixed.plus(self.suffix_costs[start]);
            if self.fits_or_cannot_trim(start, estimate, limit) {
                return FittedRequest {
                    preamble,
                    history_start: start,
                    estimate,
                };
            }
            start = self.skip_orphans(start + 1);
        }
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
    let spec = validation::find(tool_name)?;
    let value: serde_json::Value = serde_json::from_str(args).ok()?;
    validation::validate_tool_args(spec.name, &value).ok()?;
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

impl BudgetController {
    fn completion_call(&self, event: CompletionCallEvent<'_>) -> CompletionCallAction {
        let context_tokens = {
            let state = self.lock();
            if let Some(termination) = &state.termination {
                return CompletionCallAction::stop(match termination {
                    BudgetTermination::Budget(reason) | BudgetTermination::Repeat(reason) => {
                        reason.clone()
                    }
                });
            }
            state.limits.context_tokens
        };
        let input = CompletionInput::new(event.prompt, event.history, self.tool_definition_tokens);
        let base = input.fit(self.base_preamble(), 0, context_tokens);
        let base_fallback = base.estimate.used_fallback;
        let available_schema = context_tokens.saturating_sub(base.estimate.tokens);
        let fitted = match &self.schema {
            Some(schema) if available_schema > 0 => {
                let recent = self
                    .lock()
                    .recent_tool_results
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join("\n");
                let summary = bounded_schema_summary(
                    schema,
                    SchemaPriorityContext {
                        current_request: &self.current_request,
                        retained_history: &self.retained_history,
                        recent_tool_results: &recent,
                    },
                    available_schema,
                );
                if summary.text.is_empty() {
                    base
                } else {
                    let preamble =
                        compose_preamble(&self.base_prompt, &self.tool_guidance, &summary.text);
                    input.fit(preamble, base.history_start, context_tokens)
                }
            }
            _ => base,
        };

        let mut state = self.lock();
        state.diagnostics.estimation_fallback |= base_fallback || fitted.estimate.used_fallback;
        if let Err(reason) = admit_completion(&mut state, fitted.estimate.tokens, event.turn) {
            Self::set_termination(&mut state, BudgetTermination::Budget(reason.clone()));
            return CompletionCallAction::stop(reason);
        }
        let mut patch = RequestPatch::new().preamble(fitted.preamble);
        if fitted.history_start != 0 {
            patch = patch.history(event.history[fitted.history_start..].iter().cloned());
        }
        CompletionCallAction::patch(patch)
    }
}

impl AgentHook for BudgetController {
    async fn on_completion_call(
        &self,
        _context: &HookContext,
        event: CompletionCallEvent<'_>,
    ) -> CompletionCallAction {
        self.completion_call(event)
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
            validation::is_mutation(event.tool_name) && event.raw_result.is_success();

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

        let estimate = estimate_tokens(&raw_text);
        state.diagnostics.tool_output_tokens = state
            .diagnostics
            .tool_output_tokens
            .saturating_add(estimate.tokens);
        state.diagnostics.estimation_fallback |= estimate.used_fallback;
        state.recent_tool_results.push_back(raw_text);
        ToolResultAction::keep()
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
    use rig::OneOrMany;
    use rig::completion::message::{ToolCall as MessageToolCall, ToolFunction};
    use serde_json::json;
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

    fn measure_fit<T>(fit: impl FnOnce() -> T) -> (T, FitWork) {
        FIT_WORK.set(FitWork::default());
        let result = fit();
        (result, FIT_WORK.take())
    }

    fn controller(schema: Option<SchemaDefinition>, context_tokens: usize) -> BudgetController {
        BudgetController::new(
            EffectiveAgentBudget {
                context_tokens,
                ..EffectiveAgentBudget::default()
            },
            schema,
            "system".into(),
            "guidance".into(),
            "now".into(),
            String::new(),
            TokenEstimate {
                tokens: 10,
                used_fallback: false,
            },
        )
    }

    fn request_event<'a>(prompt: &'a Message, history: &'a [Message]) -> CompletionCallEvent<'a> {
        CompletionCallEvent {
            prompt,
            history,
            turn: 1,
        }
    }

    #[test]
    fn schema_fitting_avoids_repeated_prefix_work() {
        let mut schema = SchemaDefinition::new("test".into(), "1".into(), String::new());
        for index in 0..256 {
            let name = format!("record_{index:03}");
            schema.add_object_type(
                name.clone(),
                ObjectTypeSchema::new(name, "Description".into()),
            );
        }
        let (empty, zero_work) =
            measure_fit(|| bounded_schema_summary(&schema, SchemaPriorityContext::default(), 0));
        let full = bounded_schema_summary(&schema, SchemaPriorityContext::default(), usize::MAX);
        let (exact, exact_work) = measure_fit(|| {
            bounded_schema_summary(
                &schema,
                SchemaPriorityContext::default(),
                full.estimated_tokens,
            )
        });
        eprintln!("schema work: zero={zero_work:?}, exact={exact_work:?}");
        assert_eq!(exact, full);
        assert_eq!(exact.included_object_types, 256);
        assert_eq!(exact_work.schema_records, 256);
        assert_eq!(exact_work.estimates, 1);
        assert_eq!(exact_work.estimator_bytes, exact.text.len());
        let (partial, partial_work) = measure_fit(|| {
            bounded_schema_summary(
                &schema,
                SchemaPriorityContext::default(),
                full.estimated_tokens - 1,
            )
        });
        assert!(partial.included_object_types > 0 && partial.omitted_object_types > 0);
        assert_eq!(partial.estimated_tokens, count_tokens(&partial.text));
        assert!(partial.estimated_tokens < full.estimated_tokens);
        assert!(partial_work.estimates <= 256 + 12, "{partial_work:?}");
        assert!(
            partial_work.estimator_bytes <= full.text.len() * 12,
            "{partial_work:?}"
        );
        assert!(empty.text.is_empty());
        assert_eq!(empty.omitted_object_types, 256);
        assert_eq!(zero_work.schema_records, 0);
        assert_eq!(zero_work.estimates, 0);
        assert_eq!(zero_work.estimator_bytes, 0);
    }

    #[test]
    fn completion_fitting_estimates_each_message_once_even_when_all_history_is_removed() {
        let history = (0..256)
            .map(|index| Message::user(format!("history {index:03}")))
            .collect::<Vec<_>>();
        let prompt = Message::user("now");
        let preamble = compose_preamble("system", "guidance", "");
        let preamble_with_notice = format!("{preamble}\n\n{HISTORY_OMISSION_NOTICE}");
        let context = request_tokens(&preamble_with_notice, &prompt, &[], 10).tokens;
        let message_bytes = history
            .iter()
            .chain([&prompt])
            .map(|m| serialize_message(m).len())
            .sum::<usize>();
        let budget = controller(Some(schema()), context);
        let (action, work) =
            measure_fit(|| budget.completion_call(request_event(&prompt, &history)));
        eprintln!("completion work: {work:?}");
        let CompletionCallAction::Patch(patch) = action else {
            panic!("expected a fitted request")
        };
        assert_eq!(
            patch.preamble.as_deref(),
            Some(preamble_with_notice.as_str())
        );
        assert_eq!(patch.history, Some(vec![]));
        assert_eq!(budget.diagnostics().request_tokens, context);
        assert_eq!(work.serializations, 257);
        assert_eq!(work.estimates, 259);
        assert_eq!(
            work.estimator_bytes,
            message_bytes + preamble.len() + preamble_with_notice.len()
        );
        assert_eq!(work.schema_records, 0);
    }

    #[test]
    fn completion_reuses_estimates_across_both_fits() {
        let history = [
            Message::user("older history ".repeat(300)),
            Message::user("previous request"),
            Message::assistant("previous reply"),
        ];
        let prompt = Message::user("now");
        for (context, expected_start) in [(4096, 0), (300, 1)] {
            let budget = controller(Some(schema()), context);
            let (action, work) =
                measure_fit(|| budget.completion_call(request_event(&prompt, &history)));
            let CompletionCallAction::Patch(patch) = action else {
                panic!("expected a fitted request")
            };
            let retained = (expected_start != 0).then_some(&history[expected_start..]);
            assert_eq!(patch.history.as_deref(), retained);
            let preamble = patch.preamble.unwrap();
            assert!(preamble.contains(SCHEMA_HEADER));
            assert_eq!(
                preamble.matches(HISTORY_OMISSION_NOTICE).count(),
                expected_start
            );
            assert_eq!(work.serializations, 4);
            assert_eq!(work.schema_records, 4);
            let estimate = request_tokens(&preamble, &prompt, &history[expected_start..], 10);
            assert!(estimate.tokens <= context);
            assert_eq!(budget.diagnostics().request_tokens, estimate.tokens);
        }
    }

    #[test]
    fn bounded_schema_is_deterministic_prioritized_and_honest() {
        let schema = schema();
        let full = bounded_schema_summary(&schema, SchemaPriorityContext::default(), usize::MAX);
        assert_eq!(full.omitted_object_types, 0);
        assert_eq!(full.omitted_edge_types, 0);

        let budget = count_tokens(SCHEMA_HEADER) + 45;
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
        assert_eq!(first.estimated_tokens, count_tokens(&first.text));
        assert!(first.estimated_tokens <= budget);
        assert!(first.text.contains("- object `location`: A place\n"));
        assert!(first.text.ends_with(&omission_notice(
            first.omitted_object_types,
            first.omitted_edge_types
        )));
        assert_eq!(
            first.included_object_types + first.omitted_object_types,
            schema.object_types.len()
        );
        assert_eq!(
            first.included_edge_types + first.omitted_edge_types,
            schema.edge_types.len()
        );
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
        let prompt = Message::user("now");
        let input = CompletionInput::new(&prompt, &history, 10);
        let full_cost = request_tokens("system", &prompt, &history, 10);
        let exact = input.fit("system".into(), 0, full_cost.tokens);
        assert_eq!(exact.history_start, 0);
        assert_eq!(exact.estimate, full_cost);

        let below = input.fit("system".into(), 0, full_cost.tokens - 1);
        assert_eq!(below.history_start, 2);
        assert_eq!(below.preamble.matches(HISTORY_OMISSION_NOTICE).count(), 1);
        assert_eq!(
            below.estimate,
            request_tokens(&below.preamble, &prompt, &history[2..], 10)
        );
        assert!(below.estimate.tokens < full_cost.tokens);
    }

    #[test]
    fn tool_result_prompt_keeps_its_matching_assistant_call() {
        let matching_call = Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::ToolCall(MessageToolCall::new(
                "search-call-1".into(),
                ToolFunction::new("search_semantic".into(), json!({"query": "Z-Rho"})),
            ))),
        };
        let prompt = Message::tool_result(
            "search-call-1",
            format!("Semantic search results:\n\n{}", "Z-Rho lore ".repeat(180)),
        );
        let history = vec![
            Message::user("old user context ".repeat(160)),
            Message::assistant("old assistant context ".repeat(160)),
            Message::user("Find everything about Z-Rho"),
            matching_call.clone(),
        ];
        let preamble = compose_preamble("system", "guidance", "");
        let required_preamble = format!("{preamble}\n\n{HISTORY_OMISSION_NOTICE}");
        let context = request_tokens(
            &required_preamble,
            &prompt,
            std::slice::from_ref(&matching_call),
            10,
        )
        .tokens;
        let budget = controller(None, context);
        let CompletionCallAction::Patch(patch) =
            budget.completion_call(request_event(&prompt, &history))
        else {
            panic!("expected a fitted tool transaction");
        };
        assert_eq!(patch.preamble.as_deref(), Some(required_preamble.as_str()));
        assert_eq!(patch.history, Some(vec![matching_call]));
        assert_eq!(budget.diagnostics().request_tokens, context);

        let budget = controller(None, context - 1);
        let CompletionCallAction::Stop(reason) =
            budget.completion_call(request_event(&prompt, &history))
        else {
            panic!("expected context overflow rather than an orphaned tool result");
        };
        assert!(reason.contains(&format!("estimated {context}-token input")));
        assert_eq!(
            budget.termination(),
            Some(BudgetTermination::Budget(reason))
        );
        assert_eq!(budget.diagnostics().model_calls, 0);
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
        let preamble = "system prompt and schema";
        let current = "current request";
        let tool_tokens = 40;
        let selected = select_history_window(&history, preamble, current, tool_tokens, context);
        let estimated = count_tokens(preamble)
            + count_tokens(&serialize_message(&Message::user(current)))
            + tool_tokens
            + selected
                .iter()
                .map(|message| {
                    let rig_message = match message.role.as_str() {
                        "assistant" => Message::assistant(&message.content),
                        "system" => Message::system(&message.content),
                        _ => Message::user(&message.content),
                    };
                    count_tokens(&serialize_message(&rig_message)) + TOKENS_PER_MESSAGE
                })
                .sum::<usize>()
            + TOKENS_PER_MESSAGE * 2
            + REPLY_PRIMING_TOKENS
            + REQUEST_ENVELOPE_TOKENS;
        assert!(estimated <= context);
        assert!(selected.len() < history.len());
        assert_eq!(
            selected.first().map(|message| message.role.as_str()),
            Some("system")
        );
        assert!(selected[0].content.contains("longer conversation"));
    }
}
