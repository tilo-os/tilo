# Geometry-input reuse feasibility — not a GPU cache

Goal remains UI p99 <= 30 ms; shell exact mean <= 25 ms and p99 <= 50 ms,
ordinary release defaults, the original 1,000 native 3840x2160 actions.
Clip-single-group comparison is complete and not a repeatable win; it stays
default-off. See ../2026-09-06-clip-single-group/RESULTS.md.

## Root-verified boundary and limitations

After flatten, path_bbox_buf and lines_buf are consumed read-only by later
binning/path stages (render.rs around 1011–1039; binning.wgsl 97;
path_count/path_tiling storage declarations). Recording currently frees them
around render.rs 1423–1432. Pool reuse preserves allocation, not content identity.
A real retained GPU cache would need explicit ownership, eviction and submission
lifetime management, and a fresh bump buffer with the exact successful lines count.

Resolver-expanded geometry inputs are four spans of the recorded scene upload:
path tags [path_tag_base,path_data_base), path data [path_data_base,draw_tag_base),
transforms [transform_base,style_base), styles [style_base,end). Bases are u32
words; bounds must be checked. Glyph expansion and open clips affect these bytes.
Draw streams and brush color need not participate in this opportunity measurement.
Keep span boundaries: concatenation equality alone is insufficient.

No new per-frame capture is needed: the existing Recording retains the exact
vello.scene Upload and vello.config UploadUniform. A read-only, doc-hidden accessor
can extract both after the existing measured sample using checked, unaligned-safe
typed config parsing. This avoids adding work or state to resolve/render.

BudgetTracker::collect currently discards the exact used counters after sizing.
settle_bump_budget(false) is not a provenance-tagged current line count; absent,
failed or pinned readback cannot be treated as proof. This experiment will NOT
report admissible GPU-cache entries or saved dispatches. All line counts/output
lifetimes remain unmeasured. A pending flag is just a conservative diagnostic
guard, not evidence of successful counter collection when false.

## Bounded implementation contract

Terra edits only vendor/vello/src/lib.rs,
shell/src/compositor/backend/drm/paint.rs, and shell/examples/redraw_bench.rs.
No rendering/shader/budget/recording algorithm, settings default or Cargo change.
No CPU flatten, GPU retained output, new GPU download/dispatch, timer/action/warm-up
change. Prior candidate evidence and source owners remain frozen outside this scope.

Expose borrowed geometry spans and a compact equality-capable descriptor derived
from the latest recorded scene/config: all Layout fields, target/tile extent,
all seven config capacities, and CPU-versus-GPU route. Exclude only base color.
Fail closed for missing/ambiguous uploads, malformed config size, overflowed,
unordered/out-of-bounds bases and empty geometry. Accessor does no allocation,
resolving, rendering or mutation. Diagnostic methods are called only from the
new profile branch. Bridge only forwards the read-only result.

Exact QOS_REDRAW_BENCH_GEOMETRY_REUSE=1 requires QOS_REDRAW_BENCH_PROFILE=1
and rejects before adapter creation otherwise. Acceptance stays disabled for the
entire diagnostic run. Observe only after the existing elapsed time was saved.
Independent four-entry exact-geometry-input LRU: <=2 MiB per snapshot, <=8 MiB
total payload, compare four borrowed spans exactly with boundaries before copying.
On an exact hit promote/update metadata without a payload copy. Report observations,
accepted/miss/hit counts and recency 1..4, copied/retained/peak CPU payload bytes,
geometry sizes and mutually exclusive primary rejection counters. Do not label
LRU misses unique geometries or rank counts independent cache simulations.
Reject/clear history for malformed/missing/empty/oversized inputs, non-GPU route,
recovery/invalid ordinary route, or a currently pending bump readback. For exact
geometry hits compare descriptor equality and separate layout, viewport, capacity
differences against the most-recent geometry match. Full/sparse mode differences
may be counted but are not geometry bytes. Descriptor equality is still just an
input opportunity, never a safe cache hit. No allocation/copy on ordinary runs.

Tests: config/span extraction incl word offsets and failures; draw/base-color
changes preserve geometry while transform/style/path changes do not; exact span
boundary identity, same-length differing data; hit no-copy/promotion, A/B/A,
four-entry/byte caps/eviction, metadata differences, all rejection paths and
history reset; exact opt-in and unprofiled rejection. CPU/readback provenance
caveat in docs/output.

## Validation and measurement

Freeze and root-review source before any Cargo/runtime. Strict production and
vendor-test Clippy, relevant changed-line test lint, format/diff, shell benchmark
unit tests and original default/bounds native-4K pixel oracle. No numerical
tolerance or benchmark gate changes. Preserve all attempted logs/version failures.
Build only affected shell example in ordinary locked release, hash/preserve
executable and source. Six fixed diagnostic runs D/B, B/D, D/B, each original
1,000 actions. B=coarse AUTO plus native bounds; no other experimental flags or
timestamps. 15-minute cap plus 30-second TERM grace, original 1-second fence and
four-attempt recovery unchanged. No replacement of incomplete attempts.
Timing is diagnostic-only, not a change to the ordinary performance matrix.
No overlapping builds/tests/runs or source edits during execution.

No GPU cache implementation is authorized by this diagnostic. Any useful hit
rate will require separate ownership/counter/parity design and approval.
Software-Vulkan llvmpipe on this ARM host does not certify physical GPU/KMS,
2010-laptop compatibility or constant 60 FPS. No commit/push until the actual
goal is achieved and final polish/verification is complete; user merges.
