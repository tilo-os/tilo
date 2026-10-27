# Registered native app first-frame timeline

A synthetic native SDK app reached a compositor-painted frame through the
existing semantic launch action and ordinary net-off sandbox. This repairs an
unusable measurement path; it does not establish a startup speedup.
[Raw events, frame metadata and binary hashes](https://code.tilo.so/tilo/tilo/src/commit/0ebed9cae919c6f3e2a57565bde34beac0325ec5/docs/benchmarks/native-first-frame-2026-10-06.json).

## Measured timeline

| Process order | Compositor-painted frame observation (ms) | SDK first presentation (ms) | First blit-copy interval (ms) |
| --- | --- | --- | --- |
| First | 3141.994–3186.588 | 2773.788 | 1303.456 |
| Second | 651.084–689.132 | 601.724 | 17.888 |
| Third | 485.877–521.304 | 419.778 | 0.312 |

Compositor times start immediately before the semantic launch request. Bounds
include the 20 ms poll interval and control IPC duration. SDK times start inside
the app process, after launcher work, and cannot be subtracted from host times
as if they shared an origin. The compositor observation requires a new matching
surface, nonzero frame sequence, presented=true, opening=false and no frame
error. SDK presentation is the actual render-worker Presented outcome, rather
than merely queuing a render request. The live trace contained 398 rows.

These are instrumented debug binaries on the existing headless Fedora builder,
not release builds, KVM, laptop startup or device-to-photon latency. Processes
are fresh; private app state begins empty and is retained between samples.
Filesystem and driver caches are not flushed. The first sample is therefore
not a controlled cold-cache measurement. No Rust builds run during timing.

The largest first-process interval is inside the render-worker blit/copy path.
Its collapse on subsequent processes is evidence for investigating cold driver
or shader initialization, not proof of a particular JIT/cache mechanism. That
source belongs to the existing renderer owner. Device selection (76–205 ms) and
Vello setup (110–147 ms) also deserve matched release/hardware measurements.
The SDK refactor, renderer policy and shader interfaces were not changed here.
The earlier [installed Terminal trace](terminal-startup-trace-2026-10-06.md)
also identifies this checkpoint as dominant (20.631 seconds in its TCG fixture).
Those different binaries/environments are corroborating diagnostic evidence,
not a matched comparison or measured improvement.

## Ordinary OS trace diagnosis

On the pinned developer OS image a2e65c82913b52e1771464a167c03c41ae422933,
session logging explicitly said trace=true. The correct session trace existed
with a header only: 235 bytes at guest monotonic 105.51 and 108.76 seconds.
After read-only semantic queries woke the loop it grew to 494 bytes/four lines
at 111.86 seconds. Direct process environment/fd reads were denied; they were
not bypassed. This was an ordinary first-use session, not an enrolled app
qualification, and no physical reboot was performed.

The production recorder wrote early samples into a BufWriter and flushed only
when a sample started after its one-second deadline. An idle session could
therefore leave completed startup rows buffered indefinitely. The change
flushes the first completed row immediately, preserving periodic later flushes
without adding an idle timer. The regression reads the actual production
end_tick output while the recorder remains alive; a header alone is insufficient.
With the original flush condition restored, the regression fails exactly on
its live-file line count (observed one, required two). The candidate passed all
seven focused frame-metrics tests, with no ignored tests. The required remote
`check --locked -p shell --tests -j8` passed, including integration targets.
The exact staged-file pre-commit hook, Python syntax and diff checks passed.

## Reproduction and boundaries

Build shell, launcher and the native SDK probe using Cargo or a compatible
Cargo drop-in. Set `CARGO` to that executable and run the harness in the same
build environment:

```sh
"${CARGO:-cargo}" build --locked -p shell -p launcher -p tilo-ui \
  --bin shell --bin tilo-launcher --example startup-frame-probe -j8
"${CARGO:-cargo}" run --locked -p launcher \
  --example startup_frame_timeline -j8 -- \
  --out target/startup-frame-timeline --samples 3
```

The private fixture creates a temporary desktop entry before starting the
compositor. Its existing host-applications action registers wrapper ancestry;
the wrapper's child enters the normal native launcher sandbox. The wrapper
itself is a host launch and is accurately logged as unconfined. The measured
payload's launcher logs confirm Landlock FullyEnforced and restricted networking.
The compositor uses its normal services in the private profile; its peer-sync
broker was enabled. No peer connection or invite actions were issued. The
net-off statement applies to the confined payload, not the host compositor.
This is not a signed package installation benchmark. A raw sandbox client
launched outside registered ancestry was rejected by the compositor; that
security check remains intact. There are no enrollment or capability-grant
actions, identity state belongs only to the fixture, and owned processes are
cleaned up. No stopped Notes WASM investigation was resumed.

Full OS cold boots and physical GPU comparisons remain separate measurements;
coordinate those with the existing device owner after its lock repair.
