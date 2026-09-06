# Next-step cache feasibility audit (read-only)

This audit recommends measurement, not a cache implementation or performance claim.
The row-mask candidate failed to improve shell mean latency over bounds alone.
General coarse-result caching is substantially riskier than reusing allocations.

Existing reuse already covers packed-upload capacity, buffer/bind-group pools,
glyph/ramp/image caches and sparse-upload capacity. Coarse work is still rebuilt.
A reusable whole coarse result depends on the resolved packed scene (including
paths/draws/transforms/styles/clip and glyph expansion), viewport/AA/base color,
shader mode, config/bump reservation, resolver/atlas/ramp contents and valid
resource lifetimes. Damage selectors and native bounds are separate constraints:
matching geometry does not prove a cached result covers a new requested region.

The next bounded experiment should first measure a necessary condition:
exact equality of the resolved packed-scene bytes over the SAME 1,000 real
native-4K shell actions, using bounded 1/2/4-entry LRU histories and full byte
equality, not collision-prone hashes. The existing diagnostic_packed_scene()
borrow exposes the previous recording's exact scene upload without GPU work.
Record payload sizes, exact hit counts/reuse distances, and whether damage,
bounds and mode differ. Do not call these safe cache hits: render configuration,
resolver/atlas/ramp state, bump changes and pool/in-flight invalidation remain
additional requirements. Low exact-payload reuse would reject whole-coarse
caching early; high reuse would justify instrumenting those remaining guards.
It would not rule out a narrower geometry-prefix cache that ignores draw colors.

Any snapshots/comparisons must be explicitly diagnostic-only and bounded in
both entry count and total bytes. Putting them after the sample timer does NOT
make subsequent samples unperturbed: allocations/cache pressure persist. Thus
disable acceptance gates for the entire diagnostic run and never mix its
timings with ordinary uninstrumented acceptance/comparison results.
Ordinary rendering still performs GPU work; the *added* telemetry is CPU-side,
not an entirely CPU-only workload. Keep the workload, pixel assertions, native
resolution, map deadlines and recovery limits unchanged.

No such telemetry was implemented or run during this row-mask comparison.
All measured source remains frozen. The unresolved native dense/full-render
discrepancy still requires causal investigation before any cache promotion.
