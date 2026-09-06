# Per-action redraw tail correlation

Goal remains active and unmet. This is diagnostic reporting, not a claimed
performance improvement or acceptance run. No commit/push/default promotion.

The diagnostic iteration is complete; see [RESULTS.md](RESULTS.md) for all four
captures, joined-row attribution and limitations. All 4,000 action rows were
preserved and cross-checked. The tested v3 source remains frozen.

## Evidence motivating this iteration

The preceding XRGB comparison completed 24/24 native-4K, 1,000-action reports.
Matched B→BX median mean improved 25.130306→22.165605 ms, but p99 changed only
68.981→68.852 ms. CPU destination writes fell 97.56%; process RSS was essentially
unchanged. Existing GPU/CPU diagnostic quantiles cannot identify whether the
same action accounts for the wall-time tail and a reported GPU-stage tail.

All v5 source/binary/log hashes and all 24 disk reports were verified before
reopening this file's source. Their evidence remains immutable in the sibling
2026-09-06-xrgb-damage-output directory. Preserved v5 executables are not replaced.

## Authorised scope

Terra may edit only shell/examples/redraw_bench.rs for profile-only joined
records and focused tests. No DRM, vendor, UI or shader changes. No runtime,
compile or format command while editing; root validates after an explicit freeze.

Retain all 1,000 diagnostic action records, not just selected top-tail records.
A record joins the same captured wall sample with existing CPU phase values,
action index/target, render/recovery counts, returned sparse scalar statistics,
and exact-matched existing GPU timestamp status with coarse/fine stage totals.
No new GPU query, GPU work, readback, renderer option, input, warm-up, timer
boundary or normal-mode allocation. Preserve aggregate reporting and timestamp
matching/accounting. Record missing, stale, pending, invalid and disabled
observations explicitly. A recovery action's CPU totals span all attempts,
but existing GPU timestamps cover only the last attempt; never equate them.

Capture/store records using the same elapsed value appended to samples. New
diagnostic reporting work is not acceptance timing. Do not claim zero overhead
for any additional scalar/timestamp inspection. Do not move existing timed work
outside the timer to make measurements look faster. Emit records after the loop.

Root will review exact boundaries, failure/missing-timestamp accounting and
bounded storage, then run strict relevant lint/tests. Only after validation and
a new ordinary release build may diagnostic captures run. Their commands and
fixed attempt ordering must be recorded before execution. Normal v5 timing
results remain the performance evidence until a production change is measured.
