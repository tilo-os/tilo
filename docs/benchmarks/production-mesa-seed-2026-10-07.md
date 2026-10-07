# Ordinary native startup with a session-generated Mesa archive

The ordinary native launcher reduced median SDK first presentation from
1368.618 to 230.676 ms (83.1%) for a synthetic text app. Median semantic-launch
to compositor-frame observation fell from 1416.399–1444.204 to
281.086–310.024 ms. Three alternating cold/seed pairs ran in optimized release
binaries on Mesa 26.2.3 / LLVM 22.1.8 llvmpipe, AMD EPYC 9R45. These are
software-renderer results, not physical display latency or whole-OS boot gains.
[Raw events, binary/archive hashes and qualification](production-mesa-seed-2026-10-07.json).

Unlike the earlier [fixture experiment](native-mesa-seed-2026-10-07.md), this
run waits for the normal compositor to generate its archive, then launches
through the existing host-app action and native sandbox. It uses no test-only
Mesa cache marker or external cache overrides. Each launch has a fresh private
app cache. Other app data and host filesystem/driver caches are retained;
other users' caches and devices are not changed.

| Order | Mode | Compositor first-frame observation (ms) | SDK first presentation (ms) |
| --- | --- | --- | --- |
| 1 | cold | 1697.878–1726.117 | 1632.880 |
| 2 | seed | 274.030–300.878 | 220.796 |
| 3 | cold | 1416.399–1444.204 | 1368.618 |
| 4 | seed | 308.372–333.654 | 250.441 |
| 5 | cold | 1359.262–1383.431 | 1310.771 |
| 6 | seed | 281.086–310.024 | 230.676 |
| 7 | wrong-driver | 1390.083–1415.442 | 1352.355 |
| 8 | corrupt | 1397.803–1420.331 | 1349.511 |

The host interval includes launcher/cache-copy work, a 20 ms polling interval
and control IPC. SDK timing begins inside the sandboxed app and excludes
launcher preparation. Cold/seed medians use only the six alternating samples;
the two fallback controls are separate. All eight payloads report fully
enforced Landlock, no_new_privs and restricted networking. Every launch with
a session source observes denied write-open access. Source archive hashes stay
unchanged. An incompatible namespace is accepted as a structurally valid
archive but ignored by Mesa: it presents in 1352.355 ms. Corrupt bytes are
rejected before consumption and present through cold fallback in 1349.511 ms.

## Production behavior and bounded storage

After its first successful render, the compositor runs one trusted maintenance
job for a fixed public red scene, deferred one second and at lower CPU priority.
The child has a 30-second deadline, bounded diagnostic output and kernel
parent-death protection. No app scene, font content or user document enters the
archive. This run generated 154,688 bytes in four FOZ files; publication wait
was 2455.433 ms after the harness started observing it, not OS boot time.

The persistent source is limited to 32 MiB and 16 files. Its format, real file
and directory types, paths, archive pairing, lengths, headers and hashes are
validated before use. The source fingerprint includes the actual compositor
binary, selected adapter/driver and CPU capabilities. Mesa's opaque compiler,
CPU and device namespaces remain intact. Apps receive the existing read-only
session bind and copy validated bytes into a private cache after Landlock
restriction. Their ordinary writable cache is preserved, configured with
Mesa's 16 MiB limit; this is not a claim of an aggregate cap across historical
driver namespaces. CLI/service launches and non-rendering identity helpers
skip consumption or preparation. Missing, damaged and incompatible caches
retain ordinary compilation.

The normal session path also passed three sequential checks: a valid persistent
archive restores into a fresh runtime directory and private cache, a damaged
persistent archive falls back and is regenerated, and the repaired archive
restores in the next session. Socket peer credentials match each originally
spawned PID, and every valid restore creates all four private seed files.
Valid sessions do not republish the source. These checks include intentional
waits and are not startup timing comparisons. Reading process environments
was denied by existing process protections; qualification used the existing
control socket and owned cache files instead, without adding permissions.

The current release read-only identity-verification helper also ignores a
corrupt private shader cache and fails closed as expected. The prior release
snapshot had inspected that cache; gating all four non-rendering helper modes
fixes that regression. The production identity-provision test covers the
same rule while preserving existing identity metadata on Secret Service failure.

## Idle observation and limits

After all eight apps exited and the headless watcher was disabled, the owned
compositor used 0.400% of one CPU core over 5.000 seconds. RSS was
406,956–406,960 KiB (about 397 MiB). This small observation has no matched
pre-change idle comparison and is not whole-OS idle usage or an app-memory gain.

The app comparison begins after publication. It does not quantify contention
from background generation during the earliest launch in a completely uncached
session. No physical GPU, full OS image boot with this cache, matched cold/warm
compositor startup, multi-app interaction/frame-rate or app-RAM comparison was
run. The published-image emulated VM boot baseline remains a separate result;
the greeter is a separate cage/greeter process and this change does not directly
accelerate it. No devices were rebooted or deployed for this qualification.

## Reproduction and checks

Use the selected remote Cargo environment; do not compile during timing.
The runtime command below uses the existing native launcher and control client.

```sh
rcargo --no-fallback build --release --locked -p shell -p launcher -p tilo-ui \
  --features launcher/dev-overrides,shell/dev-overrides \
  --bin shell --bin tilo-launcher --example startup-frame-probe \
  --example startup_frame_timeline -j8
target/release/examples/startup_frame_timeline \
  --out target/production-mesa-seed --samples 8 --production-mesa-seed
```

The probe's kernel-observation correction was separately release-built with
only `--example startup-frame-probe`, retaining the selected packages and
features; all final samples use the recorded binary hashes. Generated archives
remain outside Git. Two earlier attempts are excluded: an observer tried to
read sandbox-denied proc status, and the wrong-driver fixture mapped multiple
namespaces to one name. Both were fixed without modifying production permissions.

Passing checks: launcher cache/sandbox regressions; real production
identity-startup integration test; real kernel parent-death test;
`shell check --tests`; shell/launcher/SDK Clippy with warnings denied; corrected
probe Clippy; optimized release builds; all eight ordinary launches; persistent
session restoration/regeneration; current release identity-verification check.

Mesa's [cache documentation](https://docs.mesa3d.org/envvars.html) and
[cache implementation](https://gitlab.freedesktop.org/mesa/mesa/-/blob/main/src/util/disk_cache.c)
describe combining a read-only FOZ archive with an ordinary writable cache.
[llvmpipe](https://docs.mesa3d.org/drivers/llvmpipe.html) uses LLVM runtime code
generation; these measurements do not establish equal gains on hardware GPUs.
