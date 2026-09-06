# Next candidate audit: GPU blur-invariant cache

Read-only audit after the complete opaque-prune experiment. No source changes,
compilation, GPU rendering or performance measurements were performed for this
audited candidate.

## Finding

Sparse fine remains the largest measured steady-state stage (bounds diagnostic
12.389 ms mean / 36.468 ms p99), but this is not a measurement of blur cost.
The UI surface shadow path calls draw_blurred_rounded_rect. Current benchmark
logs do not count blur draws or emitted blur tile commands, so workload
relevance is unquantified.

In fine.wgsl, CMD_BLUR_RECT recomputes draw-invariant setup per invocation:
clamped standard deviation and reciprocal, radius/hypot terms, exponent and
inverse, delta-adjusted dimensions and erf7 scale. It then processes its four
pixels. Moving this setup to GPU draw_leaf could reduce repetition for a
blur spanning many native tiles. This is a hypothesis, not a measured win.

The current 11-word blur info layout contains flags, inverse transform and raw
width/height/radius/std-dev; those fields remain live. An exact-opt-in GPU-only
sparse-Area implementation would need dedicated cached constants (roughly
8 f32 / 32 bytes per indexed record), matching producer/consumer shader
permutations, an allocation/indexing scheme and a fine binding. Sparse info-
offset indexing can reserve more than 32 bytes per actual blur draw; cost must
be measured, not described as a tightly packed blur-only cache without proof.
The CPU encoding ABI and ordinary full/CPU/shared paths must stay unchanged.

Storage round-trips can affect contraction/rounding of hypot, pow, exp and
erf7, especially degenerate/zero/NaN/Inf cases. Exact native pixel checks are
required; no tolerance relaxation. Added buffer traffic and cache pressure
may outweigh removed arithmetic.

## Decision and next bounded step

Defer implementation until command telemetry establishes material repeated
blur work. Prefer the existing bounded test-only/shared-coarse geometry
capture and native PTCL parser: count scene blur draws, reachable per-tile
blur commands and total command tags for actual damaged native tiles in a
few representative shell scenes. Keep capture diagnostic-only, bounded and
outside acceptance timing. Existing pixel comparisons must remain intact.
A full-frame blur-command count alone cannot establish sparse-damage cost.
Do not add a production readback merely to get this telemetry.

After telemetry, rank against other fine-stage work and prototype only if the
measured distribution justifies the added storage and exactness risk.

