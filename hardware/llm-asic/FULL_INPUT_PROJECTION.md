# Eight real rows across the gate projection input width

`tools/chip/compile_projection_slice.py` samples eight rows, first row zero,
across all 5,120 input columns of the pinned first Qwen3.8-27B gate projection.
Its `[17408,5120]` BF16 source is the same tensor and checkpoint revision used
by the single-group experiment. The resulting slice contains 81,920 unique
weight bytes. Index/header metadata is also transferred; payload size is not
total network traffic. No full tensor, full model or downloaded model code is
read or executed.

The compiler uses the existing immutable configuration/index/header inventory
and exact HTTP range reader, refetches and validates the shard layout, and
requests precisely the first eight complete rows in one bounded range. It
compiles forty 128-input W4 groups per output row using the unchanged exact
absmax/7 experimental quantizer. It stores the raw BF16 slice, lane-major packed
signed INT4 nibbles, exact rational scales, range/source identities and hashes.
A separate binary32 BF16 decoder independently regenerates the coefficients and
scales before numerical execution. Rehashing incorrect coefficients/scales
cannot pass this gate. Offline provenance records declared pinned source identities; it does not
authenticate sampled bytes or the complete checkpoint without the retained
network-fetch observation.

## Numerical observations

Four deterministic **synthetic** BF16 input profiles exercise zero, sparse,
uniform signed integers and different magnitudes across groups. None is a
captured model activation or a calibration corpus. The production proposed
numerical reference performs group activation quantization, integer partials,
FP32 scale multiplication and ordered accumulation, then BF16 conversion.
An independent host FP32/binary BF16 implementation must agree on every integer
partial and output storage word.

Each output is also compared with the exact mathematical unquantized BF16 dot
product over this slice. `profiles.json` records integer partials, scale bits,
FP32/BF16 output bits and exact signed errors. These differences combine weight
quantization, activation quantization, stored-scale rounding, FP32 accumulation
and BF16 output rounding. They are not a framework GEMM comparison or a quality
score. Eight rows out of 17,408 do not constitute a complete layer.

The observed real slice with the four synthetic profiles produced these
maximum absolute BF16-output differences from the exact unquantized dot:

| Synthetic profile | Maximum absolute difference |
| --- | ---: |
| Zero | 0 |
| Sparse | 0.113851 |
| Uniform signed integer | 16.134856 |
| Different group magnitudes | 34.966273 |

Units are output-value units for these particular synthetic inputs. These
numbers provide no threshold for acceptable full-model accuracy.

## Optional RTL observation

`--rtl` generates a **verification harness** containing forty copies of the
published eight-lane integer tile, one per fixed coefficient group. A scheduler
streams each group's quantized activations and emits its partials, preserving
result stalls and simultaneous output consumption/input acceptance. It is a
way to exercise existing RTL with all sampled coefficient groups, not a
proposed silicon architecture or an area/throughput result.

The production protocol bench compares 160 groups (four profiles × forty
groups) and 1,280 lane outputs with independently computed integer answers,
including input bubbles, output stalls, resets of pending/partial work and
handshake overlap. This proves integer partial execution only. Group scaling
and accumulation remain software; no floating-point RTL, mapped/physical
implementation, complete layer or model is claimed. Tool absence/failure fails
the receipt rather than skipping this optional requested observation. The
actual final run passed all 1,280 integer checks and reported 136 stalled
cycles and 159 overlapping output/input handshakes using Verilator 5.046. Its
immutable local receipt is
`target/chip-workbench/full-input-projection/runs/eight-rtl-final/projection.json`.

```sh
python3 -m unittest tools.tests.test_chip_compile_projection_slice -v
python3 tools/chip/compile_projection_slice.py \
  --snapshot docs/projects/llm-asic/models/Qwen3.8-27B.json \
  --out target/chip-workbench/new-full-input-sample

# Offline: validate and copy an existing bounded sample into a new run.
python3 tools/chip/compile_projection_slice.py \
  --existing target/chip-workbench/new-full-input-sample/sample \
  --out target/chip-workbench/new-full-input-rtl-run \
  --rtl --toolchain-image sha256:ACTUAL_RECORDED_IMAGE_ID
```

Run RTL in the isolated EDA image with network disabled, source and original
sample mounts read-only, and only the new output directory writable. A new
output directory is mandatory. `projection.json` records failures as well as
successes, actual artifact/source hashes, individual observation results and
caller-declared image identity. Original raw sample artifacts are copied before
execution and revalidated; previous receipts remain immutable. Generated
simulators belong under `target/`.

The next necessary numerical evidence is captured BF16 activation groups from
real coding/tool trajectories, with held-out prompts, followed by separate
weight-only, activation-only and rounding-error measurements. Compare the
quantized path against the actual framework's BF16/FP32 GEMM semantics and
nonlinear downstream behavior. The current mathematical-dot diagnostic cannot
substitute for that work.
