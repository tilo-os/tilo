# Follow-up design constraints (read-only)

The v5 held-snapshot GPU replay establishes segment-order sensitivity for the
captured quick-settings case. It is not itself a normal-rendering repair.

The next useful feasibility question is a general, opt-in canonical-order
prepass, not another fixed49-record sort test: AA already provides a fixed-order
control for that case.

A complete in-place heapsort can sort arbitrary valid fill lengths in O(n log n)
time using O(1) temporary segment records; global scratch is not inherently
required. This corrects an overly restrictive initial suggestion that full
sorting necessarily needs additional global storage. It does not establish
that the GPU implementation would meet the latency goals.

Before implementation, verify:

- Post-path-tiling ordering: no segment writer may overlap the sort. Fine must
  run after sorting; unchanged Area equations then read only completed data.
- Per-native-tile ownership: coarse allocates fill ranges via atomicAdd on
  bump.segments and writes each range into PTCL. Verify all draws/clips,
  sparse/full dispatch routes, and overflow handling before relying on disjoint
  ranges across sort invocations. Merely checking duplicates within one tile
  does not prove lack of cross-tile overlap for malformed descriptors.
- Sort key: lexicographic exact bits of the five geometry words gives a total
  order, not numerical float ordering. Equal tuples are interchangeable only
  when auxiliary values are consistent. No coordinates, coverage equations,
  tolerance or segment multiplicity may change.
- Sixth word: it is padding for ordinary Segment but a derived row mask in the
  row-mask permutation. A general reorder must preserve the mask with its
  geometry or exclude that pairing; keeping it at a destination slot is wrong.
- PTCL walk: validate actual native tile count, command widths (including fused
  forms), END/JUMP blocks, logical buffer ranges and bounded traversal. Existing
  diagnostic hardcoded49 metadata is not a general parser/repair.
- Failure: malformed/overflow/incomplete canonicalization cannot silently count
  as deterministic success. Preserve current overflow recovery bounds and
  ordinary unsupported-path behavior.
- Costs: even with no new full-sized buffer or CPU/GPU transfer, heapsort adds
  global loads/stores, branching and a dispatch. Long fills and lane imbalance
  can worsen p99. Zero extra allocation is not zero overhead.
- Evidence: exact small GPU permutation/duplicate/padding/boundary fixtures,
  unchanged original native pixel oracles, repeatability diagnostics, and fresh
  isolated ordinary-release native1000-action measurements are all needed.
  Keep the initial experiment opt-in until results justify any default change.

No sort or default change was implemented for this design note. The older
clock-location mismatch remains a separate unresolved case; do not assume this
single quick-settings replay establishes its cause.
