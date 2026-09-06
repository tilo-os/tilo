# Read-only segment row-mask audit

Status: hypothesis, not implemented or measured. Evaluate only after command
telemetry; a large fine-stage duration does not identify segment processing.

The Rust PathSegment is 24 bytes: two [f32;2] points, y_edge and a trailing
u32 padding field. The current WGSL Segment has the same24-byte stride due to
vec2 alignment; its20 bytes of fields leave the trailing word unused. A paired
GPU-only row-mask writer/fine variant could expose that word conditionally
without increasing allocation size or adding a GPU buffer/copy. It would add
metadata computation/writes/reads, so it is not free bandwidth.

After final path_tiling clipping, robustness changes and orientation swap,
encode the16 native rows that might receive any nonzero contribution. Fine
may omit a segment only where BOTH the original dy contribution and y_edge
contribution are proven zero. Keep segment ordering and all remaining Area
arithmetic unchanged. Horizontal lines can still have a y_edge contribution.

For finite final tile-local coordinates, conservatively include rows from
floor(minY)-1 through ceil(maxY); independently retain the y_edge tail when
final delta.x is nonzero. The exact1e9 sentinel contributes zero on rows0..15.
Nonfinite/out-of-range/uncertain cases must retain every row (0xffff). Signed
zero, boundary rows and producer-versus-stored floating-point rounding need
explicit proof/tests; do not use pre-robustness geometry or a narrow interval
that can produce false-negative bits. A margin is conservative, not an AA
approximation. The original fine AA equations are the oracle.

Select ONLY when actual Active native path bounds are applied and BOTH the
matching path writer (direct or indirect) and sparse-Area fine reader are
available. Otherwise retain original writer and reader in the original mode.
No scene-wide mask producer work without bounds. CPU/full/recovery/shared
paths must remain ordinary and must never read unused padding as valid masks.
Decide the quad-skip cross-product explicitly; the failed quad candidate is
not a prerequisite. Do not silently substitute one selected fine mode for
another. No default change without independent correctness and repeated
ordinary-release native1000-action acceptance evidence.

Needed tests: exact-equation row omission oracle with broad deterministic
finite inputs and boundary/signedzero/horizontal/sentinel/nonfinite cases;
layout/stride and recording selected/fallback matrices; real GPU left-edge
winding, evenodd, clips/translucency, negative/offscreen paths and partial
native edges; captured nontrivial masks plus exact dense/sparse pixels.
Existing geometry diagnostics intentionally ignore unused padding when
comparing old paths; new semantic mask validation must be explicit.

This audit is not a proof of implementation safety or a speedup claim. The
extra producer work, uniformity/divergence, driver code generation and actual
fraction of inactive rows need measurement before any promotion.

