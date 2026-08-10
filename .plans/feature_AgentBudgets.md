# Feature Plan: Agent Context, Token, and Tool Budgets

## Status

Implemented. Tool JSON-schema validation and conversation-history windowing
remain in place; schema injection and the multi-turn tool loop now share
model-aware budgets and explicit circuit-breaker outcomes.

## Configuration and accounting

- [x] **AB-01 — Budget configuration.** Add explicit schema-summary,
  cumulative-request, cumulative-tool-output, and repeated-call limits under
  chat/agent configuration with safe defaults and validation against the active
  model context window.
- [x] **AB-02 — One tokenizer policy.** Reuse the cached tokenizer/estimator for
  system prompt, schema summary, history, current message, assistant output,
  tool arguments, and tool results. Record estimation fallback explicitly.
- [x] **AB-03 — Per-request ledger.** Track consumed and reserved tokens across
  every model/tool turn; expose safe summary fields to tracing and completion
  diagnostics.

## Schema summary selection

- [x] **AB-04 — Bounded schema prompt.** Enforce the configured schema budget
  before building the Rig agent.
- [x] **AB-05 — Deterministic prioritization.** Prefer object/edge types named in
  the current request and retained history, then types involved in recent tool
  results, then remaining types in stable name order.
- [x] **AB-06 — Honest truncation.** Add a compact notice describing omitted
  type counts and instruct the model to search/ask for clarification; never
  truncate JSON or a schema entry mid-record.

## Tool-loop circuit breakers

- [x] **AB-07 — Cumulative stop.** Stop before a model/tool call that cannot fit
  the remaining request budget plus response reserve. Return a clear partial
  completion reason rather than a generic transport error.
- [x] **AB-08 — Repeat fingerprint.** Canonicalize tool name plus validated JSON
  arguments. Allow a limited repeat only when the previous result was a
  correctable validation/tool error; otherwise break an unchanged loop.
- [x] **AB-09 — Progress detection.** Treat new tool arguments, new graph
  mutations, or materially different results as progress. Repeated reads with
  identical output consume the repeat allowance.
- [x] **AB-10 — Output bounds.** Truncate oversized tool results at semantic
  record boundaries with counts and continuation guidance. Search tools should
  prefer IDs/summaries over duplicating full node blobs.
- [x] **AB-11 — Streaming outcome.** Add explicit budget/repeat termination
  events so ChatPanel can distinguish a deliberate circuit breaker from a
  provider failure.

## Tests and acceptance

- Schema tests cover below/at/above-budget summaries, deterministic ordering,
  multibyte text, and omission notices.
- Agent-loop tests cover exact repeated calls, corrected arguments, changing
  results, mutation progress, oversized tool output, cumulative exhaustion,
  and preservation of the configured max-turn ceiling.
- The final request sent to the model never exceeds the effective context
  window after response reserve.
- Normal short tool workflows remain unchanged and all budget exits provide an
  actionable user-visible reason.
