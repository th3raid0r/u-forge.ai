# Feature: Typed Chat Run Lifecycle

## Status: Planned — impact rank 4

- **Primary candidate:** `AGENT-01`
- **Bundled supporting candidates:** `AGENT-04`, `AGENT-09`, `UI-08`
- **Acceptance outcome:** remove `ALLOW-10` by replacing the argument-heavy stream method with a run request value.

## Goal

Give an agent run and a direct-provider run one explicit lifecycle shape: a request, event stream, parent cancellation token, awaitable completion, and exactly one typed terminal outcome. Producer-specific protocols remain separate, but the desktop should use one event pump and the existing `ChatRunReducer` as its only presentation/finalization boundary.

This rewrite addresses state ownership across `u-forge-agent` and GPUI. It is not complete if the two stream loops remain and only their send calls are hidden behind helpers.

## Why this ranks fourth

The current agent stream method coordinates Rig polling, GPU release/reacquisition, runtime lease ownership, cancellation, event translation, context-overflow/repeat interpretation, fallback text, diagnostics, and terminal sends. `ChatPanel` then repeats a second lifecycle state machine for agent events and direct provider tokens. Consolidating the contract removes complexity at both the producer and UI boundaries.

## Current authority and affected code

- `crates/u-forge-agent/src/stream.rs` — agent producer and Rig event translation.
- `crates/u-forge-agent/src/agent.rs` — agent request preparation.
- `crates/u-forge-agent/src/budget.rs` — transitional context accounting, repeat termination, and diagnostics being replaced by adaptive context optimization.
- `crates/u-forge/src/chat_panel.rs` — runtime acquisition, producer selection, and two stream pumps.
- `crates/u-forge/src/chat_panel/run.rs` — existing transport-neutral reducer and terminal presentation.
- `crates/u-forge-core/src/lemonade/chat.rs` — direct provider stream and shared `ChatEvent` vocabulary.
- `crates/u-forge-core/src/lemonade/runtime.rs` and `gpu_manager.rs` — lease and GPU ownership.

## Required invariants

- A runtime lease remains alive through the complete direct-provider or Rig operation.
- GPU ownership is acquired before LLM work, released at tool-call boundaries, and reacquired before the next model turn.
- Queue-backed tools cannot deadlock behind the LLM GPU guard.
- One parent `CancellationToken` governs model work and every queue-backed tool in the run.
- User cancellation and supersession remain distinct terminal outcomes.
- Receiver drop stops further model polling and prevents unobserved write-tool execution, but explicit cancellation remains the primary contract.
- Completion is observed after cancellation so runtime/GPU release is known.
- Reasoning, text, tool call, and tool result events preserve chronological order.
- Terminal fallback text is used only when no streamed assistant text was presented for the final turn.
- Exactly one terminal outcome reaches `ChatRunReducer`; late events are ignored.
- Usage may arrive after a provider finish-reason token. A protocol finish reason alone does not end lifecycle ownership before stream closure/completion.
- Streaming updates notify the affected chat message entity, not the root view on every token.
- `ChatRunReducer` remains the only UI mutation, terminal cleanup, and persistence boundary.
- GPUI stores both the task and parent token; generation/epoch checks reject late presentation but do not replace cancellation.

## Target design

### Agent boundary

Replace the argument-heavy method with an `AgentRunRequest` carrying model/profile, prompt/history, reasoning, effective agent parameters, runtime lease, GPU policy, and parent token. Return an `AgentRunHandle` with an event receiver and an awaitable typed completion.

The agent driver owns one terminal path. Event emission may stop when the receiver closes, but completion must still represent why the run ended and ensure owned resources are dropped.

### UI boundary

Introduce a UI-local chat source adapter for agent and direct-provider producers. Both adapters yield `ChatRunEvent` values and a completion. `ChatPanel` chooses a producer once, then runs one GPUI bridge loop that applies events through `ChatRunReducer`.

Do not move GPUI presentation types into core or agent crates. Transport/domain outcomes should be translated once at the UI adapter boundary.

## Implementation stages

### 1. Characterize transcripts and terminal behavior

- Add synthetic agent sequences for text-only, reasoning then text, tool call/result then final text, fallback-only final response, irreducible context overflow after compaction, repeat stop, provider error, cancellation, and receiver drop.
- Add direct-provider sequences where finish reason precedes usage and stream closure.
- Lock down exact-one-terminal behavior and chronological row ordering in `chat_panel/run.rs` tests.

### 2. Introduce request and handle types

- Replace the agent method’s positional parameters with `AgentRunRequest` and remove `ALLOW-10`.
- Return a handle containing events, completion, and the parent cancellation token or an equivalent ownership-safe API.
- Define a typed agent completion that distinguishes success, irreducible context overflow, repeat stop, cancellation, supersession, provider/agent failure, and receiver closure.
- Consume typed context-overflow and repeat outcomes from the adaptive context plan instead of interpreting a generic Rig error by probing unrelated mutable state.

### 3. Give the agent driver one lifecycle authority

- Separate Rig event translation from run-state transitions.
- Route non-terminal events through one sink that handles receiver closure consistently.
- Route every exit through one completion path.
- Preserve GPU release before tool execution and cancellable reacquisition before the next model turn.
- Keep runtime lease and GPU guard owned by the driver until completion.

### 4. Adapt direct-provider streaming

- Translate `StreamToken` values into the UI-neutral run event vocabulary.
- Record finish reason as protocol state, not immediate lifecycle completion.
- Produce success only after normal stream closure/completion; map provider errors and cancellation distinctly.
- Preserve usage observation even when it follows finish reason.

### 5. Unify the GPUI pump

- Replace the two loops in `ChatPanel::start_send_with_text` with producer selection followed by one event/completion bridge.
- Translate typed producer completion to `ChatRunTerminal` once.
- Keep `ChatRunReducer` and message rendering behavior stable.
- Continue storing the GPUI task and parent token; Stop/close cancels the token and completion remains observed.

### 6. Tighten shutdown and supersession

- Verify panel drop, new-run supersession, user Stop, runtime acquisition cancellation, receiver closure, and application shutdown.
- Ensure no write tool can run after the UI consumer is gone.
- Ensure finalization clears run state and persists the session once.

## Acceptance criteria

- Agent and direct-provider producers expose an event stream plus typed completion.
- `ChatPanel` has one stream-consumption loop and one terminal translation boundary.
- Agent event sending and completion use one auditable exit path.
- Irreducible context overflow and repeat stops are deliberate typed outcomes, not transport failures; ordinary compaction is non-terminal.
- GPU/runtime resources are held and released according to the complete run lifecycle.
- `ChatRunReducer` remains the only presentation/finalization authority and admits one terminal event.
- `ALLOW-10` is removed.

## Validation

```bash
cargo test -p u-forge-agent -- --test-threads=1
cargo test -p u-forge chat_panel -- --test-threads=1
make clippy
make test-ci
```

Before remediation is marked complete, run `make test` for runtime acquisition, GPU contention, provider streaming, and tool-loop integration under the owned embedded runtime.

## Dependencies and sequencing

Consume typed context-overflow and repeat outcomes from `feature_agent-context-optimization.md`. Coordinate with `feature_inference-queue-lifecycle.md` so the run handle follows the established split between streamed items and awaitable completion instead of creating incompatible lifecycle vocabulary.

## Out of scope

- Changing chat rendering, message styling, or per-message action ownership.
- Combining Rig and direct-provider request protocols.
- Moving `ChatRunReducer` into core.
- Changing GPU contention policy.
- Using receiver drop as the primary cancellation mechanism.
