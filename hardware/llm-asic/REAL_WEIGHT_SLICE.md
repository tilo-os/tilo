# Bounded checkpoint coefficient compilation

`tools/chip/compile_weight_tile.py` compiles four rows and the first 128
columns of `model.language_model.layers.0.mlp.gate_proj.weight`, shape
`[17408, 5120]`, from Qwen/Qwen3.8-27B revision
`1d4bf0f2ff6012fd82039f2fa52739d0dd7c60c0`. This is 1,024 bytes of real BF16
weight storage. It downloads checkpoint metadata and exact HTTP ranges;
it does not download a full tensor or execute model code.

```sh
python3 tools/chip/compile_weight_tile.py \
  docs/projects/llm-asic/models/Qwen3.8-27B.json \
  --out target/chip-workbench/new-real-weight-tile
python3 -m unittest discover -s tools/tests -p test_chip_compile_weight_tile.py
```

The compiler first uses the checkpoint inventory validator to check the pinned
source, index/header agreement, tensor storage spans, and complete shard
layout. It refetches the selected shard header and checks its hash and total
size before locating each row's payload. Every payload request must return
HTTP 206 with exactly matching Content-Range and complete bounded body. Range
and sampled-byte hashes identify the fetched bytes. They do **not** establish
the integrity of unsampled checkpoint bytes or authenticate the publisher.

BF16 is decoded directly from little-endian storage to exact rational values,
including subnormals and signed zero. Non-finite values are rejected. Each
sampled row group uses a symmetric experimental signed INT4 quantizer:

- Scale is `max(abs(weights)) / 7`; an all-zero group uses scale one.
- Integer coefficients are nearest integers to `weight / scale`, ties to even,
  clamped to `[-7, 7]`. The RTL's available `-8` code is unused.
- Scales and decoded source values are recorded as exact numerator/denominator
  pairs. There is no hidden floating-point rounding in coefficient generation.

`sample.bf16` preserves the fetched row ranges concatenated in row order.
`sample.json` records source identity, offsets, configuration/index/header
hashes, byte hashes, compiler/reference/RTL identities, scales and limitations.
`fixed_linear_fixture.sv` holds the generated real coefficients.
`tb.sv` and `vectors.json` use **synthetic INT8 activations** and independent
integer dot-product answers. The existing production tile stays unchanged.

With the isolated EDA image, run the same compiler's offline verification mode:

```sh
python3 tools/chip/compile_weight_tile.py --verify-existing \
  --out target/chip-workbench/new-real-weight-tile \
  --run-out target/chip-workbench/new-real-weight-verification \
  --toolchain-image sha256:YOUR_RECORDED_IMAGE_ID
```

Every verification requires a new `--run-out` directory; an existing directory
is refused, preserving prior observations. The receipt starts before input
validation and records failed or cancelled invocations too. Tools consume copied,
hash-validated inputs in that run rather than mutable source paths. A previous
receipt never describes the new invocation; use its explicitly named run directory.

Mount the compiler, hardware and compiled sample read-only and only the run
output directory writable, disable container networking, and follow the narrow mount
instructions in README.md. The verifier checks artifact identities and independently
decodes the raw BF16 sample, regenerates its quantization, coefficient wrapper,
activation vectors and oracle testbench, then runs the
production RTL against the independent answers, synthesizes it with Yosys, and
runs the synthesized netlist against those answers. Missing tools fail.
The new run's `verification.json` records individual results, tool versions and executable
hashes, artifact-validator identity, log/netlist hashes, and the caller-declared
image identity. These
outputs and compiled simulators belong under `target/`, not in Git.

This checks generation and exact integer execution for a real weight sample.
The tile still emits integer sums **before** weight/activation rescaling.
Sampled-group scales do not establish a complete calibrated quantization.
Synthetic activations establish neither floating-point layer error nor coding
quality. Freezing silicon still requires real activation calibration, complete
operator integration, coding trajectory evaluations and physical design.
