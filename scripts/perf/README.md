# perf scripts

Small, composable measurement + optimisation runners. Each script is
executable stand-alone; the higher-level flow is described below.

## Measure

- `frame-loop-bench.py` — run the real compositor loop with cadence and
  input-to-present instrumentation. Its `--check` mode is the quick desktop
  acceptance test: use `--refresh-hz 144 --input-hz 30 --launch-app
  dev.tilo.chat` to reject low frame throughput, laggy input, missing samples,
  or a native Chat launch whose window never draws a first frame (`frames` in
  the session's `windows` rows). It consumes an already-built `tilo-session`
  (a development build, so its test-control socket opens) and an existing app
  store, so it does not build an RPM or image. `--load windowed|fullscreen`
  runs `tilo-loadclient` (`tools/click-test-client`, built with `cargo
  build --release -p click-test-client --bin tilo-loadclient`) beside it: a
  window that draws a frame for every frame shown. `--load-client PATH` names
  another one.

- `headless_key_latency.py` — key-to-present latency of typing on the
  tree's headless desktop, from the session's own `KEY-LATENCY` lines
  (key -> client commit -> compositor submit -> the vblank that shows it).
  It types into the synthetic client that answers each key with one commit
  (`crates/session/session/examples/latency_client.rs`), or into an installed
  app with `--app so.tilo.tasks` / a tilolang app's id, opens the session
  through `scripts/headless-examples/real_apps.py`, and reports p50/p90/p99
  per leg. It leaves out the kernel input stack and the panel, so it measures
  the compositor, the client and the GPU.

- `device_latency_bench.py` — the weekly, report-only latency run on a
  reference machine (kernel input event to KMS presentation, through
  `tilo-vm`): pointer and workspace-switch KPIs per refresh rate; see
  [reference hardware](../../docs/benchmarks/reference-hardware.md).

- `drm_hardware_checks.py --device yoga --slot device --commit C
  --image-digest D --out DIR` — the DRM presentation checks that need a real
  GPU: an output scanned out through atomic page flips and never demoted, no
  lost-flip line, frames paced to the vblank and presented within one
  interval, and a scanned-out client's buffers released within one interval.
  Each reads the compositor's own session log and fails when the line is
  missing, never skips; what needs a person at the panel (tearing, a flash on
  the frame an overlay appears, a VT switch) is printed as such and never
  reported as passed. Writes one `drm.presentation-checks` record per run and
  the log it read.

- Boot time of a qcow2 under QEMU + OVMF, from its serial console:

  ```bash
  tilo-vm qemu run --disk DISK.qcow2 --overlay --display none --memory 2048 --smp 2 \
    --ready 'Reached target [^-]*(multi-user\.target|graphical\.target)' \
    --fail 'Kernel panic|Failed to load kernel' --kernel-time --timeout 180 \
    --serial-log target/perf/LABEL.console.log --report target/perf/LABEL.json
  ```

  Boots a throwaway child of the disk so consecutive runs are hermetic, and
  powers the guest down through the monitor afterwards. The report's
  `kernel_ms` is the kernel timestamp on the line that reached the target; a
  boot without one fails (`missing-timing`) rather than report a made-up time.

- `qemu-install-boot-verify.sh [--disk PATH]` — assert the bootloader
  on the produced qcow2's ESP is systemd-boot (not GRUB) and that the
  disk boots to `multi-user.target`. Reads the ESP with `qemu-img
  convert` + `sfdisk` + mtools (no NBD, no host mount) so it runs
  rootless everywhere; then boots a throwaway backing-file snapshot
  under OVMF (`tilo-vm qemu boot`) and emits `boot_ms`, the kernel
  timestamp `tilo-vm qemu run --kernel-time` reports. Emits JSON on stdout
  for CI.

- `agent_rtt.py (--socket PATH | --spawn SHELL)` — the agent round trip:
  actions list, state dump, a no-op invoke (accepted, and completed through
  `wait_ms`) and throughput under eight clients. Each is timed from the
  client's side of the control socket, after
  100 warm-up calls, with 1,000 samples. `--app PROVIDER:ID` adds an app
  action. With `--env-json` it writes `tools/perf/record.py` records. A
  spawned session starts on a shell, the throwaway identity a development
  login opens it with minted first. The performance run runs it as the
  `agent` group.

## Optimise (three-step PGO + optional BOLT)

1. `pgo-instrument.sh` — build tilo-session + terminal + file-explorer +
   tilo-install with `-C profile-generate` into `target/pgo-instrumented/`.
   Run the resulting binaries with the `LLVM_PROFILE_FILE` export the script
   prints; they drop `.profraw` files into `target/pgo-profraw/`.
2. `pgo-merge.sh` — merge every `target/pgo-profraw/*.profraw` into
   `os/pgo/tilo.profdata`. **Commit that file** — pinning the profile
   bytes in-tree is what makes the second-phase build reproducible
   under the frozen RPM repo.
3. `pgo-rebuild.sh` — read `os/pgo/tilo.profdata` and rebuild with
   `-C profile-use`. Output lands at `target/pgo-release/`.
4. (optional) `bolt-optimize.sh` — post-link `llvm-bolt` on the PGO'd
   binaries using the same profile signal. Output at `target/bolted/`.

## What's blocked

- Rebuilding the bootc qcow2 to measure the boot-time delta against
  the baseline requires pulling the digest-pinned frozen RPM repo
  (`registry.tilo.so/tilo/rpms`, anonymously readable).

Measurements are workload- and host-dependent. Record the image revision,
CPU/memory/KVM settings, adapter, and `TILO_DISTROBOX` value with each result;
do not treat an old run as a repository baseline.

`tools/perf/record.py` writes that environment into every result as a record.
`tools/perf/compare.py` compares a record only with stored records from the same
environment. Scheduled results and their baselines are kept in the
`tilo-perf-results` package; see
[Performance results](../../docs/measurements/performance.md).
