# Resolved-scene reuse feasibility diagnostic

The goal is unchanged: ordinary release defaults, 1,000 real native-4K actions,
UI p99 <= 30 ms; shell exact mean <= 25 ms and p99 <= 50 ms. Only after genuine
verification: final polish, scoped commit/push; user merges.

The previous turn was progress, not a blocker: all row-mask correctness/lint
checks passed, and 18/18 native-4K 1,000-action comparisons completed. Row masking
was not promoted: versus bounds alone shell mean worsened 2.50%, median peak RSS
rose 2.33%, and p99 improved just 3.27% while missing the absolute gate.
See ../2026-09-06-segment-row-mask/README.md. Current source/config hashes matched
that frozen v4 input at this turn's start; no compiler/test/benchmark was live.
HEAD remains 1acdb59f8ca126e4e3a7e936e78c11b7a6182929; nothing staged or pushed.

## Question and limits

Could immutable whole-coarse outputs be reused across these actual actions?
Before implementing a GPU cache, count a necessary condition: exact equality
of the resolved packed scene uploaded to Vello. This is not raw source Scene
equality, not a hash, and not sufficient to authorize reuse.

A full cache also needs matching viewport/AA/base color/shader modes/bump
config, coherent atlas/ramp/glyph resources, covered damage bounds, lifetime
and pool/overflow invalidation. These guards are not inferred from a byte hit.
Nor does a miss rule out a narrower path/geometry-prefix cache excluding draws.

## Instrumentation contract

Terra edits only shell/examples/redraw_bench.rs. All production renderer code
and shaders remain frozen. Exact opt-in QOS_REDRAW_BENCH_SCENE_REUSE=1 requires
QOS_REDRAW_BENCH_PROFILE=1; reject unprofiled use before renderer creation.
The entire run is diagnostic with acceptance gates disabled, even though the
snapshot/equality work happens after collecting each ordinary sample duration:
its allocations/cache pressure can perturb subsequent samples.

Use existing diagnostic_packed_scene() after each successful ordinary paint.
No extra GPU work, download, resolving, layout, rendering or altered action.
One exact-equality LRU with four bounded snapshots reports payload hits for
hypothetical capacities 1/2/4 by recency rank. Per payload <= 2 MiB, retained
payload bytes <= 8 MiB. Bound damage metadata too. Report observation/admission/
miss counts, LRU ranks and reuse gaps, empty/oversize/recovery/metadata rejection,
retained/peak CPU payload bytes and copied snapshot bytes. Do not call a bounded
LRU miss count a global unique count. Avoid a new payload copy on a hit.
Invalidate conservatively for invalid/recovery observations.

Hit-associated differences describe ordered input damage, coarse/full mode and
returned bounds-applied/area metadata only. Matching area does not establish
matching bounds coordinates; none of these counters establishes cache safety.

## Predeclared execution

Validate parsing/gate rejection, exact equality/recency/eviction, bytes and entry
caps, metadata differences and invalidation; shell tests, native default/bounds
pixel checks, relevant strict/test lint, format/diff/source integrity.
No source edits or competing compiler/test/runtime jobs during execution.
Preserve failures under versioned logs.

Build only the affected redraw_bench executable with ordinary locked release
options, preserve/hash it and source/config. Run six instrumented processes:
D/B, B/D, D/B. D is renderer defaults; B adds adaptive coarse and native bounds.
Every command is in diagnostic-plan.json; 1,000 real actions at 3840x2160.
No row mask, quad skip, opaque prune, fusion or GPU timestamps. Keep the
one-second map deadline, four render attempts and 15-minute process cap with
30-second TERM grace. Preserve every attempted run, without replacement.

Diagnostic exit 1 with 1,000 samples and DisabledDiagnostic is an expected
completed diagnostic, not a latency acceptance pass. A runtime failure,
missing observation or timeout is incomplete and cannot be replaced.
Record exact adapter/native extent, selected modes and observed reuse counts.
Do not use instrumented mean/p99 to update the ordinary-performance matrix.

High byte reuse would justify measuring the remaining cache-invalidation
guards; low reuse would deprioritize whole-coarse caching for this workload.
There is no predetermined performance claim or success threshold from counts.
The earlier unresolved dense/full-render discrepancy still needs causal
investigation before promoting a new rendering path. This software-Vulkan ARM
host does not certify a 2010 laptop, physical GPU copies or constant 60 FPS.

## Results (frozen v1)

All seven validation commands passed: 33 benchmark unit tests, one native-4K
default pixel check, one native-4K bounds pixel check, strict production Clippy,
test-inclusive changed-line/compiler Clippy (zero diagnostics), explicit
unprofiled-probe rejection, and format/diff checks. Six commands were in the
initial plan; test-inclusive Clippy was the additional seventh command.
The expected rejection exits 1 before any adapter is created.

The ordinary release build completed successfully in 5:30.75. The preserved
33,545,216-byte executable has SHA-256
96eb7aa30554a2c4f5a0676aa2402e722a5e44dacccb57b503f4b6486e01e49b.
Source/config and executable hashes match before and after execution.

All 6/6 predeclared diagnostic runs completed all 1,000 actions at native
3840x2160. None failed, was replaced or was discarded. Each intentionally
returned exit 1 with DisabledDiagnostic, the completed-diagnostic message and
1,000 admitted observations. Every process reported llvmpipe (LLVM 22.1.8,
128 bits), Vulkan, Cpu. This remains software rendering, without KMS/scanout.

| Mode | Complete runs | Observations | Exact hits: capacity 1 / 2 / 4 | Peak retained payload, any run | Actual snapshot bytes copied, all runs |
| --- | ---: | ---: | ---: | ---: | ---: |
| Defaults | 3/3 | 3,000 | 0 / 0 / 0 | 751,904 B | 500,138,792 B |
| Adaptive coarse + bounds | 3/3 | 3,000 | 0 / 0 / 0 | 751,904 B | 505,002,856 B |

Across all six runs, payload sizes ranged from 163,320 to 187,976 bytes.
All 6,000 observations were admitted;
all were misses. Empty, oversized, invalid-metadata and recovery rejection
counts were zero. The four-entry LRU retained four payloads at each run's end.
The capacity-1/2 counts are recency-rank classifications from that same history.

Metadata change counters are all zero only because there were no payload hits;
they do NOT demonstrate stable damage, mode or bounds metadata. Likewise,
these are not 6,000 proven globally unique scenes: reuse outside the four-entry
history was not measured.

Per process, actual rendering remained 999 sparse + 1 full submissions, zero
recoveries. Bounds applied to 999 submissions in each bounds process and zero
in defaults. Row-mask, quad-skip, opaque-prune and solid-fuse selection remained
zero everywhere. Instrumented phase accounting still reports 33,177,600 CPU
output bytes per sample; average readback payload was 807,591 or 808,636 bytes
(rounded down). Neither is peak GPU memory or physical GPU-bus traffic.

## Decision and next priority

Do not implement or promote a small exact whole-resolved-scene cache from this
evidence: it would have had zero payload hits while copying/storing history.
No production cache was added. The added diagnostic is off by default.
A larger reuse distance or a narrower geometry-only cache is not ruled out.

The next bounded caching investigation, if pursued, should identify which
resolved sections change (path tags/data, transforms/styles versus draws),
including validated resolved layout/configuration. Comparing raw source Scene
objects alone would miss glyph expansion and resource resolution. Do not infer
safe geometry reuse from a whole-payload miss or an unguarded prefix match.

The existing native dense/full-render discrepancy remains a separate promotion
constraint. A causal GPU replay of the suspected segment-order difference is
stronger evidence than further passing replays; neither these CPU byte counts
nor the two passing native tests settle its cause.

No uninstrumented performance comparison was run for this diagnostic-only
source addition. Its phase timings remain in logs/results for investigation,
not as an improvement claim. The last ordinary performance matrix remains
../2026-09-06-segment-row-mask/README.md and still misses the absolute goal.
No commit, push or merge occurred. The goal remains active and unmet.
