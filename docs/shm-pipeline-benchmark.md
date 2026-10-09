# CPU frame-copy cost after SHM conversion

The next measured cost is frame-store copying: at 4K, publication plus snapshot
cloning take about 2.3 ms per frame in this CPU fixture, including about 1.3 ms
for the snapshot. Conversion remains the largest individual stage. This is a
concrete next target, not evidence of a whole-desktop responsiveness gain.

## Method

The [pipeline example](../crates/session/shell/examples/shm_pipeline_bench.rs)
calls production `swizzle_row_to_rgba`, `store().publish_rgba` and
`store().snapshot` in sequence, using a private surface entry. It creates no
compositor or display and deletes its entry on completion/error. Every sample
checks snapshot dimensions and pixel equality outside the timed region; each
case verifies all 108 publications and stable conversion storage capacity.

Five repeat executions on the same shared AMD EPYC 9R45 x86-64 KVM builder each
use eight warmups and 100 samples per case: 500 samples per case. The integration
base is `1e3b221d24740e0f4d94d3ffbb358c7474433531`; the added harness is recorded
by its source hash. Production code was unchanged. The release profile uses
O3, fat LTO, one codegen unit and the existing x86-64-v3/hardening flags.
[Raw samples and final source/binary/compiler identities](benchmarks/shm-pipeline-2026-10-06.json)
are preserved. This unpinned shared host does not establish isolated-machine
tail guarantees. All times below are nearest-rank milliseconds; the copies
column uses per-sample publication plus snapshot time before taking its median.

| Size | Format | Conversion p50 | Publication p50 | Snapshot p50 | Copies p50 | Total p50 | Snapshot p95 |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1920×1080 | Argb8888 | 0.819 | 0.150 | 0.158 | 0.310 | 1.129 | 0.173 |
| 1920×1080 | Xrgb8888 | 1.046 | 0.152 | 0.161 | 0.314 | 1.362 | 0.173 |
| 3840×2160 | Argb8888 | 3.588 | 1.044 | 1.303 | 2.347 | 5.939 | 1.320 |
| 3840×2160 | Xrgb8888 | 4.468 | 0.967 | 1.347 | 2.315 | 6.786 | 1.362 |

Process peak RSS median is 132,532 KiB. The fixture retains source, conversion
output, stored frame and transient snapshot buffers. This is not representative
desktop memory and is a different working set from the conversion-only fixture.
Checks outside timing can warm buffers; the source pixels repeat. Mapping,
guarded pool copying, worker queue waits, image preparation/upload, GPU work,
subsurfaces, cropping, HDR and presentation are not measured. The sequential
CPU total does not describe the parallel worker/render critical path.

## Comparability and ownership

The [original 44–66% reduction at 1080p/4K](shm-conversion-benchmark.md) belongs
to the archived, matched baseline/candidate conversion binaries built with the
`4b1210dc7c` dependency/profile inputs. The original head `1e3b221d` already used
a newer integration base. Its production conversion source matches the measured
candidate, but these experiments do not remeasure that percentage on a redesigned
desktop. The original harness's only later change is its transport-neutral run
comment; the original file hash, updated hash and identical executable-source
hash are recorded, and its raw samples/binary hashes are unchanged.

The pipeline is a different fixture with extra buffers and per-sample validation.
Do not subtract its conversion time from the old microbenchmark or infer a
regression/speedup from that comparison. Compare its stages within this fixture.

`publish_frame` copies RGBA into the stored vector while holding the entries
mutex. The CPU `snapshot` route clones that vector while holding the same mutex;
`frame_image` uses snapshots for CPU frames and the widget refreshes its image
on a new frame sequence. Concurrent lock contention is not measured here.
These operations and image ownership belong with the current rendering redesign.
This change adds evidence only; it does not duplicate that refactor or change
production frame ownership.

## Bounded next step

1. Hand this 4K snapshot/publication target to the renderer owner and agree on
   its immutable CPU-frame ownership contract. Keep the mandatory guarded copy
   out of client SHM; borrowing an untrusted client mapping is not an optimization.
2. After that contract settles, prototype eliminating the snapshot's full-frame
   copy for the no-child/no-crop CPU route, with an explicit pixel lifetime.
   A target is at least 50% less snapshot CPU time in this same fixture, not a
   claimed result. Keep generation ordering, old snapshots, geometry, subsurface
   composition and HDR behavior correct; do not change the direct GPU route.
3. Compare matched before/after binaries on the same builder, test retained old
   frames across new commits and all affected geometry routes, then use the
   existing disposable frame-loop fixture for interaction/presentation evidence.
   Physical hardware comparisons wait for a matching fixture and coordination.

Reproduce inside the chosen benchmark environment, using Cargo there or a remote
Cargo drop-in configured to execute examples on the builder:

```bash
cargo run --locked -j 8 -p shell --release --example shm_pipeline_bench
```

No new production optimization, hardware test, reboot, release or deployment was
performed for this investigation. PR #2919 remains draft and held during the
rendering redesign.

## Redesign integration

The feature branch incorporates the merged unified SDK redesign at
`fc91d3bb78` using a merge from main. The merge required no changes to the
conversion implementation, CPU fixture or Wayland SHM test. It preserves the
redesign's frame-store and UI changes; Contacts, Messages and the ongoing UI
crate split remain outside this change's ownership. Integration checks establish
API and pixel correctness on that base, not a new performance comparison. The
timings above remain tied to their recorded source and binary identities.

SDK browser qualification on this integration exposed the same Calendar and
Shortcuts `no default font found` panic as exact redesign-base run #71835.
The branch also incorporates main's existing browser font-publication fix
`6b30d9bb4a` and subsequent qualification fixes through `88a53d1979`, rather
than duplicating those changes. No benchmark timings were refreshed by these
integration merges.

## Frame ownership since TIL-586

The timings above predate shared frame ownership; they are not refreshed here.
Stored frames are now `Arc<Vec<u8>>`: `publish_rgba` copies the caller's slice
before taking the entries mutex and swaps the `Arc` under it, `snapshot`
clones the `Arc` under the mutex and copies after releasing it, and the tile's
`frame_image` draws a whole, uncropped CPU frame from the stored allocation
without a copy. The production SHM route copies only a commit's buffer damage
out of the client pool while the worker's base frame is the previous SHM
buffer, converts only that damage, and writes it into the frame the current
one replaced when nothing draws that any more. The example's
`shm-capture-copy-conversion` rows measure that pool copy plus conversion for
a whole 4K commit and a 64×32 damage on one; the SIGBUS guard, queueing and
Vello's whole-image atlas upload remain unmeasured.
