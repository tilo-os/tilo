# App proxy and OS helper startup

The production sandbox D-Bus readiness stage improved from **20.162 ms to
1.977 ms median** across 40 real launches per version on the existing Fedora
EPYC builder. Its nearest-rank p95 fell from **20.244 ms to 3.410 ms**.
[Raw samples and provenance](startup-proxy-2026-10-06.json) retain every sample.
This measures a launch stage, not whole-app first paint or physical-device boot.

## Production change

The launcher formerly checked the socket inode, then slept 20 ms between
checks. The real helper usually started during the first sleep, so the interval
became a roughly 20 ms floor. It now uses
[xdg-dbus-proxy's upstream readiness notification](https://github.com/flatpak/xdg-dbus-proxy/blob/main/dbus-proxy.c):
a private duplex descriptor receives `x` after the listeners start and remains
open for the guard's lifetime. Stdout/stderr remain available for diagnostics.
The existing two-second deadline, per-bus filters, and child/socket cleanup
remain enforced. Missing or invalid readiness refuses the helper.

The real accessibility allowlist integration now constructs its proxy through
the production launcher API. The new unready-marker regression fails against
the original implementation; successful helpers, timeout/invalid notification,
and guard cleanup pass with the candidate.

## Reproduction

Run on the measurement host with the same checkout/profile and installed helper.
Set `CARGO` to cargo or a compatible cargo drop-in:

```sh
"${CARGO:-cargo}" run --locked -p launcher --release --example dbus_proxy_startup -j8
```

The example starts its own private dbus-daemon, launches 40 production proxies,
and connects to every returned listener. It reaps all owned processes. Timing
ends when the production guard returns and excludes teardown. Baseline and
candidate were measured in that order, with warm filesystem caches allowed;
this is not a controlled cold-disk experiment.

## OS helper work and measurement limits

Identity verification/repair, portal-only, and firstboot-only shell commands
never paint. They now bypass render-node pinning and font/Vello warmup. Ordinary
desktop startup retains its existing warmup. The production identity test enables
both warmup knobs and verifies the helper records no warmup stage while still
failing closed without Secret Service and preserving existing metadata. Against
the original production entrypoint, the same test fails on the forbidden
warmup: fonts occupied a 310 ms background span and the explicitly enabled
Vello scratch render another 1.141 s. These are wasted-work observations in
that fixture, not measured OS boot savings; Vello warmup is opt-in.

The native image already excludes the obsolete root-owned shell-firstboot
service. This change does not claim additional savings from removing that unit.
A matched power-on-to-input-ready-greeter comparison is still unmeasured: the
available builder has neither KVM nor a physical render node, and this task
has not rebooted or deployed to the separately owned Yoga. Existing TCG boot
observations in other reports must not be presented as candidate speedups.

## Rejected shortcuts

Hashing all 36 shipped font files (54,891,768 bytes) with four workers rather
than one reduced the release median from 29.361 ms to 19.759 ms on this builder.
That small absolute saving did not justify changing font verification.

On llvmpipe (Mesa 26.2.3, LLVM 22.1.8), a native CPU-coarse renderer fixture
reduced cold renderer/first-dispatch median from 1.157 s to 0.444 s, but increased
warm frame median from 0.739 ms to 1.840 ms. Every fixture pixel was checked.
The production renderer policy was left unchanged; these synthetic measurements
are not app startup or hardware GPU results.

A subsequent same-adapter pipeline-cache test used fresh private Mesa disk-cache
directories for four alternating samples per mode. Both early and warmed
wgpu cache blobs were only 96 bytes. Cold median renderer/first-dispatch time
was 1.108 s without a seed, 1.108 s with the early seed and 1.101 s with the
warmed seed. That does not establish a useful speedup; seed publication timing
was left unchanged. This follows the [wgpu cache documentation](https://docs.rs/wgpu/latest/wgpu/struct.PipelineCache.html)
and [Mesa cache controls](https://docs.mesa3d.org/envvars.html), without assuming
a pipeline cache contains the driver's deferred shader compilation.

## Validation

Built and executed on the remote measurement host with `--locked`, eight jobs:

- Launcher `--lib dbus_proxy::`: six production/guard regression tests passed.
- Launcher `--test it bus_filters::`: real private-bus allowlist test passed.
- Launcher `--test it launcher_smoke::runtime_bundles_and_the_dbus_proxy_shape_the_launch_argv`: passed.
- Shell `--test it identity_startup::`: production-binary test passed, failed
  on forbidden warmup against original linux_main, then passed after restoring
  the candidate. The D-Bus marker regression also failed against original code.
- Shell `check --tests`: passed.
- Launcher `clippy --all-targets -- -D warnings`: passed.

Full OS assembly, device reboots, and hardware boot/app-first-frame timing were
not run. The benchmark does not establish a whole-desktop or OS speedup.
