# Actual quantized candidate inventory

The development candidate is `ggml-org/Qwen3.8-27B-GGUF`, revision
`71bc7b627595dc8a91039addd9c791ae548d6747`, file `Qwen3.8-27B-Q4_K_M.gguf`.
Its complete download matches the publisher's recorded 18,973,870,528 bytes and
SHA-256 `c600de0300ae8a0eb3a6c0b8b5561b8b96f16bd2c863c2a66c42de29d391a747`.
This identifies the actual quantized artifact, not its conversion lineage to
our selected BF16 revision. See the [pinned publisher card](https://huggingface.co/ggml-org/Qwen3.8-27B-GGUF/blob/71bc7b627595dc8a91039addd9c791ae548d6747/README.md).

The read-only `tools/chip/gguf_inventory.py` parses little-endian GGUF v3 using
bounded header reads, unique metadata/tensor names, finite metadata, known
storage block layouts, aligned non-overlapping ranges, and complete file-size
accounting. Unknown storage types and nested arrays fail. Tokenizer arrays
retain count/type and the digest of their exact encoded values rather than
duplicating all tokens into the report. The optional expected-digest argument
streams the entire open file and checks its actual bytes; header-only analysis
does not authenticate tensor payloads. Concurrent file changes are refused.
The format reference is the [pinned upstream constants](https://github.com/ggml-org/llama.cpp/blob/5e03bdd8700948b9c41c54dd1b00f28a2aebc03f/gguf-py/gguf/constants.py).

```sh
python3 tools/chip/gguf_inventory.py \
  --model /absolute/path/to/Qwen3.8-27B-Q4_K_M.gguf \
  --out /absolute/path/to/new-inventory.json \
  --expected-sha256 c600de0300ae8a0eb3a6c0b8b5561b8b96f16bd2c863c2a66c42de29d391a747
```

The observed file has 851 tensors. The parsed header is 10,994,112 bytes with
SHA-256 `aa35b1a4418dbdad0d6ea93d238291ec56e55d379ab524b60a292f0386f8402f`.
Its actual storage mix is:

| Storage | Tensors | Tensor elements | Payload bytes |
| --- | ---: | ---: | ---: |
| F32 | 353 | 2,645,504 | 10,582,016 |
| Q4_K | 193 | 18,384,158,720 | 10,341,089,280 |
| Q6_K | 17 | 1,774,714,880 | 1,455,820,800 |
| Q8_0 | 288 | 6,734,479,360 | 7,155,384,320 |

The dimension-derived total is 26,895,998,464 elements, matching the BF16
inventory's language-backbone-plus-output-head count. Equality of totals is
not proof of per-tensor equivalence, converter identity, or execution paths.
The `Q4_K_M` filename does not mean a uniform four-bit payload: 6.73 billion
elements here use Q8_0 storage, with its block-scale overhead.

Grouping by the recorded GGUF tensor names shows all 192 feed-forward matrices
and the embedding matrix use Q4_K. Full-attention Q/K/V projections, recurrent
QKV/gate/alpha/beta/output projections use Q8_0. The 16 full-attention output
projections and the final language head use Q6_K; norms and small recurrent
state parameters use F32. These are observed conversion choices, not evidence
that this precision mix is minimal or safe for the proposed chip. Q4_K's block
encoding also differs from our experimental exact-absmax symmetric W4 codes.

The embedded architecture is `qwen35`. Its embedded chat template SHA-256 is
`c3cf9e34abf4f9e36c2d72165aa9c132d3e2a725b6c2586aaa3a8af9d7a81041`,
exactly matching the selected BF16 revision's template. Tokenizer token/type
arrays each contain 248,320 entries; merges contain 247,587. Their encoded
digests are retained in the local inventory. Template equality is useful
compatibility evidence, not an observed reasoning/tool round trip.

The generated report and model remain local under
`target/chip-workbench/inference-candidate-71bc7b6/`; large model files are not
repository inputs. No model loading, throughput, coding score, or compatibility
with the experimental ASIC W4/A8 quantizer follows from this inventory.
