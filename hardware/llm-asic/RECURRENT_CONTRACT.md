# Recurrent attention boundary and precision baseline

The recurrent path requires an independently specified state machine alongside
fixed-weight matrix projections. The new exact reference isolates that machine.
It implements one prepared head/token and returns a new state, so independent
contexts and speculative branches do not alias an in-place buffer.

The checkpoint and configuration remain pinned to Qwen/Qwen3.8-27B revision
`1d4bf0f2ff6012fd82039f2fa52739d0dd7c60c0` and configuration SHA-256
`137754ebd46991bec0c0d9e660dc98256be0b20f0e825b28c95c31de6142d041`.

Primary implementation references are pinned to Transformers commit
`c633787a7acb9bd11e57cd8d599be2b767bbdbaa`:

- [Recurrent fallback and model wiring](https://github.com/huggingface/transformers/blob/c633787a7acb9bd11e57cd8d599be2b767bbdbaa/src/transformers/models/qwen3_5/modeling_qwen3_5.py#L428),
  file SHA-256 `aac2a1bcca88829afc6dbdf83f97155ebf64d800ee9d703af3802a9aebcfcd18`.
- [Linear cache implementation](https://github.com/huggingface/transformers/blob/c633787a7acb9bd11e57cd8d599be2b767bbdbaa/src/transformers/cache_utils.py#L1025),
  file SHA-256 `2d2ab17aa02c9d2a993587ec58d1745b03595b6028c401cc6c976bb720ad6638`.

This pins an inspected family fallback. It does not certify every dependency,
hub kernel replacement, FLA kernel or the deployed inference runtime. Source
files are parsed and hashed, never imported or executed by the extractor.

## Executable mathematical core

For one head, state `S` has shape `[key_dim, value_dim]`. The boundary receives
prepared query `q`, key `k`, value `v`, decay `rho`, and update gate `beta`.
`recurrent_reference.py` computes exact rational arithmetic:

```
R = rho * S
prediction[j] = sum_i k[i] * R[i,j]
correction[j] = beta * (v[j] - prediction[j])
S_next[i,j] = R[i,j] + k[i] * correction[j]
output[j] = sum_i q[i] * S_next[i,j]
```

Ordering matters: prediction reads the decayed state; output reads the updated
state. The core accepts exact integers/Fractions and explicitly rejects float
inputs. It is a mathematical oracle, **not** an IEEE-754 FP32 emulator.

Upstream preparation includes QKV projection, depthwise convolution and
activation, query/key L2 normalization with epsilon `1e-6`, query division by
`sqrt(key_dim)`, exponential decay and sigmoid gating. Downstream operations
include gated RMS normalization and output projection. These operations are
outside the executable reference boundary. Their floating-point rounding,
nonlinear approximation and interaction with quantization need separate
contracts and runtime comparisons.

## State dimensions and proposed hardware consequence

The configuration has 48 recurrent layers, 16 query/key heads, 48 value heads,
and 128-element key/value heads. Query/key heads repeat three times to match
value heads. The inspected fallback casts core tensors to FP32 and maintains
state of shape `[batch,48,128,128]` per recurrent layer: **144 MiB at batch one**
across those layers. The convolution cache has `[batch,10240,4]` per layer;
assuming two-byte elements gives another **3.75 MiB**, excluding recorded-past
extensions. These are calculations from the source contract, not measured
allocations.

A first hardware state tile should operate on banked key/value dimensions and
keep prediction/correction vectors separate from the large state matrix. Its
interface needs context identity, token ordering, reset, state load/store and
backpressure. A straightforward implementation first reduces the key read,
then performs the residual update and query read. Verify those scheduling and
context semantics before combining it with projection hardware.

Memory placement is an architectural decision still to measure. Under the
explicit assumption that each layer's dense state is fetched from and written
to external memory once per token, the ideal read-plus-write traffic alone is
288 MiB/token at batch one. An implementation needing two full reads increases
that estimate. On-chip retained state changes external traffic and adds SRAM
cost. Neither estimate establishes a speed, power or silicon-area result.

## Quantization scope and verification

Experimental W4 coefficients currently cover only the demonstrated MLP sample.
They do not select precision for QKV, output projection, convolution or the
recurrent state. Preserve the inspected FP32 recurrent/normalization/decay
baseline until calibration and complete coding evaluations establish a change.
The convolution cache follows projected-input dtype; its two-byte storage
figure remains an explicit assumption. A sigmoid gate can already be rounded
in projection dtype before the fallback's FP32 cast.

```sh
python3 -m unittest discover -s tools/tests -p test_chip_recurrent_contract.py
python3 tools/chip/recurrent_contract.py \
  docs/projects/llm-asic/models/Qwen3.8-27B.json \
  --fetch --source-dir target/chip-workbench/new-pinned-recurrent-source \
  --out target/chip-workbench/new-recurrent-contract.json
```

The extractor validates immutable configuration and both primary source hashes,
records inspected symbol line ranges and dimensions, and refuses missing or
changed sources. Omit `--fetch` to reuse an already verified source directory.
Outputs are created exclusively rather than overwriting existing evidence.
Tests cover independent exact golden transitions, decay/update ordering,
residual overwrite, zero update/decay, independent branches, dimension/range
rejection and strict configuration/source identity.

No PyTorch, NumPy, accelerator device or model weights are needed. A later
runtime comparison needs a pinned PyTorch/Transformers/kernel environment and
synthetic/recorded activation tensors. Chunked prefill equivalence, FP32
bit-level behavior, recurrent RTL, full attention and complete model execution
remain unimplemented. No model-quality or reduced-state-precision claim follows
from these tests.
