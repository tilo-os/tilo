# Current measured state — canonical ordering v4

The goal remains unmet. All 24 predeclared attempts were executed serially:
23 completed 1,000 actions; the first default shell attempt failed during
initial paint with a readback-fence timeout and has no latency summary. It
was not replaced or treated as zero. Only one UI run passed its numeric gate;
no shell run passed both shell gates. No default promotion, commit or push.

## Matrix

Medians of completed run means and p99s, not a pooled percentile. Worst is
the largest retained individual sample. Peak RSS is the median peak process
RSS of completed runs, not GPU allocation or dedicated VRAM. The two completed
default shell runs have a different denominator; their median is the average
of those two values. Every failed/incomplete attempt remains in the CSV/JSON.

| Workload | Variant | Mean ms | p99 ms | Worst ms | Peak RSS KiB | Complete | Numeric pass |
|---|---|---:|---:|---:|---:|---:|---:|
| shell | default | 40.349 | 111.026 | 498.628 | 580232 | 2/3 | 0/3 |
| shell | bounds | 26.023 | 71.020 | 614.137 | 580724 | 3/3 | 0/3 |
| shell | canonical | 40.785 | 88.247 | 547.202 | 584536 | 3/3 | 0/3 |
| shell | bounds-canonical | 28.760 | 75.631 | 665.195 | 589832 | 3/3 | 0/3 |
| ui | default | 8.570 | 41.094 | 114.436 | 377592 | 3/3 | 0/3 |
| ui | bounds | 7.921 | 32.834 | 78.453 | 386376 | 3/3 | 0/3 |
| ui | canonical | 9.126 | 47.470 | 141.439 | 386672 | 3/3 | 0/3 |
| ui | bounds-canonical | 8.554 | 35.359 | 109.602 | 392064 | 3/3 | 1/3 |

D/default has no candidate flags. B/bounds enables adaptive coarse and native
tile bounds. C/canonical enables general canonical segment ordering.
BC/bounds-canonical enables all three. Targets are UI p99 <= 30 ms, shell
exact mean <= 25 ms and p99 <= 50 ms. Shell means use total_us / samples before
comparison, not the rounded display mean. The CSV retains that precision.

These are ordinary release builds with exploratory runtime flags. They are
not acceptance-eligible: the default native pixel oracle still has a retained
3-byte mismatch outside damage. See the pre-timing baseline exception; neither
the assertion nor its outcome was changed to permit a success claim.

## What improved, and what did not

- B versus D observed shell median mean -35.505% and p99 -36.033%, with
  +0.085% median completed-run RSS. D completed only 2/3 attempts. These
  observed differences do not mean shell met either target.
- Adding C to B worsened shell median mean by 10.516%, p99 by 6.493% and
  median peak RSS by 1.568%. All three paired shell means increased.
- Adding C to B worsened UI median mean by 7.991%, p99 by 7.690% and median
  peak RSS by 1.472%. All three paired UI means increased. The one passing
  BC UI p99 was 28.147 ms; its other runs were 43.169 and 35.359 ms.
- The new general ordering passed its candidate native pixel and repeatability
  checks. That is a correctness/reproducibility result, not a speed gain or
  proof that every possible scene is correct.
- C/D and BC/B include moving the existing bump readback after fine. They
  cannot isolate sorting cost from that readback-order change.
- UI applied tile bounds in 0/1,000 submissions in every run. Some B/BC
  submissions selected adaptive coarse (67–99 and 80–92 respectively);
  their observed UI differences cannot be attributed to tile bounds.

## Work and memory accounting

Every completed shell run recorded 1,000 render attempts: 999 sparse, 1 full,
0 recovery. B and BC applied bounds to all 999 sparse submissions. C and BC
selected canonical ordering for 1 full and 999 sparse attempts; other modes
selected none. Each C/BC UI run selected canonical ordering on 1,000 sparse
attempts and no full attempts. Selected means recorded, not merely requested.

UI known GPU bytes ended at 62,282,124 in every run, and CPU pixel readback
was 0 bytes. This endpoint is neither peak VRAM nor a physical bus-transfer
measurement. RSS includes driver/runtime memory outside that accounting.

The separate native correctness oracles measured 75,915,264 sparse/full GPU
readback payload bytes across 13 retained attempts. The ordinary shell
timing runs did not enable per-phase byte accounting. Source inspection shows
the CPU still writes the whole tight 4K XRGB destination: 33,177,600 bytes
per successful paint. Do not describe that CPU write as a whole-frame GPU copy.

## Evidence and integrity

- [All attempts CSV](comparison-results.csv)
- [Parsed results, denominators and differences](comparison-summary.json)
- [Frozen commands/order](comparison-plan.json)
- [Predeclared interpretation](comparison-contract.json)
- [Default failure exception, recorded before timing](pre-timing-baseline-exception.md)
- [Validation summary](v4-validation-summary.json)
- [Build and binary hashes](v4-release-binaries.json)
- [Source inputs](v4-source.sha256)
- [Copy accounting](copy-accounting.md)

All 28 source/config/fixture hashes and both copied executable hashes matched
after the comparison. All 24 logs were reparsed and checked against the saved
results. Exact selection headers, actual workload extent, action counts,
shell totals/recovery and UI pixel-readback accounting were audited. No source,
compiler, test or other benchmark overlapped these timings.

Host adapter: llvmpipe (LLVM 22.1.8, 128 bits), Vulkan, device type Cpu on
aarch64. This does not certify a 2010 laptop or constant 60 FPS. The harness
does not time DRM/KMS page flips or display scanout.

## Next bounded opportunity

A read-only Terra design identifies how to avoid the full CPU XRGB write in
the real alternating two-buffer scanout path. It requires per-buffer revision,
accumulated damage, previous cursor footprint and fail-closed invalidation.
It is not implemented or measured. Existing diagnostic profiles place the
CPU swizzle around 2.0–2.2 ms mean, with GPU completion much larger, so this
cannot be presented as enough by itself to meet the tail target.
