# Model selection evidence

Evidence reviewed on **October 6, 2026**. Product horizon: October 2028.
This is a primary-source survey with analytical calculations. **No common
model evaluation, quantization evaluation or hardware measurement has been run
for this project.** Published scores are attributed claims, not our results.

## Initial choice and alternatives

Use Qwen3.8-27B as the initial compiler and hardware exploration target while
evaluating more capable alternatives. Do not freeze production weights until
quality, precision, implementation and manufacturing evidence support it.

| Model | Architecture and release terms | Published coding evidence | Exploration role |
|---|---|---|---|
| Qwen3.8-27B | Dense 27B language backbone; Apache-2.0; 262K native context | Terminal-Bench 2.1 73.0; SWE-bench Pro 61.7; DeepSWE 1.1 42.2 | Initial dense target |
| Qwen3.8-Flash-Next | 125B backbone/6B active, plus 51B n-gram embeddings and 4B MTP; Qwen Community License | Pro 62.5; DeepSWE 58.7; NL2Repo 48.1 | Stronger quality/conditional-compute candidate |
| DeepSeek-V4.1-Flash | 552B backbone plus 196B Engram memory; 8B active prefill/16B decode; MIT | Terminal-Bench 2.1 90.6; DeepSWE 74.2 | Quality reference and ambitious complete implementation |
| Mistral Medium 3.5 | Dense 128B; 256K context; Modified MIT | SWE-bench Verified 77.6 | Available Mistral coding reference |
| Mistral Large 4 preview | 1.05T total/49B active; 1M context; weights promised later in October | DeepSWE 61.7; Terminal-Bench 4 28.3 | Watchlist/API quality reference pending released weights |

Sources: [Qwen27B card](https://huggingface.co/Qwen/Qwen3.8-27B),
[Qwen Flash-Next card](https://huggingface.co/Qwen/Qwen3.8-Flash-Next),
[DeepSeek card](https://huggingface.co/deepseek-ai/DeepSeek-V4.1-Flash),
[Mistral Medium card](https://huggingface.co/mistralai/Mistral-Medium-3.5-128B),
[Mistral Large announcement](https://mistral.ai/news/mistral-large-4/) and
[Large specifications](https://docs.mistral.ai/models/mistral-large-4-0).

Qwen27B combines 48 Gated DeltaNet layers and 16 gated full-attention layers.
Dense execution reduces routing variation; its persistent recurrent state still
requires dedicated implementation and verification. Flash-Next replaces full
attention with sparse micro-block attention and adds conditional embedding
lookups. These are distinct operator/memory requirements, not interchangeable
matrix engines.

DeepSeek uses a causal encoder-decoder, compressed sparse attention, conditional
memory and speculative generation. Its card reports 890 bytes/token of global
KV with FP4 and sharing, not the complete working set. The headline active
parameter count does not describe total embedded weights or lookup storage.
Sparse accesses require physical routing and access-latency analysis.

Medium 3.5 explicitly replaces Devstral 2 in Mistral Vibe. Large 4 was announced
on the review date; a forthcoming checkpoint is not a downloadable artifact.
Its release license and detailed physical implementation inputs remain unknown.

Other useful comparison points are
[Qwen3-Coder-Next](https://huggingface.co/Qwen/Qwen3-Coder-Next) (80B total/3B
active, Apache-2.0, hybrid attention and non-thinking) and
[Mistral Small 4](https://huggingface.co/mistralai/Mistral-Small-4-119B-2603)
(119B total/6.5B active, Apache-2.0, official NVFP4 and EAGLE checkpoints).
Their low active counts are not small storage requirements. The newer lead
models remain the main evaluation set.

## Benchmark comparability and license boundaries

Qwen's Pro results use corrected tasks, Claude Code and a 256K window; its
DeepSWE27B result uses Claude Code. Flash-Next reports the best DeepSWE score
across Claude Code and mini-SWE-agent. These are important conditions, not a
shared independent leaderboard.

DeepSeek's instruct scores use maximum effort and a 1M window. Its own scaffold
comparison gives DeepSWE 65.6 with Codex versus 74.2 with mini-SWE. Mistral's
Verified score concerns a different benchmark. Large4's announcement attributes
its coding index results to Artificial Analysis. Do not sort all these values
into one quality ranking or assume they survive aggressive quantization.

Flash-Next's [license, section 2](https://huggingface.co/Qwen/Qwen3.8-Flash-Next/blob/main/LICENSE)
requires a separate license for commercial Model-as-Service or AI Work
Assistant businesses, and explicitly includes coding assistants. Commercial
chip/product implementation terms need review before commitment. Medium3.5
uses Modified MIT with revenue-related exceptions; verify its
[actual license](https://huggingface.co/mistralai/Mistral-Medium-3.5-128B/blob/main/LICENSE).
A captured Hub license label is not a complete legal review or proof of ASIC
implementation rights.

## Pinned configurations and analytical storage bounds

The [metadata records](models/) pin configuration revisions and canonical
configuration digests. [The project README](README.md#reproducible-inputs)
documents the profiling CLI and the limits of those records. The full license,
weights and serving artifacts must be separately pinned for actual execution.

The initial Qwen27B record pins revision
`1d4bf0f2ff6012fd82039f2fa52739d0dd7c60c0`:
[immutable config](https://huggingface.co/Qwen/Qwen3.8-27B/resolve/1d4bf0f2ff6012fd82039f2fa52739d0dd7c60c0/config.json).
Its captured complete tensor inventory is 27,781,427,952 parameters; this
differs from the nominal 27B language-model count. Neither is a verified
text-only per-tensor inventory.

| Derived quantity | Calculation/assumption | Result |
|---|---|---|
| Nominal four-bit weight payload | 27B parameters × 4/8 | 13.5 GB, decimal |
| Full-attention BF16 KV per token per sequence | 16 layers × 2 K/V × 4 KV heads × 256 dimensions × 2 bytes | 65,536 bytes |
| Full-attention KV at 32,768 tokens | Above × context | 2 GiB |
| Full-attention KV at 262,144 tokens | Above × context | 16 GiB |
| FP32 recurrent matrices per sequence | 48 layers × 48 value heads × 128 × 128 × 4 bytes | 144 MiB |
| Untied embedding and output matrices | 2 × 248,320 vocabulary × 5,120 hidden dimensions | 2,542,796,800 parameters |

These are configuration-derived estimates. Recurrent matrix shape follows the
[reference implementation](https://github.com/huggingface/transformers/blob/main/src/transformers/models/qwen3_5/modeling_qwen3_5.py);
it must be pinned and checked against actual runtime traces before becoming an
execution contract. The config specifies FP32 SSM state. Lower weight precision
does not establish safe lower recurrent-state precision. Convolution state,
quantization scales, alignment, temporary activations, MTP and vision work are
excluded from the nominal calculations. The current CLI's cache calculation
includes conventional full-attention KV only, not recurrence.

For illustration only, uniform four-bit logical payloads of 180B and 748B
parameters would be 90 GB and 374 GB respectively. These sums describe the
listed Flash-Next and DeepSeek components, not exact checkpoint storage, proven
precision choices or silicon area. Fixed-weight logic can encode computation
differently from SRAM; neither payload bytes nor active parameter count predicts
die area. Physical estimates need a process, implementation, interconnect,
state, interfaces and actual synthesis/placement results.

## Common Tilo coding evaluation contract

All candidates must use the same versioned task pack and explicit resource/tool
contracts before selection. Initially include repository issue repair,
multi-file features, test diagnosis, branch-conflict integration, long-context
code retrieval and RTL/verification changes. Use held-out Tilo/Rust tasks with
production-path checks, alongside pinned public coding benchmarks. Separate
generation, issue repair, integration and hardware-verification outcomes.

Every run must record:

- Immutable checkpoint, weight digest, tokenizer/template, precision and runtime.
- Harness revision, Tilo action schema, permissions, environment/tool images,
  task base revision, hidden acceptance tests and network policy.
- Context budget, compaction/cache policy, reasoning setting, sampling and seed,
  wall-clock/token limits and number of attempts.
- Task outcome, tests, invalid tool calls, regressions, retries, prefill/decode
  usage, elapsed time and observable resource consumption.
- Artifact/source provenance and evidence class: analytical, simulated,
  physically implemented, FPGA measured or silicon measured.

Tools execute through Tilo's permissioned resource/action boundary. Agent
identity and attribution remain visible. Missing measurements are unavailable,
not zero; an acceptance check unable to observe its subject fails. Provider API
aliases that can change do not constitute immutable local-checkpoint evidence.

Select on successful coding work per time, energy and cost, including retries
and tool execution. Compare full precision against each proposed numerical
variant using paired tasks and repeated runs. Perplexity and isolated operator
accuracy are diagnostic evidence, not sufficient acceptance criteria. Quality
floors and allowable loss must be established from these results rather than
invented before a baseline exists.

## October 2028 usefulness and upgrade decisions

No evidence establishes that a current checkpoint will be frontier-quality in
two years. The product case must remain useful through economics, privacy,
latency, concurrency and demonstrable coding outcomes while quality evolves.

1. Freeze the evaluation contract early; refresh competing models and bind the
   fabrication checkpoint late. Retain old checkpoint results for comparison.
2. Keep a model/operator compiler reusable across generations. Make retargeting
   cost and unsupported operators visible in the workbench.
3. Compare fixed-weight silicon with architecture-specialized replaceable
   weights, programmable corrections/adapters and replaceable model chiplets.
   They are alternatives to evaluate, not promises that frozen weights become
   upgradeable without cost or quality loss.
4. Track complete-agent latency and quality at multiple context sizes and
   concurrency levels. Cached/tool-heavy agents can have different economics
   from short standalone completion benchmarks.
5. Establish updated model quality, independent physical review and financing
   before complete-chip fabrication. An FPGA demo or representative test tile
   retires particular risks; it does not establish full-model production.

The immediate research output is a measured common comparison plus verified
tensor/operator inventories. Until those exist, Qwen27B is an engineering lead,
not a concluded optimal model or a fabrication commitment.
