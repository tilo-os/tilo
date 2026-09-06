# UI tail profile: audited results

Four of four predeclared captures completed all 1,000 native-4K redraws, with 4,000/4,000 matching GPU and dispatch timestamp samples, no missing or stale submissions, no full/recovery attempts, and 40 preserved joined slow-render records. All four raw disk logs exactly match captured output; all 33 source hashes and the preserved executable hash remained unchanged. All ten parser fixtures passed. No Rust/WGSL or default changes, and no acceptance claim.

These are instrumented diagnostics, not comparable acceptance timings. Host render profiling and per-dispatch GPU queries change cost. Do not interpret their higher wall times as an application regression or combine these numbers with the ordinary benchmark matrix. Animated damage/geometry can also vary with real elapsed time across runs.

|Capture|Total mean ms|Total p99 ms|Submit mean ms|Settle mean ms|Last compute mean ms|Other compute mean ms|
|--|--:|--:|--:|--:|--:|--:|
|default 1|18.361|81.925|0.614|17.625|1.258|14.182|
|ppt8 1|18.285|67.247|0.574|17.587|1.096|14.265|
|ppt8 2|18.245|82.087|0.565|17.557|1.145|14.131|
|default 2|18.564|78.583|0.553|17.882|1.267|14.189|

Host draw-to-scene means are 66–67 µs, versus 18.1–18.5 ms render means. All measured renders use sparse fine, with zero dirty-copy encoding and CPU readback bytes; no scratch target was created. This points away from widget traversal, caller texture copying and scratch allocation as the dominant cost in these diagnostic captures. It does not measure all allocation churn or driver-internal copies.

The last/fine pass averages about 1.1–1.3 ms, while the preceding compute-pass interval averages 14.1–14.3 ms. There are ten recorded dispatch stages per sample: flatten, clip_leaf, binning, tile_alloc, backdrop_dyn, coarse, fine_sparse_area, draw_leaf_prefix, path_count_direct and path_tiling_direct. Flatten is the largest individual aggregate stage mean (2.89–3.08 ms), followed by draw_leaf_prefix (1.74–1.93 ms). Several other stages average roughly 1 ms. These measured device intervals can include driver scheduling/preemption, especially on llvmpipe; they are not exclusive CPU execution or compiler times. Sums of stage quantiles do not explain a frame p99.

Of the 40 joined slow-render records, 39 have host settle greater than submit and 38 have other-compute greater than last-compute. 40/40 also appear in their capture's total-time top ten. Exceptions are retained, including a 68.063 ms host-submit interval and large fine-pass intervals. The complete top-ten rows do not replace the 1,000-frame distributions, and no exact stage-specific p99 cause is claimed because the current report lacks per-stage arrays for each tail frame.

All known-GPU endpoint reports are 62,282,124 bytes, with CPU readback 0. This is known-resource accounting, not peak VRAM, private shader scratch or driver memory. PPT8 shows 1,000 actual sparse selections per enabled capture and zero for controls. Its faster fine pass cannot remove the larger preceding-compute cost by itself.

Next priority is a read-only feasibility audit of existing CPU prefix stages/residency, looking for fewer pre-fine GPU dispatches without GPU-to-CPU readbacks, redundant uploads or changed geometry. No new candidate is authorized merely by this profile; CPU/GPU numerical and ordering parity need explicit review and testing.
