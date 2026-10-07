# First model-linked arithmetic tile

This directory implements a fixed-coefficient signed INT4 × INT8 group
projection and its independent integer oracle. It is an arithmetic and protocol
vertical slice toward the model-specific LLM ASIC, not a complete accelerator.
Coefficients and activations in the initial verification fixture are **synthetic**.
The separate [checkpoint slice compiler](REAL_WEIGHT_SLICE.md) now lowers a
bounded sample of real weights through the same tile. Its activation fixtures
remain synthetic; neither experiment measures model quality.

The initial target is the pinned
[Qwen3.8-27B configuration](../../docs/projects/llm-asic/models/Qwen3.8-27B.json),
revision `1d4bf0f2ff6012fd82039f2fa52739d0dd7c60c0`. Its text backbone has
hidden size 5120, intermediate size 17408, and 64 decoder layers. A 128-element
input group therefore partitions that hidden dimension into 40 groups.
This is a proposed lowering choice, not a quantization format specified by the
checkpoint. The generic RTL also supports smaller groups for verification.

The upstream [Qwen3.5-family implementation](https://github.com/huggingface/transformers/blob/main/src/transformers/models/qwen3_5/modeling_qwen3_5.py)
uses gate, up, and down feed-forward projections; the pinned target identifies
the same architecture family. Those matrix shapes contain 17,112,760,320
coefficients across 64 layers. That makes feed-forward projections a useful
initial specialization target. The cited moving source is an architecture
reference, not a pinned, certified inference runtime.

## Arithmetic and stream contract

`rtl/integer_group_tile.sv` consumes one signed activation per accepted rising
edge and broadcasts it to `LANES` output rows. `WEIGHTS` contains lane-major
signed four-bit coefficients: input zero occupies the lowest nibble of each
row. The module emits one packed signed INT32 partial sum per lane after
exactly `GROUP_SIZE` accepted activations.

- Inputs are two's-complement INT8 in `[-128, 127]`; weights are INT4 in `[-8, 7]`.
- Outputs are the exact integer dot products before group rescaling. With at
  most 5120 terms, even the largest absolute product sum fits INT32.
- Input advances only when `in_valid && in_ready`. An offered activation must
  remain stable while its valid handshake is stalled.
- Output remains valid and unchanged until `out_valid && out_ready`.
- A pending stalled output prevents consuming more input. Consuming an output
  can coincide with accepting the first activation of the next group.
- Synchronous active-low reset discards both an incomplete group and an
  unconsumed result. Traffic resumes after reset is released.
- Supported elaboration parameters are `1..64` lanes and `1..5120` terms.
  Simulation rejects invalid dimensions; production elaboration must validate
  them before invoking synthesis.

Each differently scaled group must be rescaled before combining its result
with other groups. Summing all integer partials and applying one arbitrary
scale would change the calculation. This tile implements neither those scales
nor SiLU, normalization, attention, recurrent state, sampling, or full-model
execution. W4A8 is an experimental arithmetic contract, not a demonstrated
quality-preserving precision for this model.

## Verification and evidence

The independent reference uses unbounded Python integers, explicit input
ranges, and no RTL or cycle-model helper. Fast contract tests live in the
repository's ordinary Python suite:

```sh
python3 -m unittest tools/tests/test_chip_integer_reference.py
```

`verify_tile.py` generates immutable synthetic fixtures, compiles the production
RTL with Verilator, compares every emitted lane against the independent
answers, synthesizes the same fixture with Yosys, and runs the synthesized
netlist against those answers again. The bench covers signed extremes, zeros,
alternating signs, deterministic random groups, stalls, output stability,
overlapping output/input handshakes, and reset with both partial and pending
state. It fails on missing or extra results and bounded-time incomplete runs.
Missing tools fail; no RTL observation is silently skipped.

```sh
python3 hardware/llm-asic/verify_tile.py \
  --out target/chip-workbench/new-tile-run --lanes 4 --group-size 128
```

Choose a new output directory for each run. Results contain stable relative
source identities and SHA-256 digests, generated-vector identities, observed
tool versions and executable digests, individual check status, and log/netlist
digests. The optional toolchain-image identity is declared by the caller;
the script does not independently inspect its enclosing container. Outputs,
compiled simulators, and logs stay under `target/` rather than in Git.
Child process groups are killed and reaped on timeout or interruption.

Checks on the initial RTL passed for `(lanes, group size)` of `(4,128)`, `(3,8)`,
and `(1,1)`. Two deliberate source mutations demonstrated fault detection:
zero-extending a negative product failed the numerical oracle, and discarding
valid output during a stall failed the protocol assertion. These observations
establish test sensitivity to those faults, not exhaustive correctness.
Netlist simulation results are separately recorded by the current runner.
Yosys's warning that accumulator memory becomes registers is expected for this
small register-based tile.

Generic cell statistics do not establish silicon area, timing closure, power,
memory-macro cost, or complete-model throughput. No foundry library, physical
implementation, or manufacturing quote is supplied by this slice.

## Isolated EDA environment

`Containerfile.eda` pins the observed Fedora 43 AMD64 base repository digest
and the observed top-level RPM versions. Transitive repository contents can
change; archive and use the built image's immutable identity for reproducible
tool execution. The initial local environment had Verilator 5.046 and Yosys
0.63. Host EDA packages are not required.

```sh
podman build -f hardware/llm-asic/Containerfile.eda -t localhost/tilo-chip-eda:local .
podman image inspect localhost/tilo-chip-eda:local --format '{{.Id}}'
```

Run the recorded image identity with networking disabled, source mounted
read-only, and only the selected output directory writable. Override the
entrypoint to `python3`, then invoke `/src/verify_tile.py`. Avoid relabelling
the shared checkout; this host's runs use `--security-opt label=disable` with
narrow source/output mounts. A newly built recipe image is not claimed to have
the same identity as the initial interactive experiment image.

## Next useful specialization step

1. Pin the inference implementation and tokenizer alongside the existing
   configuration and per-tensor checkpoint inventory. Extend the bounded
   real feed-forward slice compilation to representative calibrated groups.
2. Establish calibrated quantization and group-scale semantics with actual
   activations. Compare against the original floating-point layer, then
   evaluate complete coding trajectories before freezing precision.
3. Compile the same real coefficients into fixed-coefficient and ROM-backed
   tiles. Compare matching execution schedules and real process mappings;
   synthetic test coefficients are unsuitable for area extrapolation.
4. Implement group rescaling, gate/up fusion, SiLU, and the down projection
   with independently specified numerical contracts. Build a complete
   feed-forward layer with real shape, transfer, and numerical measurements.
5. Add the hybrid attention path. The pinned model has 48 recurrent and 16
   full-attention layers. Its specified 48 value heads with 128×128 FP32
   recurrent state imply 144 MiB across recurrent layers at batch one, before
   other state and overhead. This calculation is not a measured allocation.
   A matrix tile does not solve that state/storage problem.

Use existing Tilo Terminal actions for operator-visible runs and the shared
framework process/effect lifecycle for supervised jobs. A future `RunTile`
effect should name immutable inputs, invoke fixed tool arguments through
`bounded-command`, preserve workspace access checks, and publish completion
through the chip resource's actions. The present evidence viewer does not yet
execute or import this RTL verification schema. The existing focused native CI
workflow is a Cargo-test runner, not an unrestricted remote EDA service; any EDA
lane must use normal scheduler admission and a pinned tool image.
