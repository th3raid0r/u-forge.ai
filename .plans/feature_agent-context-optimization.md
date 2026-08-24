# Feature: Adaptive Agent Context Optimization

## Status: Planned — impact rank 3, direction reset after rollback

- **Primary candidate:** `AGENT-02`
- **Bundled supporting candidates:** `AGENT-03`, `AGENT-05`, `AGENT-10`
- **Acceptance outcome:** remove `ALLOW-05` by deleting or replacing the argument-heavy budget controller boundary.

## Product direction

The initial agent-budget implementation used conservative static controls across schema, history, tool data, and model calls. Those controls disrupted ordinary tool use and were partially rolled back. The remaining code must not be treated as a design to preserve or merely reorganize.

The replacement treats model context as one adaptive working set:

- admit useful context without independent per-source quotas;
- measure total context pressure against the selected model's live effective context;
- compact intelligently when the request approaches approximately 90% occupancy;
- preserve the current task, recent conversation, and structurally complete tool transactions;
- allow tools to be verbose when the information is useful;
- once the TypeScript sandbox exists, transform large raw tool results before the agent reads them rather than forcing every tool to return prematurely abbreviated output.

The goal is graceful context management, not a larger collection of token limits.

## Why this remains impact rank 3

`crates/u-forge-agent/src/budget.rs` is still a large cross-cutting change surface, but the remediation target is now deletion and replacement rather than decomposition. It currently mixes remnants of static admission policy, schema selection, history fitting, diagnostics, repeat detection, and hook termination. Removing obsolete controls and introducing one pressure/compaction authority simplifies every agent model turn and unblocks the intended sandbox data flow.

## Current authority and affected code

- `crates/u-forge-agent/src/budget.rs` — transitional budget, fitting, diagnostics, and repeat machinery to audit and substantially reduce.
- `crates/u-forge-agent/src/agent.rs` — request assembly and hook/controller construction.
- `crates/u-forge-agent/src/stream.rs` — interpretation of context and repeat termination.
- `crates/u-forge-agent/src/tools/` — current model-visible tool result boundaries.
- `crates/u-forge-core/src/config.rs` — effective model context and any obsolete budget configuration.
- `crates/u-forge-core/src/lemonade/selector.rs` and selected-model reconciliation — live model context ceiling.
- `.plans/feature_TS-Agent-Sandbox.md` — future raw-result transformation boundary.
- `.plans/feature_chat-run-lifecycle.md` — typed run outcomes for unrecoverable context overflow and repeat protection.

## Context model

### Hard ceiling

The selected model's reconciled context window remains a real provider constraint. Request construction must account for the intended response reserve so the provider is not asked to exceed that ceiling. This is the only ordinary hard token boundary.

Sandbox CPU, memory, input/output bytes, and execution limits are separate host-safety controls. They are not prompt-budget policy and must not be weakened by this work.

### Pressure trigger

Use an initial compaction trigger at 90% of the effective context envelope, including the response reserve. Below that threshold, context should pass through without source-specific trimming. The threshold is one evidence-tunable policy constant, not a set of user-facing limits for schema, history, tool arguments, tool results, or assistant output.

### Protected working set

Compaction must preserve:

- stable system/tool instructions required for the current call;
- the current user request;
- the newest relevant turns;
- incomplete assistant tool calls and their matching tool results;
- unresolved user decisions, commitments, identifiers, mutation outcomes, and errors needed to continue the task;
- schema records actively required by the current operation, kept as complete records while the existing schema-summary contract remains in use.

### Compactable archive

The first compaction candidates are the oldest completed conversation and tool-transaction groups. A group is compacted as a coherent unit, not by independently dropping assistant calls, tool results, or arbitrary strings.

Compacted summaries must retain provenance and explicitly state that detail was compacted. String slicing and silent omission are not acceptable compaction strategies.

### Raw versus model-visible tool data

Verbose raw tool output and model-visible context are different resources. The host may retain a structured raw result outside the prompt while exposing a transformed result to the model.

Before the TypeScript sandbox lands, pressure-time fallback may use tool-aware structured projection or transaction compaction when a result cannot fit. It must be explicit about omitted detail and must not impose a fixed output quota during normal operation.

After the sandbox lands, the preferred flow is:

1. a tool returns structured raw data;
2. raw data remains outside model-visible context;
3. a sandboxed TypeScript transform receives that data as explicit input;
4. the transform filters, groups, joins, or reshapes it for the active task;
5. only the transformed artifact and concise provenance enter the agent transcript.

The sandbox's byte and execution bounds protect the host. They do not justify forcing all tools to be terse.

## Required invariants

- No independent hard token ceilings for schema records, retained history, tool arguments, tool output, or assistant output.
- No cumulative token budget across otherwise valid model turns.
- Context estimation is performed on the complete model request envelope, not on isolated sources whose totals can drift from the actual request.
- Compaction begins only under measured pressure, initially at 90% occupancy.
- Every model dispatch remains below the selected model's hard context ceiling with response reserve included.
- Current and incomplete tool transactions remain structurally valid.
- Old completed transactions compact before recent active context.
- Compaction preserves identifiers, committed mutations, unresolved actions, errors, and user decisions needed for continued work.
- Tool results are not rejected merely because they are verbose.
- Schema JSON and individual schema records are never byte-sliced.
- Repeat-loop protection remains a separate safety concern based on validated canonical arguments and observed progress; it is not a token budget.
- Expected compaction is not presented as an agent failure. Only an inability to produce a valid request below the hard ceiling becomes a typed context-overflow outcome.
- Diagnostics remain aggregate and content-free: estimated occupancy before/after, compaction count, and fallback use are sufficient.
- No mutex guard is held across `.await`.

## Target request flow

For each model call:

1. Assemble the complete desired request from instructions, tools, schema context, transcript, current prompt, and response reserve.
2. Estimate total occupancy against the selected model's reconciled context.
3. If occupancy is below 90%, dispatch the request unchanged.
4. If occupancy reaches the threshold, identify the oldest compactable completed groups while protecting the active working set.
5. Compact enough groups to restore useful headroom, then re-estimate the complete request.
6. Dispatch when the request fits below the hard ceiling.
7. If adaptive compaction cannot make a structurally valid request fit, return a typed context-overflow outcome with an actionable explanation instead of silently deleting active context.

Compaction should create headroom rather than shaving exactly to the provider limit and immediately compacting again on the next turn.

## Implementation stages

### 1. Audit rollback residue and define the deletion set

- Trace which `EffectiveAgentBudget`, `BudgetController`, schema-fitting, diagnostics, and termination fields still affect runtime behavior after the rollback.
- Add characterization tests for ordinary tool use that previously failed under conservative defaults.
- Classify each current mechanism as provider-safety, repeat-safety, obsolete static policy, or diagnostics-only.
- Delete obsolete per-source admission and configuration rather than wrapping it in new abstractions.
- Keep current `.rulesdir` and architecture descriptions unchanged until code and tests establish the replacement behavior; update them in the same implementation change that switches the runtime contract.

### 2. Introduce complete-request pressure measurement

- Derive the hard ceiling from the selected model's reconciled context and response reserve.
- Estimate the actual serialized request envelope once per dispatch.
- Record aggregate occupancy and tokenizer fallback without maintaining separate enforcement counters for every source.
- Use a single initial 90% trigger constant with tests at below, exact, and above-threshold boundaries.

### 3. Represent transcript groups and protection state

- Identify coherent user/assistant turns and assistant-call/tool-result transactions.
- Mark groups as active/protected or completed/compactable.
- Preserve matching tool-call history for current tool results and eliminate orphaned transaction fragments.
- Keep the newest relevant context available without a fixed message-count or token quota.

### 4. Implement adaptive compaction

- Compact oldest completed groups first.
- Produce a structured summary that retains task state, identifiers, decisions, mutation outcomes, errors, unresolved actions, and provenance.
- Re-estimate after each compaction pass and stop when useful headroom is restored.
- Do not compact on every turn or repeatedly summarize already compacted material without demonstrated need.
- Define a deterministic fallback for compaction failure; do not silently truncate.

### 5. Simplify the Rig integration

- Replace `BudgetController` with a small context-pressure/compaction hook or request builder.
- Remove `ALLOW-05` by deleting the old constructor or replacing it with one cohesive request context.
- Separate repeat protection from context optimization so either can evolve without sharing mutable policy state.
- Emit a typed context-overflow outcome only after compaction cannot satisfy the provider ceiling.

### 6. Prepare verbose tool-result transformation

- Define a transport-neutral distinction between raw tool data and model-visible transformed data.
- Keep existing tools functional before the sandbox; do not block this plan on V8 integration.
- Add the future sandbox adapter contract: explicit structured input, bounded execution, transformed output, provenance, cancellation, and typed failure.
- Avoid designing tool-specific static token limits as a temporary substitute.

### 7. Remove obsolete configuration and update contracts

- Remove unused static budget settings, diagnostics fields, UI controls, and tests only after repository-wide call-site verification.
- Migrate persisted configuration gracefully if obsolete fields have shipped.
- Update `ARCHITECTURE.md`, `.rulesdir/rust-patterns.mdc`, and public configuration documentation to describe measured pressure and adaptive compaction after the implementation lands.

## Acceptance criteria

- Requests below the pressure threshold are not reduced by schema/history/tool-output quotas.
- Ordinary tool calls that failed under conservative static defaults complete without special configuration.
- Compaction starts from measured complete-request occupancy at approximately 90%.
- Old completed transaction groups compact before recent or incomplete work.
- Every dispatched request fits the selected model's hard ceiling including response reserve.
- Tool results may remain verbose; pressure is handled by adaptive compaction or transformation rather than normal-path rejection.
- The old budget controller and obsolete per-source configuration are deleted or reduced to independently justified safety mechanisms.
- Repeat protection remains progress-aware and independent of context pressure.
- Compaction diagnostics expose no prompt or tool-result content.
- A future sandbox transform can consume raw structured tool data before the agent reads it without changing the context manager's core model.

## Validation

```bash
cargo test -p u-forge-agent -- --test-threads=1
make clippy
make test-ci
```

Required focused scenarios:

- below-threshold requests pass through unchanged;
- exact and above-90% requests compact once and regain headroom;
- current tool-call/result pairs remain intact;
- oldest completed transactions compact first;
- verbose current tool output remains usable;
- compaction preserves IDs, mutation results, unresolved actions, and user decisions;
- tokenizer fallback remains conservative without reintroducing per-source rejection;
- irreducible requests return typed context overflow;
- repeat-loop protection still stops unchanged no-progress calls;
- sandbox-transformed and native tool-result adapters produce the same model-visible transcript contract.

Before remediation is marked complete, run `make test` to verify model-context reconciliation and the complete owned-runtime agent loop.

## Dependencies and sequencing

This plan can begin immediately with rollback-residue characterization and deletion. Coordinate its typed context-overflow and repeat outcomes with `feature_chat-run-lifecycle.md`.

The sandbox transformation adapter is a forward-compatible boundary, not a blocker. Its runtime implementation remains gated by `.plans/feature_TS-Agent-Sandbox.md`.

## Out of scope

- Preserving the current static budget architecture for compatibility with its internal APIs.
- Adding new per-source token quotas under different names.
- Silently truncating tool results, schema JSON, or transaction messages.
- Weakening sandbox resource/security limits.
- Requiring tools to be terse solely to simplify prompt accounting.
- Implementing the TypeScript runtime before its design gate is approved.
