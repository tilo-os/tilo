# CI performance and cache budget

CI has two distinct feedback paths:

- `check.yml` selects affected packages (including reverse dependencies) and
  balances them across two shards. It is the required per-change path.
- `nightly.yml` runs the complete workspace, all targets, examples, and live
  integration coverage. It is the coverage backstop rather than duplicated on
  every push.

Software-Vulkan golden tests are omitted from the two affected-package shards
and run once at the end of the nightly test job. A nextest test group can
serialize goldens with each other, but it cannot make them exclusive of the
thousands of ordinary tests in the same invocation (or of the other shard).
Loaded llvmpipe/lavapipe runs have produced stable false pixel deltas; the
isolated tail reuses the nightly job's compiled artifacts, so it preserves full
daily visual coverage without another workspace build or additional test CPU.
The check, nightly, and baseline-refresh workflows also pin Vulkan's ICD to
Mesa lavapipe and require wgpu's `llvmpipe` adapter. Runner nodes can expose
different physical GPUs, and `WGPU_BACKEND=vulkan` alone selects the API rather
than a particular adapter; a golden produced on one node is not canonical
unless both the driver and adapter are fixed.

Two affected shards are intentional. A third, permanently dedicated "heavy
apps" shard was measured as the wrong trade-off for this workspace: it repeats
large shared dependency graphs and adds runner CPU even when a change does not
touch those apps. `tools/ci_select.py` already weights costly packages while
preserving dependency-correct selection, so heavy packages receive their own
capacity when they are affected without creating an always-on third compile.

## Cache layers

- sccache stores compiler outputs in the repository-scoped S3 prefix
  `forgejo-runners/v1/`. This is the main cross-runner Rust cache.
- The immutable Cargo target archive is a fallback for artifacts sccache cannot
  represent. Only shard 1 writes a missing key, avoiding duplicate uploads.
- The OS image build uses a registry-backed BuildKit cache because its
  intermediate container layers are not compiler outputs.

The layers solve different problems; replacing all three with a single cache
would either omit non-compiler artifacts or upload entire target directories on
every shard.

## Build-profile reuse

Required and nightly tests set both `CARGO_PROFILE_TEST_DEBUG=0` and
`CARGO_PROFILE_DEV_DEBUG=0`. Cargo puts the built-in test and dev profiles in
the same `target/debug` tree, but debuginfo is part of an artifact's
fingerprint. Leaving dev at its default therefore made the all-target and
example steps rebuild dependencies that nextest had just emitted, solely to
add debugger data that CI never consumes.

A controlled qdoc-pdf probe used fresh target directories and no compiler
cache. After `cargo test --no-run`, building its examples took 3.35 seconds
with matching zero-debuginfo profiles versus 54.47 seconds with Cargo's default
dev debuginfo. A smaller collab-types test/check probe reduced the subsequent
all-target check from 10.00 to 7.51 seconds. The workflows retain separate
all-target and linked-example gates—the coverage is unchanged—while sharing
compatible dependency artifacts between them.

## Measurements

Numbers below come from Forgejo's `/usr/bin/time` output and sccache counters.
They include all 11.5k workspace tests, not a synthetic microbenchmark.

| Run | Date | Wall | User + system CPU | sccache hit rate |
| --- | --- | ---: | ---: | ---: |
| nightly 1279 | 2026-08-31 | 573.00 s | 1,708.26 s | 20.61% |
| nightly 1298 | 2026-08-31 | 436.71 s | 1,731.32 s | 93.35% |

The effective remote cache reduced end-to-end wall time by 23.8% while total
process CPU stayed within 1.4%. Run 1298 recorded 3,883 compile requests, 2,806
hits and 200 misses among 3,015 executed requests (Rust 90.60%, C/C++ 99.86%).
Older fully warm runs reached 100% and completed compilation in roughly two
minutes; cold run 944 was effectively uncached at 0.07%.

These are operational baselines, not permanent targets. Keep `ci-measure.sh`
around the test/check commands and compare wall time *and* total CPU before
adding shards or caches; a faster wall clock that doubles runner CPU is a
regression for this installation.
