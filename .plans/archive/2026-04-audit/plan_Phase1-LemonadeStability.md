# Phase 1 — Lemonade Inference Stability

**Status (2026-08-03): Open.** Retained from the audit for follow-up. Paths and symbols are authoritative; line references below describe the 2026-04-24 snapshot.

**Source findings:** C3, H8

**Why this is its own branch:** Both findings live in
`crates/u-forge-core/src/lemonade/` (with one neighbour in `queue/workers.rs`
for C3). Both shape the user-visible behaviour of the inference layer
during failure modes. Coupling them gives one cohesive PR around "Lemonade
edges and timeouts".

**Branch name suggestion:** `fix/phase1-lemonade-stability`

---

## Scope

| ID | What | Where |
|----|------|-------|
| C3 | LLM call holds GPU guard for full reqwest 30s timeout, blocking STT | `crates/u-forge-core/src/lemonade/chat.rs:237`; client at `crates/u-forge-core/src/lemonade/client.rs:40-45`; worker at `crates/u-forge-core/src/queue/workers.rs:65-88` |
| H8 | Catalog discovery uses `tokio::try_join!` — one bad endpoint blocks all init | `crates/u-forge-core/src/lemonade/catalog.rs:131-139` |

---

## Suggested approach

### C3 — bound the LLM call with `tokio::time::timeout`

- Wrap `provider.complete(job.request).await` in a configurable
  `tokio::time::timeout` (default 5s for non-streaming completions).
- On timeout: drop the GPU guard *first*, then propagate the error so STT
  can proceed. The drop ordering matters — verify with the surrounding
  code in `chat.rs` and `workers.rs`.
- Streaming completions already have their own bounds; do not wrap them
  in a second timeout.
- Surface the timeout as a concrete error variant (`LemonadeError::Timeout`
  or similar) so the UI can show "Lemonade Server is unresponsive" rather
  than a generic "request failed".
- Make the timeout duration configurable via `AppConfig` so users on slow
  hardware can extend it.

### H8 — switch catalog discovery to `tokio::join!` with per-endpoint outcomes

- Replace `tokio::try_join!(/models, /system-info, /health)` with
  `tokio::join!`. Capture each result individually.
- Build `LemonadeServerCatalog` from the successes; track failures on
  the catalog struct (e.g. `endpoint_failures: Vec<EndpointFailure>` with
  endpoint name + error).
- Update callers to surface per-endpoint failures in the UI's "connect"
  flow, so "models is fine but health is hanging" is a discoverable
  diagnostic.
- Define a "minimum viable catalog" — e.g. `/models` is required, others
  are optional. If the required endpoint fails, return the error;
  otherwise return a partially-populated catalog with failure annotations.

---

## Testing instructions

Canonical command:

```
cargo test --workspace -- --test-threads=1
```

Targeted tests:

- **C3 unit:** mock the provider so `complete` sleeps longer than the
  configured timeout; assert the timeout fires, the GPU guard is
  released, and a subsequent STT request can acquire the guard. The GPU
  guard interaction is tested via `crates/u-forge-core/src/lemonade/`
  test fixtures if present.
- **C3 integration:** if you have access to a misbehaving Lemonade Server
  fixture (or can construct one with `wiremock` / a TCP echo that hangs),
  drive a real `provider.complete` call through the worker and assert
  graceful timeout. If not feasible, document the gap.
- **H8 unit:** mock the HTTP client so each of the three endpoints can
  fail independently; assert the catalog is built from the surviving
  endpoints and the failures are recorded on the struct.
- **H8 — required-endpoint behaviour:** assert that failing the
  designated "required" endpoint causes discovery to fail.

Manual verification (if Lemonade Server is available locally):

- Start the app with Lemonade running, confirm normal operation.
- Stop Lemonade mid-LLM-call and observe that STT becomes available
  again within the configured timeout.
- Restart Lemonade with one endpoint disabled (e.g. block `/health` via
  a proxy) and observe the partial-catalog UI message.

If you cannot reproduce manually, say so explicitly.

---

## Documentation fold-in

- **`ARCHITECTURE.md`** — Hardware section talks about the asymmetric GPU
  policy. Update with the new timeout contract: "the LLM call holds the
  GPU guard for at most `lemonade.llm_timeout_ms` (default 5000ms) before
  releasing it on timeout."
- **`ARCHITECTURE.md`** — Lemonade catalog section should describe the
  new partial-success behaviour and per-endpoint failure annotations.
- **`u-forge.toml`** / config docs — document the new
  `lemonade.llm_timeout_ms` (or whatever it is named) and any required-
  endpoint setting.
- **`bugfinding.md`** — leave alone.

---

## User input prompts

Pause and ask before:

1. **C3 timeout default.** Audit suggests 5s. Confirm the value, or ask
   for an alternative. If the user runs heavy local models with high
   latency, 5s may be too aggressive.
2. **C3 config surface.** Ask where the timeout should live: a top-level
   `lemonade.llm_timeout_ms` key, or scoped under
   `inference.timeouts.llm`?
3. **H8 required-endpoint policy.** Ask which endpoint is the required
   minimum (audit hints at `/models`, but this is a product call). The
   answer changes whether `/health` failures are warnings or errors.

---

## Commit & push

When tests pass:

1. Two commits is fine if the diffs are independent:
   - `fix(lemonade): timeout LLM completions and release GPU guard early (C3)`
   - `fix(lemonade): partial-catalog discovery with per-endpoint failures (H8)`
2. Each commit body should reference the bugfinding ID it closes.
3. Push the branch and open a PR. Wait for review.

---

## Out of scope

- Replacing reqwest or restructuring the HTTP client.
- Cancellable jobs (that is F7, Phase 3).
- Provider abstraction beyond Lemonade (F6, Phase 3).
- Worker EWMA tuning (H5 lives in Phase 2's tuning plan).
