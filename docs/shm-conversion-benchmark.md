# SHM conversion runtime comparison

Measured on 2026-10-05/06 UTC against commit `4b1210dc7c`. The changed production
path appends a complete initialized row once, then swaps its blue/red channels
in place, rather than performing four capacity-checked `Vec::push` calls per
pixel. XRGB still forces alpha to 255; ARGB preserves it. Validation, guarded
SHM access and sandbox policy remain in the production path.

## Method and results

The [example](../crates/session/shell/examples/shm_swizzle_bench.rs) invokes
`swizzle_row_to_rgba` directly. Both preserved binaries use the same harness,
lockfile and repository release settings (O3, fat LTO, one codegen unit,
existing x86-64-v3 baseline and hardening flags). No new CPU flags were enabled.
The host is an AMD EPYC 9R45, x86-64 KVM remote builder shared with other jobs;
runs were unpinned. This is synthetic CPU work, not a physical display test.

Ten alternating baseline/candidate pairs each take eight warmups and 100
samples per case: 1,000 measured samples per case and variant. Each sample
converts all rows into reused frame storage. Every run verifies all pixel bytes,
frame length and stable destination capacity. Results below pool the samples
and use nearest-rank percentiles; times are milliseconds per complete frame.
[Raw samples, binary hashes and toolchain metadata](benchmarks/shm-swizzle-2026-10-05.json)
are preserved. Shared-host scheduling can affect tails, especially the baseline
4K p99; these measurements do not establish an isolated-machine guarantee.

The recorded harness file hash is from measurement time. A later documentation
comment change makes its run command transport-neutral; the evidence records
both file hashes and the identical executable-source hash (excluding module
documentation comments). Production conversion code and raw samples are
unchanged. The 44–66% reduction at 1080p/4K applies only to this CPU conversion.

| Size | Format | Baseline p50 | Candidate p50 | Less time | p95 baseline / candidate | p99 baseline / candidate |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 801×601 | Argb8888 | 0.465 | 0.147 | 68.3% | 0.505 / 0.162 | 0.527 / 0.173 |
| 801×601 | Xrgb8888 | 0.464 | 0.200 | 56.9% | 0.514 / 0.220 | 0.527 / 0.229 |
| 1920×1080 | Argb8888 | 2.006 | 0.691 | 65.6% | 2.155 / 0.819 | 2.224 / 0.921 |
| 1920×1080 | Xrgb8888 | 2.008 | 0.932 | 53.6% | 2.127 / 1.054 | 2.184 / 1.091 |
| 3840×2160 | Argb8888 | 8.009 | 3.749 | 53.2% | 8.530 / 4.044 | 16.813 / 4.199 |
| 3840×2160 | Xrgb8888 | 8.432 | 4.677 | 44.5% | 9.047 / 4.999 | 11.822 / 5.134 |

Benchmark process peak RSS measured with `/usr/bin/time` had medians 66,926 KiB
baseline and 66,922 KiB candidate (ranges 66,420–67,180 and 66,448–67,244 KiB).
This is process memory for the conversion fixture, not desktop memory. The
predeclared goal of halving median conversion time at 1080p/4K was met for both
1080p formats and 4K ARGB, but 4K XRGB improved 44.5% and missed that target.
Mapping, capture copies/queues, encoding, GPU upload and scanout are excluded;
these numbers cannot be converted into whole-frame or whole-desktop speedups.

Reproduce in the same benchmark environment. Use Cargo there or a configured
remote Cargo drop-in that executes examples on the builder:

```bash
cargo run --locked -j 8 -p shell --release --example shm_swizzle_bench
cargo test --locked -j 8 -p shell --lib compositor::surface_readback::tests::
cargo test --locked -j 8 -p shell --test it shm_readback::
cargo check --locked -j 8 -p shell --tests --examples
```

For a paired comparison, build this same example at the base and candidate
revisions in separate worktrees. Preserve each binary before rebuilding, then
alternate their execution on the same builder. Keep flags, warmups, fixtures,
revision, machine and samples with the results. Do not share target directories.
Copy the same example into the base checkout before building it; the historical
base predates this harness. Keep all other baseline inputs unchanged.

The [CPU frame-copy investigation](shm-pipeline-benchmark.md) profiles a next
target in a separate fixture; its results are not a before/after comparison with
this conversion-only benchmark.

## Reference and remaining system measurements

The [exact linked post](https://x.com/ThePrimeagen/status/2106156035602022630)
says “AI is confused as to how fast omarchy installs via vm”. Its screenshot
mentions a 6.8 GB installed disk but supplies no measured duration or benchmark
link. It is an Omarchy installation anecdote, not a Clear Linux result.
[Clear Linux's performance documentation](https://clearlinux.github.io/clear-linux-documentation/guides/clear/performance.html)
describes measured compiler and system tuning. The
[2024 benchmark author's comparison](https://www.phoronix.com/review/intel-ubuntu2404-fedora40/6)
is a different CPU, distro image and workload collection; it does not establish
that any particular optimization transfers to Tilo.

| Requested dimension | Evidence in this change | Missing comparison |
| --- | --- | --- |
| Boot/install | No device or guest reboot/install performed | Matching image baseline/candidate and authorized disposable boot cycle |
| Native app startup | Not measured | Matching signed app catalog and disposable launch fixture |
| Interaction / presentation | Not measured | Matched backend, input trace and presentation timestamps; physical Yoga session belongs to other runtime work |
| Frame work | SHM conversion above; actual Wayland capture test added | Complete rendered/presented frame comparison |
| Memory | Conversion process RSS and stable destination capacity | Representative desktop/app working sets |

The existing frame-loop runner now rejects missing requested load clients,
missing Wayland endpoints, malformed/empty traces and duplicate trace columns.
It keeps the load alive through the configured launch budget and reaps its own
session group on early failure. Those integrity fixes are not evidence of a
measured frame-loop improvement. Full boot, startup and presentation claims
remain unestablished. No hardware deployment or reboot was performed.
