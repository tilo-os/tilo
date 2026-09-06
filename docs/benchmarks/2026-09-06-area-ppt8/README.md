# Area eight-pixel invocation experiment — design gate

Goal remains active and unmet. No commit, push, default promotion or performance
claim. Previous tail-correlation captures are complete and immutable.

## Rationale

Current Area fine assigns four horizontal pixels to each of 64 invocations per
16×16 tile. A bounded eight-pixel, 32-invocation permutation could reduce
repeated PTCL interpretation and segment fetches without a workgroup cache,
new GPU allocation, query, copy, barrier, geometry or pixel omission.
Doubling private arrays may increase register pressure/spill; benefit is unknown.
This is distinct from rejected quad-skip, row-mask, fusion and shared staging.

## Correctness design gate

A naïve change from four to eight pixels is NOT accepted. Area fill computes
segment-relative X before subtracting the per-invocation pixel lane. Changing
the invocation origin can reassociate f32 subtraction/addition for pixels 4–7.
Linear gradients similarly compute a base dot product and then a lane offset.
Preserve the original four-pixel subgroup origins and scalar operation order
inside an eight-pixel invocation. Audit all other paint and spill paths.

PTCL ordering, segment ordering, AA, blend/clip order, native coordinates,
pixel coverage, image sampling and output bytes must remain unchanged.
No tolerance relaxation and no benchmark workload, timer, warm-up or gate change.
Keep existing default, MSAA, CPU, async, replay and other opt-in permutations
unchanged. Candidate eligibility and actual selection need explicit tests.

A shared-coarse exact comparison must use the same produced geometry and
commands for both fine paths, so unrelated atomic segment-order variation
does not masquerade as a fine regression. The original independent native
pixel oracle must also still run; prior independent rounding instability is
not declared fixed by shared-coarse equivalence.

## Status

Terra's design audit confirmed the Area/linear-gradient hazards and a viable
two-subgroup implementation. Root reviewed the coordinate expressions and
approved the bounded implementation-contract.json scope. The first candidate
freeze is preserved in review-freeze-1.sha256. Precompile review then reopened
only its new test/helper code to close the issues in initial-review.md.
Production review is recorded in production-review.md. Subsequent lint-only
report/counter refactors are preserved in the versioned validation history.
Frozen v5 passes all four lint checks, 107 vendor tests, and the separately
invoked native-4K exact shared-coarse proof. Shell/UI integration validation
has passed. Ordinary release and all 30 planned comparisons are complete;
see RESULTS.md for the audited matrix, preserved failures and unmet goal.
