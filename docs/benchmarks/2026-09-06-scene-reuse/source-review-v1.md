# Frozen v1 implementation review

Only shell/examples/redraw_bench.rs differs from the row-mask v4 source/config manifest. All production renderer and shader inputs remain identical. Terra authored the benchmark source; root reviewed the final implementation before running validation.

The exact opt-in is rejected before FrameBuilder or bridge creation unless explicit profiling is enabled. No history is allocated by default. The observer runs after samples.push on the existing successful-paint path; it borrows the last resolved upload without a second resolve, render or GPU copy. The full process remains diagnostic-only because instrumentation can perturb later frames.

One exact-byte four-entry LRU uses boxed payload snapshots. Hits promote recency and do not copy payload bytes; misses evict/drop the oldest entry before allocating. Each payload is at most 2 MiB; at most four entries and 8 MiB of payload are retained. Allocator metadata and bounded damage metadata are separate from the reported payload budget. A miss is not a globally unique scene.

Recency ranks classify hypothetical entry capacities 1/2/4, not independent cache implementations. The reported snapshot-copy total belongs only to the actual four-entry history. Empty/oversized payloads and invalid/recovery observations invalidate the history. Rejection counters give mutually exclusive primary reasons; payload checks take precedence.

Metadata change counts compare the most recent same-payload observation: ordered input rectangles, returned actual full/non-active-sparse/active-sparse route, and returned bounds-applied/area. The any-changed union avoids summing overlapping component counts. Payload-only LRU state can collapse metadata variants. Exact bounds coordinates, full configuration/layout, atlas/ramp/glyph content and pool/lifetime guards are not measured, so no counter establishes a safe coarse-cache hit.

Review checked the unchanged 1,000-action native-4K workload, exact sample gate, output-copy path, one-second map deadline and bounded retries. These checks do not resolve the earlier ordinary full-render pixel discrepancy or establish a new rendering path's correctness.
