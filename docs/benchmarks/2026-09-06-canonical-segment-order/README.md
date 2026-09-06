# Canonical segment-order feasibility

Completed v4 evidence: [current matrix and interpretation](RESULTS.md).
The candidate passes its targeted correctness checks, but adds mean time and
RSS compared with bounds alone. The goal remains unmet; the default pixel
failure and all failed/incomplete timing attempts remain preserved.

The following is the original pre-execution plan, retained for attribution.

Goal unchanged: ordinary default release build,1,000 real actions at native
3840x2160. UI p99 <=30ms; shell exact mean <=25ms and p99 <=50ms.
Final polish, scoped commit/push only after verified completion; user merges.

Previous turn was progress: the held-snapshot GPU replay established that
changing only one49-segment fill order reproduces and reverses both captured
quick-settings byte changes. Final131 tests and10 validation commands passed.
All25 source/config/fixture hashes match at this turn's start. No jobs remain
live; no files are staged. See ../2026-09-06-segment-order-replay/README.md.

The next candidate is GENERAL in-place canonical ordering, not another
fixed49-tuple diagnostic. Exact opt-in VELLO_CANONICAL_SEGMENT_ORDER=1 remains
off by default. In-place heapsort gives O(n log n) work and constant temporary
record storage, but may add costly GPU reads, writes and tail latency. No
speed or memory improvement is assumed.

The geometry key is an exact lexicographic comparison of five meaningful u32
words. All six words move together, because word six is a derived mask in the
row-mask permutation. Coordinates, multiplicity, winding, blending and Area
coverage equations remain unchanged. Full synchronous GPU Area renders need
all native tiles; sparse renders need their actual fine damage tiles. Reuse
existing damage records/uploads and resource lifetimes; no extra full-sized
CPU/GPU copies or scratch allocation may be hidden in the implementation.

Terra first provides a concrete integration/failure-lifetime design for root
review, before source edits. The parser must bound native tiles, command
blocks, actual segment ranges and traversal; malformed or incomplete work
cannot silently be claimed successful. Unsupported CPU/async/nonArea paths
and existing ordinary shared-coarse/order-replay diagnostics remain explicit
fallbacks. Selection must be reported, not inferred from the environment.

Root will freeze all source before validation and runtime. Required evidence
includes exact CPU/GPU sorting and multiset/padding tests, parser/failure and
sparse-outside tests, recording lifetimes, original native pixel oracles,
repeatability, strict relevant lint, and isolated ordinary-release full
1,000-action comparisons. No tolerance relaxation, workload simplification,
discarded/replaced timing attempts or compiler/benchmark overlap.

Performance comparison and executable commands will be fixed before any
timing: default, bounds, canonical, bounds+canonical on both workloads. The
older clock-location mismatch remains a separate unresolved case. Exploratory
flagged results do not establish unflagged/default acceptance or certify a
2010 laptop, physical GPU transfer counts, KMS presentation or constant60FPS.

Pre-execution attribution clarification: the ordinary default does not defer
the bump readback. A selected canonical submission must move that existing
readback after sort/fine to observe a parser failure. C/D and BC/B therefore
measure the complete canonical candidate, including changed readback ordering,
not the isolated cost of sorting. The24 timing commands/order and all gates
remain unchanged. There was no runtime or performance result when this
pre-execution clarification was recorded; completed results are linked above.
