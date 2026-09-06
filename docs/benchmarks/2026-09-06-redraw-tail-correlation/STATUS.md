# Tail-correlation status

Goal remains active and unmet; no default promotion, commit or push.

The preceding XRGB performance comparison is complete and independently audited:
24/24 full reports, shell numeric passes 0/18, UI control passes 1/6 (not an
attributed XRGB gain). Matched B→BX median mean 25.130306→22.165605 ms;
p99 68.981→68.852 ms; CPU output -97.56%; RSS essentially unchanged.
See ../2026-09-06-xrgb-damage-output/RESULTS.md.

Only shell/examples/redraw_bench.rs is authorised for this diagnostic iteration.
The other 32 hashed source/config/fixture inputs match XRGB v5. No shader,
vendor, UI, DRM or production algorithm change is authorised.

Implementation:
- Profile-only bounded 1,000-record join using the same captured wall sample.
- Existing CPU-observed wall phases, action/attempt/sparse facts and exact
  timestamp observation. No new GPU instrumentation.
- Coarse, fine, other and unknown durations partition total timed-stage ns;
  unknown includes ID 0 and out-of-catalog IDs.
- Existing GPU timestamp aggregates and final-submission-only recovery caveat
  are preserved. Missing/unmatched durations are None, not zero.
- Optional QOS_REDRAW_BENCH_PROFILE_OUTPUT creates a new artifact exclusively,
  writes/flushes after the loop and propagates errors. No normal-mode path
  lookup/allocation or report IO; stdout fallback remains available.
- Root requested file output to avoid tool truncation of 1,000 verbose rows.

v1: format passed; strict production compilation failed on the borrowed
profile_output_path destructuring pattern. No runtime or release/capture.
v2: pattern fixed; format passed; strict production Clippy rejected passing a
560-byte Copy action record by value. No runtime or release/capture.
Both attempts and unchanged source hashes are preserved. Terra completed the
scoped by-reference adjustment and the v3 source is frozen.

v3: all seven validation commands passed: formatting, strict production lint,
test-inclusive changed-line/compiler lint (zero diagnostics), 43 ordinary
example tests, one original native GPU oracle each in D and B, and 43 profile+B+X
example tests. Total: 88 passing test executions. Fourteen ignored executions
are the same seven pre-existing explicit diagnostics in each broad suite.
No adapter skip was observed. All 33 source hashes match the frozen version;
the other 32 inputs are identical to XRGB v5. Earlier independent default-GPU
rounding instability is not declared resolved by these passing oracle runs.

Validation is seven commands: three relevant formatting/lint checks, ordinary
shell example tests, original native GPU oracle in D and B, and profile+B+X
shell example tests. Unchanged production suites remain covered by v5 evidence.
The active predeclared capture plan is capture-plan-v2.json: BX, B, B, BX,
four diagnostic-only 1,000-action native-4K runs with unique output artifacts.
Ordinary release v3 passed in 5m 34s. The joined-record parser passed ten
synthetic checks before the predeclared captures were authorised. All four
captures are complete: 4,000/4,000 exact GPU matches, no retries, no missing
records, no truncated outputs and no replacements. Expected diagnostic exit 1
was observed in each. All source/binary hashes remained unchanged; raw outputs
and saved analyses were independently verified from disk. See RESULTS.md.

No performance acceptance run or new production optimisation occurred in this
iteration. The completed v3 evidence remains immutable. After this audit, a
separate Area-PPT8 candidate was authorised in ../2026-09-06-area-ppt8/.
Current source is reopened for that bounded Terra implementation; do not equate
future source with the frozen v3 manifests. The preserved v3 binary is untouched.

All earlier validation/benchmark cells are terminal. Do not infer live state
solely from functions store while an exec cell is running: its store changes
may not be visible to other cells until completion. Track the actual live cell.
