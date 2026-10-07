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

Root/operator prerequisite: the paired userspace delivery must already be verified,
the owner session unlocked, and one freshly launched Notes worker must have inherited
presentation diagnostics. Live compositor configuration cannot retrofit an existing
app observer or GPU timestamp queries. Record the same boot, active owner, exact
loaded app/SDK hashes and delivery receipt before input. Do not use an existing user
note as the benchmark target. No reboot is part of this recipe.

Use the maintained host frontend from the qualified cohort. First run a single-key
readiness guard, then a separate 100-key capture only if that guard succeeds:

```sh
python3 scripts/perf/device_notes_latency.py --device yoga --slot device \
  --tool /absolute/path/to/qualified/tilo-vm --manage-diagnostics \
  --samples 1 --out target/yoga-notes-one-key
python3 scripts/perf/physical_notes_stages.py \
  --capture target/yoga-notes-one-key --require-foreign-capture \
  --out target/yoga-notes-one-key/stages.json
python3 scripts/perf/device_notes_latency.py --device yoga --slot device \
  --tool /absolute/path/to/qualified/tilo-vm --manage-diagnostics \
  --samples 100 --out target/yoga-notes-100
python3 scripts/perf/physical_notes_stages.py \
  --capture target/yoga-notes-100 --require-foreign-capture \
  --out target/yoga-notes-100/stages.json
```

Run these sequentially; never proceed after a failed command. Both captures create
and verify their own new scratch note, invoke ordinary exact-owned Open (which
closes the narrow drawer), then verify the actual child focus, all page bindings,
canonical/rendered hashes, caret and unique compositor keyboard route. No generic
Focus action on an unidentified old editor is used. Each isolated `a` produces a
unique trusted input/Wayland serial and changed-body frame; uniqueness concerns
input identities, not 100 different printable characters. Missing fields or a
first key delivered with an unchanged body fail before any second key.

`--manage-diagnostics` saves the original four booleans, enables only existing live
presentation/CPU phase observation, and renews its bounded quota before each key,
after the preceding family has passed body and presentation checks. These semantic
configuration actions do not inject keys or create physical input IDs. They occur
outside the measured interval. GPU query booleans remain unchanged. The scope
restores all four options in `finally`, including cancellation and capture errors;
if restoration itself fails, the run fails and `diagnostics-original.json` supplies
the exact options for normal owner recovery. Only successful restoration produces
`diagnostics-restored.json`. The scratch notes remain for inspection; existing
notes receive no input or cleanup edits.

Require exactly 100 adopted, uniquely joined samples before assessing the target:
**kernel/libinput key → exact changed Notes body KMS presentation p50 <20ms and
p99 <50ms**. A one-key success is readiness only. `report.json` retains raw latency;
`stages.json` additionally rejects missing/duplicate physical outcome ownership,
omitted capture pixels and incomplete exact token/sequence/presenter joins. Do not
certify a capture whose attribution analysis fails. Unavailable/skipped fine GPU
samples remain unavailable and do not invalidate the independent CPU/KMS evidence.
After both analyzers succeed, the measured target can be checked without rounding:

```sh
python3 - <<'CHECK'
import json
from pathlib import Path
root = Path("target/yoga-notes-100")
report = json.loads((root / "report.json").read_text())
stages = json.loads((root / "stages.json").read_text())
assert report["latency_complete"] is True
assert len(report["samples"]) == len(stages["frames"]) == 100
assert report["latency"]["p50_ms"] < 20
assert report["latency"]["p99_ms"] < 50
print("100 exact samples meet the measured 20/50ms target")
CHECK
```

CPU submit, render, wait and presentation intervals overlap; never add all stage
columns. Diagnostics overhead remains part of the measured run, and KMS timing
does not measure panel pixel response. Raw captures and observations are private
files. Gen58's delivered key with unchanged body is a failed readiness capture,
not a latency result.

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
