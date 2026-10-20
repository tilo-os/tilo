# Native startup with a read-only Mesa shader archive

A release SDK test app reached its first compositor-painted frame in a median
247.579–268.592 ms with a precompiled shader archive, versus
1721.657–1749.095 ms with an empty private Mesa cache. These are six alternating
fresh app processes on the same software-rendering builder, three per mode.
The archive is a 154,688-byte public synthetic-scene artifact. This is a
validated fixture optimization, not a shipped OS cache policy or hardware result.
[Raw events, binary hashes, seed hashes and qualification](https://code.tilo.so/tilo/tilo/src/commit/0ebed9cae919c6f3e2a57565bde34beac0325ec5/docs/benchmarks/native-mesa-seed-2026-10-07.json).

| Order | Read-only seed | Compositor frame observation (ms) | SDK first presentation (ms) | Encoder-to-blit-copy interval (ms) |
| --- | --- | --- | --- | --- |
| 1 | No | 1721.657–1749.095 | 1638.774 | 1134.019 |
| 2 | Yes | 247.579–268.592 | 217.084 | 17.874 |
| 3 | No | 1742.811–1770.644 | 1698.640 | 1355.820 |
| 4 | Yes | 246.176–266.762 | 215.834 | 16.492 |
| 5 | No | 1447.986–1468.694 | 1398.577 | 1147.222 |
| 6 | Yes | 284.677–305.782 | 235.819 | 15.352 |

The SDK median is 1638.774 → 217.084 ms (86.8% reduction), including cache setup.
The host observation has a 20 ms polling interval plus control IPC; it is not
physical display latency. All six payloads used the ordinary native launcher,
Landlock FullyEnforced, no_new_privs and restricted networking. Every seeded
payload verifies that opening the source marker for writing is denied. Source
archive hashes remained unchanged. The registered host wrapper is unconfined;
its child is the measured sandboxed app.

## Attribution and cache mechanism

The existing cold interval persists in optimized release binaries. A separate
`startup-blit-phase-probe` uses the SDK device context and upstream texture
blitter, with no fonts, window or app state. It checks every destination pixel
on three frames per process. Adding a wait after raster submission moves the
roughly 1.1-second first-frame cost from blit submission into that wait. It
does not remove the cost. Thus a large blit interval alone cannot identify the
blitter as the expensive operation; pending driver work can block later calls.

On llvmpipe (Mesa 26.2.3, LLVM 22.1.8), the probe's read-only archive produces
39 cache hits and zero misses, compared with 39 misses for the empty cache.
Three alternating pairs reduced median first-frame GPU work from 1391.622 to
29.981 ms; setup plus first work fell from 1600.375 to 197.907 ms. All 393,088
pixels matched the expected BGRA red on each frame. The native app experiment
above then confirms the benefit through actual semantic launch and presentation.

Mesa's [cache environment documentation](https://docs.mesa3d.org/envvars.html)
and [cache implementation](https://gitlab.freedesktop.org/mesa/mesa/-/blob/main/src/util/disk_cache.c)
describe combining a read-only FOZ archive with the ordinary writable cache.
The fixture preserves Mesa's driver/LLVM/CPU identity and GPU-name namespace.
It does not force another device's cache or globally switch apps to an unbounded
single-file writable cache. [llvmpipe](https://docs.mesa3d.org/drivers/llvmpipe.html)
uses LLVM for runtime code generation; these results do not establish equivalent
savings on a physical GPU.

## Reproduction

Use the repository's selected Cargo execution environment for both compilation
and timing; execute these commands in that environment. No builds should run
concurrently with the timed commands.

```sh
"${CARGO:-cargo}" build --release --locked -p shell -p launcher -p tilo-ui \
  --features launcher/dev-overrides --bin shell --bin tilo-launcher \
  --example startup-blit-phase-probe --example startup-frame-probe \
  --example startup_frame_timeline -j8
target/release/examples/startup-blit-phase-probe \
  --export-cache target/startup-mesa-seed
target/release/examples/startup-blit-phase-probe \
  --seed-cache target/startup-mesa-seed
target/release/examples/startup_frame_timeline \
  --out target/startup-frame-mesa-balanced --samples 6 \
  --mesa-seed target/startup-mesa-seed --mesa-seed-source package \
  --alternate-mesa-seed
```

The archive export requires a new output directory and closes GPU objects before
copying completed archives. The app starts with a new private Mesa directory on
every process; other private app data is retained between samples. Seeded and
unseeded runs share the compositor, payload, driver and release binaries. The
host renames the seed directory between stopped processes; the app sees only
the existing read-only package bind. Existing driver/filesystem caches outside
the private app directory are not globally flushed. The six-process sample is
small and the synthetic app is a text payload, not forty application journeys.

`--fresh-mesa-cache` selects a fresh unseeded cache. `--mesa-seed-source session`
requires the existing session seed bind. At the time of this fixture experiment,
the headless launcher omitted that bind when no render node was selected, so
this mode failed observation rather than reporting a successful seeded run. The package mode above requires
no extra filesystem, device or networking permissions.

## Follow-up production qualification

The [ordinary publisher/launcher qualification](production-mesa-seed-2026-10-07.md)
adds the software-session bind and validates the production cache path,
invalidation, isolation and corruption recovery. Its measurements and limits
are separate from this fixture result.

This earlier experiment changed no production renderer, launcher policy, boot
unit or OS package.
A production publisher must own trusted public shaders, preserve compatible
driver/device identities and keep writable app caches private. It must be
measured with representative apps and cold OS boots; warming shaders on the
boot critical path could merely move the delay. Coordinate this with the
existing shared-renderer and device owners. This evidence identifies a concrete
common-path optimization without weakening the sandbox or claiming a measured
OS startup reduction.
