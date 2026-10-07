# Proposed activation quantization and group scaling contract

The current production tile emits signed INT32 group sums. This reference
specifies a **proposed**, uncalibrated bridge to floating-point linear outputs.
It does not declare a compatible Qwen quantization, implement a hardware scaler,
or establish full-layer/model execution or coding quality.

`scaled_linear_reference.py` consumes one token, at most eight output rows,
up to 5,120 BF16 inputs, and up to 40 groups. The intended group size is 128;
smaller groups exist for bounded arithmetic tests. These bounds cover the
input width of the pinned first gate projection, not every model operator.
Bias, activation functions and residual connections are excluded.

## Exact order and storage

1. Preserve the coefficient compiler's experimental symmetric `[-7,7]` W4
   codes, generated from the exact BF16 row-group absmax divided by seven.
   Do not requantize codes using rounded scales. Round each exact positive
   weight scale to FP32, nearest ties to even (RNE).
2. For each token's BF16 input group, compute the exact maximum magnitude.
   The proposed activation scale is absmax/127, with scale one for an all-zero
   group. Compute integer codes using the **exact** scale, nearest ties to
   even, clamped to `[-127,127]`. The tile's `-128` code is intentionally unused
   by this proposed symmetric quantizer. Store the activation scale as FP32 RNE.
3. Compute the signed INT32 dot product. Per output lane/group, round the
   weight-scale times activation-scale product to FP32, convert the partial
   to FP32 RNE, then multiply and round to FP32. Add contributions in increasing
   input-group order with an FP32 rounding boundary after each addition.
4. Convert the completed FP32 accumulator to BF16 RNE. No fused multiply-add,
   reassociation or double-precision accumulation is interchangeable with this
   contract. Cancellation/order tests explicitly demonstrate different results.

All inputs and operations must remain finite. Source NaN/infinity, FP32 overflow,
BF16 output overflow and positive scale underflow to FP32 zero fail; there is
no saturation to maximum finite values. Arithmetic and output conversion use
gradual underflow and preserve signed zeros where their operations require it.
A zero group's scale one is exactly representable. A scale-product underflow
may yield zero as a documented FP32 operation. Devices that flush subnormals
need a separately declared contract and comparison, not silent substitution.

Exact numerator/denominator scales and unsigned FP32/BF16 storage words are
both retained, making scale-storage rounding inspectable. JSON storage words
are integers; an eventual binary serializer must explicitly specify little
endian bytes rather than inheriting host byte order.

## Hardware route

Dynamic absmax requires seeing the whole BF16 group before generating its INT8
stream. A 128-element BF16 group buffer occupies 256 bytes; two such buffers
would allow capture and emission to overlap. This buffer/frontend is absent
from the current tile. The maximum word also supplies the activation scale.

The independently tested `activation_code_integer` formulation extracts the
BF16 significands/exponent difference. Its ratio numerator is `127*significand`
(at most 32,385, 15 bits). For relevant exponent differences the divisor is at
most 65,280 (16 bits). A difference of nine or more implies a code of zero.
Integer quotient/remainder comparisons implement exact ties-even decisions.
This avoids using an approximate floating reciprocal for code generation;
future RTL still needs a real divider or equivalent proven circuit, buffering
and protocol tests. FP32 division by 127 can separately produce the stored scale.

Use offline FP32 weight-scale constants and a shared, registered FP32 rescaler
per small tile as the first architecture candidate. Eight lanes need sixteen
scale/contribution multiplies and eight accumulator additions per 128 accepted
activations. A serialized rescaler might fit that cadence, but no multiplier
latency, clock, queue depth or stall behavior is yet measured. Keep explicit
operation boundaries rather than selecting fused arithmetic prematurely.

## Evidence and invocation

The implementation performs specified arithmetic using exact rationals and
rounds each boundary into finite IEEE-format storage bits. Tests check every
finite BF16 storage word, independent host FP32 rounding, signed zero,
subnormals, ties, overflow, activation extremes, integer frontend equivalence,
ordered multi-group goldens and actual compiler-artifact validation.

```sh
python3 -m unittest tools.tests.test_chip_scaled_linear_reference -v
python3 hardware/llm-asic/scaled_linear_reference.py \
  --sample /absolute/path/to/compiled-sample \
  --activations /absolute/path/to/bf16-storage-words.json \
  --out /absolute/path/to/new-numerical-receipt.json
```

The activation file is a JSON list of unsigned 16-bit BF16 words. The CLI reads
bounded local files only, independently validates the canonical compiler
artifacts, and consumes their exact scales and coefficients. A new receipt path
is mandatory. Validation failures produce failed receipts without replacing
prior observations. Receipts record contract/reference/validator identities,
actual input hashes, pinned sample provenance, exact and stored scales, integer
partials and FP32/BF16 outputs.

The per-row diagnostic compares the BF16 result with the exact unquantized
BF16 **sampled dot product**, including its exact error. It is not a framework
GEMM comparison, full-layer error, representative activation distribution,
quality score or verification of unsampled checkpoint payloads. The retained
local eight-row experiment uses synthetic BF16 activations and real 128-column
sampled weights; it performs no network request or additional weight download.
