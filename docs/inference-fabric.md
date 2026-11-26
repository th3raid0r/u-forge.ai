# Live inference fabric baseline (#108)

This experiment measures the current application paths before a scheduler
cutover. It does **not** change placement, retry policy, batch size, or defaults.
The timed mocks in `benches/inference_lifecycle.rs` remain useful for queue
mechanics, not for speed, memory bandwidth, or energy claims.

## Run on an idle machine

1. Build `u-forge` to provision the [pinned runtime](../BUILD.md#build-and-run).
   Use the provisioned `lemond` (and its adjacent resources) and stage the exact
   **cached** checkpoints and installed backend builds in a private benchmark
   cache. Do not use a desktop or external Lemonade server as the fixture. Do
   not run another inference workload during the experiment.
2. Copy [`benchmarks/inference-fabric.toml`](../benchmarks/inference-fabric.toml)
   and adapt the topology, sensor paths, models and immutable artifact pins.
   Verify each `artifact` against the staged file hash or immutable snapshot;
   these are operator attestations, while the runner checks runtime version and
   catalog checkpoint/backend pins and confirms effective options in `/health`.
   The example CPU/GPU IDs are two registrations of **one GGUF checkpoint**.
   In the isolated cache's `user_models.json`, register both IDs using the same
   `checkpoint`, `recipe = llamacpp` and `labels = ["embeddings", "custom"]`.
   The IDs, unlike the weights, must be distinct to be resident together. For
   example, given an existing registration under the CPU ID:

   ```sh
   cache_home="${XDG_CACHE_HOME:-$HOME/.cache}"
   src="$cache_home/u-forge/lemonade"
   run_dir=$(mktemp -d "$cache_home/u-forge-fabric.XXXXXX")
   cache="$run_dir/cache/u-forge/lemonade"
   mkdir -p "$cache" "$run_dir/data" "$run_dir/runtime"
   touch "$run_dir/.u-forge-inference-fabric"
   # Stage the runtime and resources only; do not bring an adjacent models/ directory.
   cp -a "$PWD/target/debug/lemonade/lemond" "$PWD/target/debug/lemonade/resources" "$run_dir/runtime/"
   # Use copies or copy-on-write clones, not symlinks to mutable app state.
   cp -a --reflink=auto "$src/bin" "$cache/"
   mkdir -p "$cache/models"
   for model in models--ggml-org--embeddinggemma-300M-GGUF \
                models--unsloth--gemma-4-E4B-it-GGUF \
                models--pqnet--bge-reranker-v2-m3-Q8_0-GGUF; do
     cp -a --reflink=auto "$src/models/$model" "$cache/models/"
   done
   jq '. + {"fabric/embeddinggemma-igpu": .["ggml-org/embeddinggemma-300M-GGUF"]}' \
     "$src/user_models.json" > "$cache/user_models.json"
   ```

3. Pin AC/battery, OS power mode, CPU affinity/governor, background activity,
   fan policy and thermal starting band. The harness waits for that band before
   cold and warm phases and checks AC and (if a probe is configured) power mode.
   It does not change the machine's settings. Keep the packaged backend defaults
   and per-model recipe files fixed; the TOML pins llama.cpp context, threads,
   batch/microbatch and engine parallel slots. FLM thread scheduling is engine
   owned. Verify explicit GPU device identifiers against the pinned backend's
   `--list-devices` and inventory the memory domains and actual offload; a
   backend name, device string or client RPC overlap is not proof of independent
   fabrics. A discrete GPU that uses host RAM for a material part of inference
   is not an independent memory domain for that workload.
4. Run **at least five** repetitions per configuration. Preserve stdout JSONL,
   this exact config, the source revision (`git rev-parse HEAD`, clean tree),
   runtime/backend/driver versions, and meter/sensor provenance. Keep stderr
   local; it can include runtime diagnostics. The runner intentionally changes
   residency/capacity only inside its owned disposable runtime/cache.

   ```sh
   XDG_CACHE_HOME="$run_dir/cache" XDG_DATA_HOME="$run_dir/data" \
     UFORGE_LEMOND_PATH="$run_dir/runtime/lemond" \
     make inference-fabric FABRIC_CONFIG="$PWD/benchmarks/inference-fabric.toml" \
     > "$run_dir/results.jsonl" 2> "$run_dir/diagnostic.log"
   ```

   For energy, timestamp and sample a **whole-system** cumulative joule counter
   (e.g. from a wall meter), publishing a JSON reading atomically to
   `whole_system_joules_path`: `{"unix_ms": 1795669200000, "epoch": 1, "joules": 4123.5}`.
   Supply `meter_max_age_ms` and sample often enough to satisfy that freshness
   bound. Use the local clock for the acquisition timestamp; advance `epoch`
   after any meter restart, reset or rollover. The warm sweep reports the
   approximate counter difference without idle subtraction, only if both
   readings are fresh, newer at end, in the same epoch, and the sweep spans
   at least ten freshness windows. Otherwise the energy is `null`, not zero.
   GPU PPT, RAPL package energy and instantaneous watts are not whole-system energy.

## Comparison matrix

| Case | What to compare |
| --- | --- |
| `single` | Each configured CPU, GPU and supported NPU separately. A different package/precision/space (such as GGUF vs FLM) is a standalone datapoint, **not** a matched comparison. |
| `weighted` | One production `InferenceQueue` with the selected eligible resident providers, configured weights, and one graph. This is today's weighted backfill path. |
| `overlap` | Same corpus, partitioned across one actual `embed_all_chunks` sweep per target. Each does its own SQLite transaction; unlike weighted, all targets have forced work. Use returned batch counts and request overlap to reject cases that didn't exercise both providers. |
| One batched request | Repeat the matrix with `chunks = 32`. A single target has one 32-input call with engine-owned parallelism; partitioned overlap splits those same 32 inputs. Weighted may use just one provider on this case; do not interpret it as simultaneous. |
| Discrete | Configure CPU + dGPU, or GPU0 + GPU1, using verified explicit device binding and separate model IDs for the same package. Run the single and simultaneous rows for each pair. Repeat on a unified host rather than extrapolating. |

For backfill, `cold_load_ms` and `dimension_probe_ms` are separate from warm
first-batch latency and warm wall throughput. Cold means not resident in the
runtime, not a dropped OS page cache. `warm_provider_window_chunks_per_second`
uses first provider batch start through last batch completion; it does not
mistake a burst of near-simultaneous completions for throughput. The warm wall window is the real sweep, through the lane-wide SQLite commit(s), not
just provider CPU time. Interactive probes use the same queue(s) as indexing;
queue wait is submission to provider start, not request completion. They run
closed-loop with one query outstanding, drained before the next case. Tiny
sweeps can have no probes. Batch shape is checked at the provider boundary;
a backend may internally serialize a batch or the provider may fall back to
single requests on malformed responses. Do not infer engine-level concurrency
from HTTP overlap alone. Compare batches/fallbacks using backend diagnostics
if an engine-level claim depends on them.

## Calibrate the user-search interruption budget

The optional search section indexes a fixed 64-chunk graph with the selected
embedding route and keeps chat, embed, and rerank resident. For each repetition
it measures chat alone paired with chat + one search, and chat alone paired
with chat + repeated searches. It uses the real direct chat/profile lease and
hybrid search stages. Search starts after a fixed number of delivered deltas;
repeated searches wait for more delivered deltas between requests. Repeat with
different `query_repetitions` and `candidates_per_lane` to bound admissible
inputs on that fabric. Keep the prompt/seed, model, output cap, and shape
identical within a pair. Recheck warm and cold eligibility independently; cold
loads/first results are reported separately, not counted as cheap warm work.

- Delivery timestamps are client-observed SSE **deltas**, which can contain
  multiple model tokens or arrive coalesced. Inter-delta p95/p99 and model usage
  tokens/s (stream start through last delta, including first-delta time) both
  matter. Do not equate an SSE event with an exhaustively sampled server token.
- Match nonempty delivered deltas by ordinal and content/framing (compared in
  memory, never printed), with the same completion-token usage. Reject
  divergent streams and workloads extending beyond the last observed delta as
  calibration evidence. For each operation, sum positive treatment-minus-control
  gap differences intersecting that provider's time window. Faster gaps do not
  erase stalls; a boundary gap shared by two operations counts conservatively
  against both. Full-search and whole-run totals count each intersecting gap
  once in their respective search window unions. Budget verdicts are null if the
  pair is not admissible; raw timings remain available. This is paired observational
  evidence, not proof that each individual gap was caused by the search.
- `search_wall_ms` measures completing the full hybrid pipeline, not added
  stream interruption. `agent_tool_wait_ms` is null: this workload does **not**
  run a synchronous Rig tool. Such a tool's wait should be separately measured,
  and never counted as interruption against the same already-paused stream.
- Require FTS and both inference stages to apply, without retry, with the exact
  requested rerank candidate count, while the stream is active.
  Inspect per-operation and end-to-end violations of **25 ms / 50 ms** and the
  worst observed stall. Do not widen the product targets to admit a workload.
- Verify all models resident before/after warm runs with unchanged backend
  listener socket identities and no chat profile reload. The Linux socket inode
  detects a worker replacement even on the same port. The fixture directly
  records app-initiated profile reloads, but boundary snapshots do not count
  every lifecycle event. Corroborate
  uninterrupted residency with backend lifecycle logs/traces when using the
  result to authorize overlap. Missing identity is not a residency pass.

## Evidence and rollout gate

The stdout schema is versioned in its `manifest` record. `backfill`,
`chat_search_cold`, and `chat_search_pair` records are independent observations
keyed by repetition and scenario. Skipped/failed records carry a reason; empty
percentiles and unmatched interruption samples are null. `admissible_sample`
means the fixture checks passed, **not** that a latency target passed. A valid
weighted row can select a single route; `provider_request_overlap_pairs` lists
which route-index pairs actually had overlapping requests for either mode. Raw
metrics comprise timings, counts, labels, configuration, and optional joules;
no corpus, query, responses, embeddings, credentials or endpoint URLs are
emitted. Preserve all repetitions, including excluded ones. For example, to
see warm-throughput spread across accepted repetitions (add p50/p95 and sample
counts for latency and energy when comparing revisions):

```sh
jq -s '[.[] | select(.kind == "backfill" and .sample.admissible_sample)]
  | group_by([.mode, .routes])
  | map({mode: .[0].mode, routes: .[0].routes,
         chunks_per_second: (map(.sample.chunks_per_second) | sort)})' results.jsonl
```

Before changing defaults, record the accepted baseline and **agreed numerical
regression budgets** in #107 for backfill throughput/wall time, interactive
p95 wait, reloads and whole-system J/chunk wherever metered. Preserve the fixed
25/50 ms limits. Rerun the matched matrix at cutover, compare repetition spread
and worst observed interruption, and report all gaps. There is no agreed
budget encoded by this baseline harness. Unverified hardware, placement, model
space, search shape, cold overlap and energy claims stay gated; none is a
reason to block the safe single-pool path.
