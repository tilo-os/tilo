# Buffer-age-aware CPU XRGB output

Goal unchanged: 1,000 real actions, native 3840×2160, ordinary default release.
UI p99 <= 30 ms; shell exact mean <= 25 ms and p99 <= 50 ms.
Polish, scoped commit and push follow verified completion; user merges.

Completed results: [RESULTS.md](RESULTS.md). All 24 planned attempts have full
reports. Partial CPU output reduces bytes by 97.56% and the matched bounds
median mean by 11.80%, but p99 is still 68.852 ms. The goal is not met;
no default promotion, commit or push follows from this comparison.

Previous goal turn: progress. The canonical-order v4 comparison completed all
24 planned attempts, 23 with full timing summaries and one retained initial
timeout. No shell run passed both limits. C/BC passed targeted exact
repeatability checks, but BC/B added 10.516% shell mean and 7.991% UI mean.
No default was promoted. See ../2026-09-06-canonical-segment-order/RESULTS.md.

## Approved implementation scope

Terra authors shell DRM paint/scanout/direct-routing and benchmark plumbing,
plus a small shared history module if needed. No vendor/UI source edits in
this iteration. Exact opt-in QOS_DRM_XRGB_DAMAGE=1 stays off by default.
Old full-output APIs keep their complete-destination guarantee.

A bounded transaction/history core tracks one or two destination slots,
revision deltas, extent/pitch, bridge/config identity, and cursor restoration.
It produces private full/rect write plans. Only successful complete writes
advance state. Unknown history, gaps, configuration change, invalid damage,
short destinations, errors and accumulator caps fail closed to full/reset.
No pointer identity is treated as proof of untouched content.

Production uses actual alternating scanout slots; the same core may use the
benchmark's existing exclusively owned single Vec. Preserve its actions,
initial paint, scene/layout, GPU work, fences and timing boundaries. The intended
optimization is solely fewer CPU output bytes when prior destination contents
are proven. Old software cursor footprints are restored from existing staging
before normal cursor drawing. No full-frame backing image is added.

Sparse GPU readback already copies damage rectangles. This change does not
claim to eliminate those GPU copies or reduce physical bus traffic. Keep full
dirty-framebuffer flushing; KMS/scanout remains outside this benchmark.

## Correctness and verification

Require pure state-machine, abort/commit, revision/epoch/identity/layout,
single/two-target, cap, cursor and bounds tests. Actual two-target stale-buffer
and cursor parity is mandatory, including padded pitch and recovery. Test-only
full swizzle from the exact current mapped staging may provide the CPU output
reference without introducing independent GPU segment-order nondeterminism;
it must not run in ordinary performance mode. Original independent native
GPU pixel oracles and old alternating-buffer full-output tests stay unchanged.

Freeze every source owner before compilation. Preserve every version's source
manifest and failed compilation/test. Strict production lint and test-inclusive
changed-line/compiler checks must pass. Comparison commands/order below are
fixed before timing, with no discarded/replaced attempts or overlapping jobs.
Fresh unflagged/default correctness and performance are still required for
goal completion after any later promotion.

## Expected scope of benefit

A current full tight 4K CPU output writes 33,177,600 bytes per paint. Earlier
diagnostics measured about 2.0–2.2 ms mean for swizzle, versus much larger GPU
completion time. This can reduce shell bandwidth/mean time, but is not assumed
sufficient for the shell p99 or UI target. UI performs no CPU pixel readback;
its D/X pairs are regression controls, not an attributed gain.

A read-only scheduling check found cpu.max = max and zero nr_throttled /
throttled_usec at every accessible cgroup ancestor through the process scope.
No CPU quota evidence explains the tails. This snapshot does not exclude
other scheduling contention and did not change affinity, priority or driver
worker configuration.

## Predeclared comparison

18 shell runs: D, X, B, BX, BC, BCX, three rounds with rotated order.
Six UI controls: D and X, three rounds. Exact commands are in comparison-plan.json;
variants, order and interpretation are in comparison-contract.json.
All runs are exploratory and not acceptance-eligible. Preserve existing
baseline mismatches and audit any new failure before permitting timing.
