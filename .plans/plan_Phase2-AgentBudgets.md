# Phase 2 — Agent Token & Turn Budgets

**Status (2026-08-03): Open.** Retained from the audit for follow-up. Paths and symbols are authoritative; line references below describe the 2026-04-24 snapshot.

**Source findings:** M4, M5

**Why Phase 2:** Agent tool validation (H6, Phase 1) needs to land first
so we can layer cumulative-token tracking on top of validated calls. Both
items here are policy-shaped — they need decisions before code.

**Branch name suggestion:** `feat/phase2-agent-budgets`

---

## Scope

| ID | What | Where |
|----|------|-------|
| M4 | Schema injection into agent system prompt is unbounded | `crates/u-forge-agent/src/lib.rs:937-954` |
| M5 | Tool turn limit has no token / repeat-call guard | `crates/u-forge-agent/src/lib.rs:898, 1077` |

---

## Suggested approach

### M4 — schema-summary token budget

- Compute (or estimate) the token cost of `graph.schema_prompt_summary_all()`
  before injection. Use the LLM tokenizer if available; otherwise
  approximate by character count / 4.
- Define a budget (e.g. 5% of context window or a configurable cap).
- When over budget:
  1. Prefer types referenced in recent traffic (heuristic: types named
     in the last N user messages and tool calls).
  2. Fall back to alphabetical truncation if no recency signal.
  3. Append a `[…N more types omitted, ask the agent for details on a
     specific type]` marker so the LLM knows the list is partial.
- Log when truncation occurs with the original size and the budget.

### M5 — token + repeat-call circuit breakers

Add three layered guards to the `prompt_stream` loop:

1. **Cumulative input-token budget.** Track tokens across all turns; bail
   with a clear error when crossing the threshold (configurable;
   suggested default ≈ 80% of context window).
2. **Repeat-call detection.** Hash each `(tool_name, args_json)` and
   maintain a small ring buffer per loop. When the same hash appears 3
   times in a row, force a "different approach required" message back
   into the loop (or terminate with an error explaining the loop).
3. **Convergence check (optional).** If consecutive turns produce
   identical assistant content (no new tool calls, no new analysis),
   terminate.

Existing `max_tool_turns = 5` stays as the outermost safety net.

---

## Testing instructions

Canonical command:

```
cargo test --workspace -- --test-threads=1
```

Targeted tests:

- **M4 unit:** construct a schema with N (large) types; assert that the
  injected summary is below the budget and the truncation marker is
  present. Assert recency-based selection picks the right types when
  given a synthetic recency signal.
- **M5 token budget:** mock token counts so the cumulative budget is
  exceeded mid-loop; assert the loop terminates with the correct error
  type.
- **M5 repeat detection:** drive a loop where the LLM produces the same
  tool call three times; assert the breaker fires.
- **Regression:** a normal short loop must complete unchanged.

Manual verification (if Lemonade is available):

- Run a long agent session with a verbose schema; observe truncation
  fires only when expected.
- Force a known tool-loop scenario (e.g. ask the agent to look up a
  non-existent node); observe graceful exit instead of token exhaustion.

---

## Documentation fold-in

- **`ARCHITECTURE.md`** — agent section: add a paragraph describing the
  three breakers and the schema-summary budget, including the
  configurable knobs and their defaults.
- **`u-forge.toml`** / config docs — document the new `agent.token_budget`,
  `agent.max_repeat_tool_calls`, `agent.schema_summary_token_cap`.
- **`.rulesdir/`** — add to the agent-rules file: "tool loops are bounded
  by both turn count and cumulative tokens; do not bypass these guards
  in new tools."
- **`bugfinding.md`** — leave alone.

---

## User input prompts

Pause and ask before:

1. **Defaults for budgets.** Token-budget default, schema-summary cap,
   and repeat-call threshold are all policy. Propose values, get sign-off.
2. **Repeat-detection action.** When the breaker fires, do we (a)
   terminate the loop with an error, (b) inject a system message asking
   the LLM to try a different approach, or (c) both — error after one
   inject attempt? Audit suggests (c).
3. **Tokenizer choice.** Per-message accuracy is best with a real
   tokenizer; estimate is fine if no tokenizer is available locally.
   Confirm.

---

## Commit & push

When tests pass:

1. Two commits:
   - `feat(agent): bound schema-summary injection to a token budget (M4)`
   - `feat(agent): cumulative token + repeat-call circuit breakers (M5)`
2. Push and open a PR.

---

## Out of scope

- Streaming-mode token accounting (already partially handled).
- Memory / compaction features.
- Anything that changes the agent's tool surface.
