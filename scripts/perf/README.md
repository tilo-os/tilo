# perf scripts

Small, composable measurement + optimisation runners. Each script is
executable stand-alone; the higher-level flow is described below.

## Measure

- `shm_swizzle_bench` — a Cargo example calling the production SHM pixel
  conversion on reused storage, with pixel/capacity checks and raw timing
  samples. Run it in the benchmark environment with `cargo run --locked
  -p shell --release --example shm_swizzle_bench`. The
  [paired comparison](../../docs/shm-conversion-benchmark.md) records its scope,
  results and remaining system measurements.

- `shm_pipeline_bench` — a CPU fixture calling production conversion,
  frame-store publication and snapshot paths, with byte/capacity/sequence
  checks. The [frame-copy investigation](../../docs/shm-pipeline-benchmark.md)
  records the next measured target and its boundary with renderer work.

- `frame-loop-bench.py` — run the real compositor loop with cadence and
  input-to-present instrumentation. Its `--gate` mode is the quick desktop
  acceptance test: use `--refresh-hz 144 --input-hz 30 --launch-app
  dev.tilo.chat` to reject low frame throughput, laggy input, missing samples,
  or a native Chat launch that never presents a first frame. It consumes
  already-built binaries and an existing app store, so it does not build an
  RPM or image.

- `latency_bench.py` — the nightly `latency` KPI group: a headless compositor
  times 600 pointer moves (input to finished frame, from the frame trace) and
  100 workspace switches (the compositor's `WORKSPACE-SWITCH` lines: chord to
  first frame, to the resting frame, dropped intervals), and writes records.
  `device_latency_bench.py` is its weekly hardware counterpart (kernel input to
  KMS presentation, through `tilo-vm`); see
  [reference hardware](../../docs/benchmarks/reference-hardware.md).

- `binary-sizes.sh [target-dir]` — walk `target/release/` (or the
  given dir) for shell + terminal + file-explorer + tilo-relay + fs +
  tilo-install, emit a `{name: bytes}` JSON blob on stdout.

- `qemu-boot-measure.sh <qcow2> <label>` — headless QEMU boot of a
  qcow2 using qemu-system-x86_64 + OVMF. Set `TILO_DISTROBOX` when those
  tools are in a distrobox. Snapshots the base so consecutive runs are hermetic; polls
  the serial console for the multi-user.target reach; extracts the
  kernel timestamp on the reach line into a JSON `{state, boot_ms}`
  blob. Powers the guest down cleanly.

- `qemu-install-boot-verify.sh [--disk PATH]` — assert the bootloader
  on the produced qcow2's ESP is systemd-boot (not GRUB) and that the
  disk boots to `multi-user.target`. Reads the ESP with `qemu-img
  convert` + `sfdisk` + mtools (no NBD, no host mount) so it runs
  rootless everywhere; then boots a throwaway backing-file snapshot
  under OVMF and emits `boot_ms` in the same units as
  `qemu-boot-measure.sh`. Emits JSON on stdout for CI.

- `compare.sh <base.json> <cur.json>` — diff two runs (either output
  format) and print a percentage-change table.

- `agent_rtt.py (--socket PATH | --spawn SHELL)` — the agent round trip:
  actions list, state dump, a no-op invoke (accepted, and completed through
  `wait_ms`), throughput under eight clients and the no-op through the MCP
  server. Each is timed from the client's side of the control socket, after
  100 warm-up calls, with 1,000 samples. `--app PROVIDER:ID` adds an app
  action. With `--env-json` it writes `tools/perf/record.py` records.
  **Perf / scheduled** runs it nightly as the `agent` group.

- `agent_task.py (--socket PATH | --spawn SHELL [--seeder EXAMPLE])` — a
  whole agent task: a scripted model, streamed like the Messages API from a
  loopback fake, creates a note in Notes, writes its body, finds it by
  search and opens it, one tool call per turn over the control socket.
  Records the task's wall time, the time in Tilo, in the (fake) model and in
  the agent loop, the turns and each step; `--split-repetitions` also times
  each invoke's accept and apply apart. `--think-ms` and `--tokens-per-s`
  model a real provider; both default to 0. `--spawn` seeds a
  signed-in Notes desktop with the `agent_task_desktop` example of `shell`.
  It joins the `agent` group once a whole task settles: today a listing
  stops settling after the first new note in a hosted Notes tab.

## Optimise (three-step PGO + optional BOLT)

1. `pgo-instrument.sh` — build shell + terminal + file-explorer +
   tilo-install with `-C profile-generate` into `target/pgo-instrumented/`.
   Run the resulting binaries; they drop `.profraw` files into
   `target/pgo-profraw/`.
2. `pgo-merge.sh` — merge every `target/pgo-profraw/*.profraw` into
   `os/pgo/tilo.profdata`. **Commit that file** — pinning the profile
   bytes in-tree is what makes the second-phase build reproducible
   under the frozen RPM repo.
3. `pgo-rebuild.sh` — read `os/pgo/tilo.profdata` and rebuild with
   `-C profile-use`. Output lands at `target/pgo-release/`.
4. (optional) `bolt-optimize.sh` — post-link `llvm-bolt` on the PGO'd
   binaries using the same profile signal. Output at `target/bolted/`.

## Physical device Notes latency

Use the ordinary `tilo-vm` device and slot after applying a qualified release
component with `tilo-vm swap shell --slot device --builder remote`. Update Notes
through its signed release channel:

```sh
TILO_VM=yoga tilo-vm slot exec --slot device --timeout 180 -- app update so.tilo.notes
python3 scripts/perf/device_notes_latency.py --device yoga --slot device \
  --samples 100 --out target/yoga-notes-after-update
```

Open the updated Notes app through its normal application action before capture.
The device session must be unlocked, with one Notes window. Enable the greeter's
normal `configure-session-diagnostics@greeter` presentation flag before sign-in
for physical input and presentation evidence. For GPU diagnosis also enable
`gpu_stages`, `gpu_timestamps` and `gpu_dispatch_timestamps`; stage logging alone
does not enable GPU timestamp queries. See `docs/agent-action-api.md` for the
semantic configuration and normal sign-in actions.

The capture creates and opens a fresh scratch note. Before every kernel key it
checks the actual selected/bound page, adopted canonical and rendered hashes,
caret, editor focus and compositor keyboard routing. Existing notes receive no
benchmark input. The scratch note remains available for inspection afterward.

The output contains private raw logs, the exact input/frame/presentation timeline,
editor observations, scratch identity and `report.json`. Missing, discarded or
coalesced exact frames fail the capture; a partial run never produces a passing
latency summary. A small sample count is useful for diagnosis, but does not
establish a stable p99. Record the deployed component receipt, Notes release and
device/image identity with comparative runs. The clock starts at the original
kernel/libinput event and ends at hardware presentation; it excludes peripheral
scan time and panel pixel response.

To attribute a Linux capture to client CPU stages, use a runtime that emits
the native render timing rows, then analyze that same capture:

```sh
python3 scripts/perf/physical_notes_stages.py \
  --capture target/yoga-notes-after-update \
  --out target/yoga-notes-after-update/client-stages.json
```

The analyzer requires the verified changed Notes body and KMS presentation,
then joins render stages using the process, worker lifetime, CPU frame and
render worker identities. Missing render stages fail analysis. Older clients
without the optional post-draw main-thread stages report that absence explicitly.
The output is private and refuses to overwrite an existing report. These CPU
durations include scheduling and logging; they do not measure GPU execution or
certify performance.

## What's blocked

- Rebuilding the bootc qcow2 to measure the boot-time delta against
  the baseline requires pulling the digest-pinned frozen RPM repo
  (`registry.tilo.so/tilo/rpms`, anonymously readable). In CI the
  `.forgejo/workflows/build-image.yml` job rebuilds it; the rebuilt
  image + qcow2 flow through the release channel.

Measurements are workload- and host-dependent. Record the image revision,
CPU/memory/KVM settings, adapter, and `TILO_DISTROBOX` value with each result;
do not treat an old run as a repository baseline.

`tools/perf/record.py` writes that environment into every result as a record.
`tools/perf/compare.py` compares a record only with stored records from the same
environment. Scheduled results and their baselines are kept in the
`tilo-perf-results` package; see
[Performance results](../../DEVELOPMENT.md#performance-results). Use
`compare.sh` for a one-off diff of two local runs.
