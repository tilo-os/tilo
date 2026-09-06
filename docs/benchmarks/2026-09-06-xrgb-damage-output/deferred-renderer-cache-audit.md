# Deferred renderer-cache audit

Read-only Terra audit while the XRGB output candidate is implemented. Neither
renderer-cache candidate is implemented or enabled in this iteration.

## Workgroup segment staging

Ordinary Area fine has 64 invocations per 16×16 native tile. Each walks the
same segment sequence. A 64-record workgroup array could substitute one
cooperative storage load per record, followed by the unchanged per-invocation
AA equations in the original accumulation order. It adds about 1,536 bytes of
workgroup storage and two barriers per batch, with no external GPU buffer or
upload. Quad-skip's divergent early return is incompatible with those barriers.

The source and existing command-distribution evidence do not establish a win:
many fills contain only a few segments, and the software driver may already
reuse uniform/cached loads. Added barriers can outweigh the storage-load
reduction. Do not implement on an assumed 64× speedup.

## Geometry-prefix reuse

The first useful cache point is after flatten: retain lines and path bounding
boxes and skip path-tag reduction/scan, bbox clear and flatten. Fresh resolver
and draw/clip/binning/tile/coarse/tiling/fine work remain required.

A safe hit requires exact resolved layout/structure, path tags, path data,
transforms, styles and draw tags, plus compatible viewport/capacity. Changing
draw-data bytes may still be compatible after those checks. A fingerprint must
be followed by exact equality; raw unexpanded Encoding is insufficient.

Flatten also sets bump.lines. Reuse must restore the exact cached line count
into an otherwise freshly zeroed bump allocation. Existing adaptive bump
readback might supply it without any additional GPU readback; pending/pinned/
no-measurement paths cannot create such an entry. Retained lines/path boxes
need explicit eviction and lifetime accounting; CPU exact-comparison snapshots
add bounded memory and copy costs.

Earlier whole-packed-scene LRU telemetry found zero hits, but it included
changing draw parameters. It does not prove zero geometry-only hits. Conversely
there is no measured geometry-only hit rate yet. Before a production cache,
measure exact resolved geometry-prefix hits on the actual 1,000-action stream,
including structural/viewport/capacity rejection and available line-count
measurement. Preserve ordinary timing separately from telemetry.

## Decision

Implement only the proven full CPU output-write opportunity now. Keep these
renderer ideas deferred until their reuse potential/cost is measured. The
remaining GPU-completion bottleneck is acknowledged, not declared solved by
the CPU copy work.

## Buffer-pool reuse and existing diagnostics

The reported UI pool size and adaptive policy budget measure different things:
the pool also holds scene/configuration/reduction/download allocations. Pool
bytes above policy bytes are not by themselves a budget breach or a leak.
Stable endpoint bytes and minor page faults cannot establish allocation churn.

ResourcePool keys buffers by rounded size class, usage and label. Free buffers
return to that pool after submission. An effective adaptive-budget change calls
`engine.clear_pool()`, which clears free GPU buffers, cached bind groups and
CPU preprocessing pools. This can interrupt otherwise useful reuse, but its
frequency on the action stream has not been measured. No pool changes are
authorized or implemented in this iteration.

The existing `VELLO_BUMP_LOG=1` reports measured counters and a ` (grew)` marker.
In synchronous `collect()`, that marker means raw adaptive reservation growth;
the returned boolean instead compares effective budgets after viewport floors.
Growth hidden below a floor can therefore print the marker without a pool
clear. Zero markers excludes adaptive-growth clears in that path; N markers
is an upper bound, not an exact count. The separate asynchronous `observe_now()`
currently returns raw growth, so it must not be conflated with `collect()`.
The printed policy is pre-observation, and neither log is a general pool-clear
or allocation-hit/miss trace. `QOS_REDRAW_BENCH_PROFILE=1` does not enable it.

Any diagnostic using these logs must run alone, retain its original output,
and remain separate from ordinary performance comparisons. If more evidence
is needed, bounded request/hit/miss/return/clear-reason counters should precede
selective retention changes. No runtime diagnostic has run for this audit.

### UI prefix-trim follow-up: repeated-every-frame hypothesis rejected

Root also reviewed the high-water trim in ui/src/offscreen.rs. The suspected
automatic clear on every small sparse frame was not confirmed: UI supplies
the dirty prefix width/height as renderer parameters, and returned
`stats.coarse_pixels` describes that same prefix, not always the full target.
After a full-to-small trim, the high-water becomes the small prefix area, so
successive small-prefix frames do not repeatedly satisfy the trim condition.

Alternating large/far-edge prefixes and small prefixes can still cause repeated
trims. AUTO active-bin selection does not alter this prefix policy. Existing
aggregate benchmark logs do not contain the per-frame prefix/trim data needed
to establish frequency. Do not change the already-correct coarse_pixels field
or label the policy a correctness bug. A CPU-only reason/prior-high-water/
current-prefix counter would be the next diagnostic; retaining large sparse
prefix classes would be a measured memory-versus-reuse tradeoff, not a free win.
