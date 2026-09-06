# XRGB damage-output: completed comparison

The goal is **not met**. No default promotion, commit, push or merge.

All 24 predeclared attempts completed all 1,000 real actions at native
3840×2160. None was replaced or omitted. All 18 shell runs missed at least
one gate; one of six UI controls passed its numeric p99 gate, but the UI XRGB
flag has no code path and its binary is byte-identical to the previous control.
That isolated UI pass is not an improvement attributable to this change.

## Current matrix

Mean, p99 and RSS are medians of three complete runs. Worst sample is the
largest observed across those runs. These are exploratory variants in the
ordinary release build, not acceptance-eligible default configurations.

| Workload | Variant | Mean ms | p99 ms | Worst ms | Peak RSS MiB | Numeric passes |
|---|---|---:|---:|---:|---:|---:|
| shell | Default | 38.241 | 96.059 | 653.168 | 566.0 | 0/3 |
| shell | XRGB partial output | 36.020 | 95.450 | 598.330 | 562.6 | 0/3 |
| shell | Tile bounds | 25.130 | 68.981 | 676.379 | 570.2 | 0/3 |
| shell | Tile bounds + XRGB | 22.166 | 68.852 | 579.892 | 570.9 | 0/3 |
| shell | Tile bounds + canonical order | 27.393 | 75.260 | 727.216 | 577.7 | 0/3 |
| shell | Tile bounds + canonical + XRGB | 25.191 | 72.345 | 684.592 | 577.8 | 0/3 |
| ui | Default | 8.052 | 37.309 | 115.514 | 368.3 | 0/3 |
| ui | XRGB partial output | 7.903 | 34.712 | 112.344 | 368.7 | 1/3 |

Shell gates remain exact total/sample mean ≤25 ms and p99 ≤50 ms.
UI p99 remains ≤30 ms. Complete gate failures stay in the medians and
denominators (shell exit 1; UI exit 2). No timeout occurred in this comparison.

## Matched effect of partial CPU output

Negative changes mean less time/RSS. Ratios below compare the medians, not
a pooled frame distribution or selected best runs.

| Matched pair | Mean change | p99 change | Peak RSS change |
|---|---:|---:|---:|
| Default → XRGB partial output | -5.81% | -0.63% | -0.59% |
| Tile bounds → Tile bounds + XRGB | -11.80% | -0.19% | 0.12% |
| Tile bounds + canonical order → Tile bounds + canonical + XRGB | -8.04% | -3.87% | 0.01% |

All nine matched shell pairs have lower mean time with partial output.
Individual mean changes range from -1.84% to -13.40%. Tail improvement is
not consistent for X/D or BX/B: the first pair in each worsened p99 by 30.04%
and 13.65% respectively. The fastest median-mean variant (BX) still has
68.852 ms p99 and a 579.892 ms worst sample. Canonical order remains slower
in median mean than the corresponding bounds-only variant.

Every full-output control writes 33,177,600,000 CPU destination bytes across
1,000 paints (33.18 GB, decimal). Every X variant performs one full write and
999 partial writes, totaling 807,591,936–808,636,416 bytes (about 0.81 GB):
**97.56% fewer CPU output bytes**. The small total variation is preserved;
it is not normalized away. The target remains full native 4K.

This is CPU destination-write traffic, **not** memory footprint, GPU bus
traffic, GPU readback savings or peak VRAM. Peak process RSS did not show
a material repeatable reduction. UI known GPU accounting remains 62,282,124
bytes and CPU readback remains zero in every UI run. Shell ordinary reports
do not provide a comparable peak-GPU-memory figure.

Every shell run records 999 sparse + 1 full render attempt and zero recovery
attempts. Bounds variants actually apply bounds on sparse submissions, and
X variants actually execute partial CPU writes; their request flags alone
are not taken as evidence.

## Validation and provenance

- Version v5: all 17 planned commands pass; 803 passing test executions across
  repeated configurations, zero failures, seven ignored older explicit diagnostics.
- No adapter-unavailable skip in the selected runtime suites.
- Original independent native GPU oracle passes D/B/BC this round. The prior
  independent default rounding mismatch remains unresolved; this single round
  does not prove its cause fixed.
- New native current-staging oracle passes X/BX/BCX for one and alternating two
  targets with padded pitch and actual partial writes.
- Cursor restoration, overflow/recovery, cap, revision/config, abort, short
  buffer and post-map cleanup checks pass.
- Ordinary release build succeeds in 9m 57.60s, without tuning compiler or
  renderer-worker settings. Build peak RSS is not application memory.
- All 33 source/config/fixture input hashes and both executable hashes match
  after comparison.
- All 24 disk logs were re-read and independently parsed; raw output, exit,
  parsed fields and evidence checks exactly match the captured results.
- No source editing, build, test or other benchmark overlapped timing.

Machine: aarch64 Qualcomm Oryon, Vulkan llvmpipe (LLVM 22.1.8, 128 bits),
CPU device. This does not prove physical 2010-laptop performance, KMS scanout,
or constant 60 FPS. The existing x86-64-v3 build floor is unchanged.

## Decision and next priority

Retain the candidate as an opt-in, validated bandwidth/mean improvement;
do not promote it as goal completion. The next performance work must address
the GPU-completion/fine-rendering tail and UI p99, not claim that fewer CPU
bytes solved them. The earlier renderer/pool audit records unmeasured options
and rejected hypotheses; no additional speedup is claimed from that audit.

Polish, a scoped commit and a push remain conditional on actually meeting
the default-build goal. The user will merge.

Raw commands and logs: comparison-plan.json. Per-attempt data:
comparison-runs.json. Aggregates and paired deltas: comparison-summary.json.
Source/binary/log provenance: v5-source.sha256, v5-release-binaries.sha256,
postcomparison-source.sha256, comparison-logs.sha256 and postcomparison-audit.json.
