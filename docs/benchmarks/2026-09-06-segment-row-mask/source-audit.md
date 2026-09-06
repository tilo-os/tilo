# Row-mask cost and exactness review

This is a source-level audit, not timing evidence.

The ordinary fine shader already has a dy != 0 branch around its expensive
analytic edge-area integration. A clear-row mask therefore does NOT remove
that expensive integration a second time. Its incremental opportunity is:
skip the full segment load when the backend honors scalar field loads, skip
the original y/delta/clamp/dy work, and skip the y_edge evaluation and four
zero additions. The mask test/branch and producer computation/store are costs.
The earlier approximately 60% potential-clear-row figure cannot be interpreted
as a 60% reduction in fine-stage time or total frame time.

The writer's current design computes bounded integer interval masks after
clipping, robustness edits and orientation swap. A [0,16] finite ordered
comparison rejects NaNs/infinities/unexpected coordinates. The exact f32 1e9
edge sentinel is known to clamp to zero on rows 0..15. Other uncertain fields
retain 0xffff. Conservative intervals cover endpoint-adjacent rows and the
edge suffix; no estimated AA value or narrower geometry is substituted.

The proposed interval model was independently checked with explicit float32
rounding at original operation boundaries over 213,924 synthetic/boundary
segment cases and 702,024 clear-bit checks, with no false negatives. This
was not the Rust implementation, not shader execution, not a representative
timing workload and not GPU proof. The Rust implementation and GPU producer
still require their separate tests and captures.

Important details for final review:
- The independent dy oracle must use (p0.y - row) + (p1.y - p0.y), not p1.y-row.
- Finite endpoints alone do not prove every AA intermediate finite: very small
  delta_y can overflow its reciprocal. Kept rows must use unchanged equations.
- Signed-zero/subnormal and y_edge terms cannot be ignored because a line is
  horizontal or because its dy term is zero.
- Read the scalar mask before the full Segment expression so skipped rows can
  avoid unnecessary loads, without claiming what a given driver actually does.
- The 24-byte stride is unchanged, but previously unused padding is meaningful
  only with the matching producer; CPU/full/fallback must never consume it.
- Bounded Active selection is not synonymous with AUTO provenance. Forced
  Active and AUTO Active need the same valid native-bounds eligibility.
- Indirect producer recording must be tested as DispatchIndirect, including
  its setup/bindings and same-mode fallback, not only as a predicate result.
- Both same-geometry fine readers and bounded geometry downloads must finish
  before the shared source resources are freed. Separate blend spill/output
  state avoids a first pass contaminating the second.

The frozen v4 vendor suite passes all 89 tests. The new same-geometry GPU
fixture runs direct and indirect writers, each capturing 16,777,216 bytes of
backing buffers and checking 60 reachable clear bits. Both ordinary sparse
and row-mask sparse readers match dense pixels exactly on the 289-pixel ROI;
all pixels outside it retain their zero sentinel. These are small-fixture
correctness results, not representative performance or skipped-work estimates.

The backing buffer capacity is rounded and need not be divisible by the
24-byte segment stride. The parser validates each reachable 24-byte record,
not an assumed alignment of the entire allocation. The earlier compile and
harness failures are retained in validation-summary-v1/v2/v3.json and logs.

Native application oracles and lint remain in progress. No feature promotion
or latency success follows from this audit.
