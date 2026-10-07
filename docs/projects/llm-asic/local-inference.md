# Local inference through Tilo

The development host has an 8,188 MiB NVIDIA GPU and about 30.6 GiB host RAM.
That excludes a nominal 27B W4 payload wholly resident in dedicated VRAM;
it does not exclude quantized CPU inference or partial GPU offload. Start with
one request and a bounded context, measure memory and latency, then decide
whether a larger device is necessary. The initial host inspection fetched no weights or runtime and executed no
candidate inference. Artifact staging and actual inference acceptance are
separate steps; source compatibility alone does not establish either.

## Existing boundary and new execution contract

Use Settings, the signed local-model catalogue and `local-model-service` to
provision the model, then the existing per-user model gateway and ordinary
Tilo agent participant to execute tasks. Agents receive launcher-issued gateway
access and scoped resource/action grants. A second inference gateway or coding
orchestrator would bypass the surface being evaluated.

The production catalogue inspected in `dist/local-models/catalog-source.json`
contains no chat model or OCI runtime. Published source examples are test
fixtures, not usable release pins. A running host installation was not observed
at the default inspected catalogue/socket paths; that is a bounded observation.

The runtime release now has an optional signed `execution` object:

```json
{"engine":"llama_cpp","max_context_tokens":8192,"gpu_layers":12}
```

or:

```json
{"engine":"vllm","max_context_tokens":8192,"cpu_offload_gib":16}
```

These examples illustrate the schema; they do not attest suitable settings
for Qwen3.8-27B. Omission retains the legacy vLLM engine and omits the field
from serialization, preserving old catalogue signing payloads. Explicit
settings are covered by the existing whole-catalogue signature. Only finite
numeric fields are supported; unknown execution fields and arbitrary argv
are rejected. Context is capped by both this limit and the model release's
signed context window. CPU-only llama.cpp refuses nonzero GPU layers.

`llama_cpp_gguf` is an explicit single-file model layout. It is eligible only
for the llama.cpp engine. This first single-file route serves text; vision
claims are refused because no separately pinned vision projection is declared.
Existing `vllm_gguf` and `hf_snapshot_tar_zst` are
eligible only for vLLM. Hardware and engine/layout must match at selection,
installation and start. The existing OCI supervisor still loads the verified
archive, uses its digest-pinned image, mounts the content-addressed model
read-only and supplies device access according to the trusted backend.

The image's immutable entrypoint must be `llama-server` for llama.cpp or vLLM
for vLLM. The supervisor builds an argv vector, fixes loopback binding and
sets `--alias` or `--served-model-name` to the signed model ID. This avoids
routing a gateway request for the catalogue ID to an engine serving only its
container filesystem path. The legacy Rust type name `VllmCommand` is retained
for compatibility; it now carries engine-specific argv from signed metadata.

The [llama.cpp server documentation](https://raw.githubusercontent.com/ggml-org/llama.cpp/master/tools/server/README.md)
documents CPU/GPU quantized inference, OpenAI-compatible chat, context/GPU-layer
settings and Jinja tool templates. Its current head is a capability reference,
not the runtime revision we will attest. The
[Qwen3.8-27B model card](https://huggingface.co/Qwen/Qwen3.8-27B)
identifies supported inference families and thinking controls. Neither source
establishes compatibility of a particular converted artifact and OCI image.

## Candidate conversion identified

The llama.cpp publisher has a
[pinned Qwen3.8-27B GGUF repository](https://huggingface.co/ggml-org/Qwen3.8-27B-GGUF/blob/71bc7b627595dc8a91039addd9c791ae548d6747/README.md).
Its source card names the base model and a DFlash2 source and says conversion
is automated; it does not supply a complete converter command/lineage receipt.
This is a concrete artifact candidate to qualify, not proof that each tensor
matches the selected BF16 checkpoint. Preserve individual artifact identity,
conversion provenance and actual load/tool outcomes before selecting it as
our evaluation or ASIC checkpoint. The metadata inspection did not fetch GGUF weights.

The subsequent candidate inventory verified the entire Q4_K_M object:
18,973,870,528 bytes, SHA-256
`c600de0300ae8a0eb3a6c0b8b5561b8b96f16bd2c863c2a66c42de29d391a747`.
It declares `general.architecture=qwen35` and 851 tensors. Its embedded chat
template's SHA-256 matches the pinned base template below exactly. Stored
logical dimensions total 26,895,998,464 elements; F32, Q4_K, Q6_K and Q8_0
storage coexist, so a nominal all-W4 byte estimate does not describe this
artifact. Equal shape totals and template bytes do not prove weight lineage.
The local immutable candidate receipts and extracted template are under
`target/chip-workbench/inference-candidate-71bc7b6/`; no model inference was
established by this inventory.

## Pinned reasoning policy to qualify

The selected base revision's
[chat template](https://huggingface.co/Qwen/Qwen3.8-27B/resolve/1d4bf0f2ff6012fd82039f2fa52739d0dd7c60c0/chat_template.jinja)
was fetched as metadata (8,952 bytes; SHA-256
`c3cf9e34abf4f9e36c2d72165aa9c132d3e2a725b6c2586aaa3a8af9d7a81041`).
Its defaults enable thinking and preserve reasoning history; it recognizes
`xhigh`, `medium` and `low` reasoning effort, defaulting to `xhigh`.
The initial comparison should explicitly retain those defaults and record
output/time budgets, with a separate bounded lower-effort experiment if needed.
That is a proposed evaluation policy, not an observed runtime setting.

`reasoning_content` must survive the provider response, streamed assembly and
assistant-message replay within the same conversation. It must not become a
user-visible final answer or be invented from content. The coding app has its
own `Conversation` parser; the participant planner uses the shared OpenAI turn
reader. The shared turn reader, gateway SSE assembly and coding-app conversation now
preserve the provider's optional string `reasoning_content` in assistant replay.
Absent/null values remain absent; other types fail as malformed provider output.
Reasoning stays separate from final answer text and visible streaming deltas.
Production-path regressions cover streamed tool calls and replay into the next
coding request; new conversations start with empty history. Generic fields
called `reasoning` are not guessed to be this provider contract.
Verify the exact pinned llama-server build emits the expected field and honors
these template controls before reporting a policy-matched comparison.

## Immutable runtime candidate source review

The inspected official CUDA image identifies llama.cpp source commit
`5e03bdd8700948b9c41c54dd1b00f28a2aebc03f` (build `b11434`), with OCI
index digest `sha256:c6fd6053d3b6a1a54849ab5468971cf3ae03fbf3d72fdfb25a1f862e320cace3`
and amd64 manifest digest
`sha256:6d4624a7cb40fd84f3b14d57c0d19ac1b6f8308c8cc833bac1461933e0bbe6a2`.
Its configuration entrypoint is `/app/llama-server`. These identities describe
an artifact candidate; an OCI source label is not a reproducible-build proof.

At that exact revision, the
[Qwen3.5 graph implementation](https://github.com/ggml-org/llama.cpp/blob/5e03bdd8700948b9c41c54dd1b00f28a2aebc03f/src/models/qwen35.cpp)
implements full attention alongside gated DeltaNet/recurrent attention.
The [chat implementation](https://github.com/ggml-org/llama.cpp/blob/5e03bdd8700948b9c41c54dd1b00f28a2aebc03f/common/chat.cpp)
recognizes the Qwen XML tool markers, reads assistant `reasoning_content`
and serializes it back into template messages. The
[Jinja capability mapping](https://github.com/ggml-org/llama.cpp/blob/5e03bdd8700948b9c41c54dd1b00f28a2aebc03f/common/jinja/caps.cpp)
maps `preserve_reasoning` to `preserve_thinking`, matching the selected
base template's control. The pinned
[server contract](https://github.com/ggml-org/llama.cpp/blob/5e03bdd8700948b9c41c54dd1b00f28a2aebc03f/tools/server/README.md)
documents separate reasoning output and template effort controls.

These source facts justify a loading experiment. They do not establish that
the converted GGUF's architecture metadata and embedded template match this
base revision, that this image executes correctly on this host, or that the
model will make a valid tool call. Record the embedded template digest and
actual health/discovery/tool outcomes before accepting this route.

## Required next evidence

1. Choose and pin a runtime source commit/build and immutable OCI digest.
   Verify that exact build supports Qwen's DeltaNet architecture, tokenizer,
   chat template, thought-history handling and tools. vLLM needs a compatible
   automatic tool parser; the present bounded execution schema does not yet
   configure one. Transport/parser reasoning replay is covered by production
   regressions; actual engine/template behavior still requires observation.
   llama.cpp's embedded template/Jinja route likewise needs
   an observed tool-call smoke test, not an inferred capability label.
2. Pin a complete model revision and quantized artifact. For a conversion,
   retain original tensor digests, converter source identity/command and
   output digest. Inventory must distinguish removed vision/MTP tensors and
   mixed precision. No arbitrary third-party GGUF filename implies provenance.
3. Stage actual artifacts through the existing catalogue release compiler.
   Verify published permanent HTTPS bytes and sign via the established release
   path. Do not add placeholder hashes, signing keys or fake production models.
4. Observe startup/health, model discovery and a read-only tool round trip
   through the gateway. Record actual runtime image/engine/argv, model and
   tokenizer/template digests, effective context, thinking/generation settings,
   host/GPU facts and peak memory. Existing lifecycle `Ready` alone is not a
   measured inference-quality result.
5. Evaluate calibrated private coding tasks with the same actions, permissions,
   prompts, budgets and grader. Preserve runtime/action trace and provenance
   required by [evaluation.md](evaluation.md). Larger hosted models may serve
   as quality references; mutable hosted aliases do not attest a local chip
   checkpoint.

There are no local model-quality scores or promises of adequate offload
throughput here. The smallest next measurement is supported artifact loading
and gateway tool use, followed by one calibrated coding task.
