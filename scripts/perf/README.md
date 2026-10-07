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

On Yoga use the existing authenticated `OwnerVm` adapter and a freshly created,
explicitly pinned scratch **window record**. The default `tilo-vm control` agent
endpoint is not an owner endpoint; never relabel it or select a user Notes record
by application alone. The operator's existing private `scratch-capture.py`
`PinnedOwnerDevice` validates the exact app/room/record, tab, surface and host local
record before/after actions and uses the canonical owner socket. Preserve that
adapter and its protected-record checks. The maintained runner is reusable with
it directly (importing the private helper performs no actions):

```sh
# OWNER_CAPTURE is the existing audited private scratch-capture.py; TOOL is
# the matching qualified maintained frontend. No credentials are arguments.
# PERF_DIR is the absolute scripts/perf directory of this qualified source.
python3 - "$OWNER_CAPTURE" "$PERF_DIR" "$TOOL" target/yoga-notes-one-key 1 <<'CAPTURE'
import argparse, json, runpy, sys
from pathlib import Path
sys.path.insert(0, sys.argv[2])
from device_notes_latency import observed_capture, private_write
helper = runpy.run_path(sys.argv[1])
out = Path(sys.argv[4]); out.mkdir(mode=0o700, parents=True, exist_ok=False)
options = argparse.Namespace(device="yoga", slot="device", tool=sys.argv[3],
                             out=out, samples=int(sys.argv[5]), manage_diagnostics=True)
vm = helper["OwnerVm"](options.tool, "yoga", "device", out / "commands.log")
vm.unlock()  # existing approved private owner conversation, no bypass
owned = helper["PinnedOwnerDevice"](options, vm, out)
owned.pin(owned.control(["actions", "list", "so.tilo.notes", "--json"])["actions"])
owned.focus_on_scratch_read = True
owned.reset_on_input = False  # observed_capture alone owns quota renewal/restoration
report = observed_capture(owned, options)
private_write(out / "report.json", json.dumps(report, indent=2))
CAPTURE
python3 scripts/perf/physical_notes_stages.py \
  --capture target/yoga-notes-one-key --require-foreign-capture \
  --out target/yoga-notes-one-key/stages.json
```

Only after both commands pass, repeat the exact owner-adapter command with
`target/yoga-notes-100 100`, then analyze that directory with
`physical_notes_stages.py --require-foreign-capture`. The pinned adapter must
still target the newly owned Scratch window, never an existing user record.
For other authorized endpoints, the standalone runner now requires explicit
`--notes-room` and `--notes-record` and invokes the exact record's declared Read;
it refuses application-only selection. Those selectors do not confer owner
permission, and transport refusal must remain a failure.

Run these sequentially; never proceed after a failed command. Both captures create
and verify their own new scratch note, invoke ordinary exact-owned Open (which
closes the narrow drawer), then verify the actual child focus, all page bindings,
canonical/rendered hashes, caret and unique compositor keyboard route. Only after the new title/body and closed-drawer editor state pass does the owner
adapter invoke the exact declared tab focus; no generic Focus action on an
unidentified old editor is used. Each isolated `a` produces a
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


### Owned Settings search navigation

`settings_effects.settings_search_navigation(vm, bound, scope, call)` exercises
Ctrl+K, the fixed `keyboard` query and Enter on a fresh pinned Settings window.
Use the maintained `OwnerVm` from `scripts/vm-acceptance/store-chooser.py`;
`bound` is the actual authenticated `(application, room, record)` of the fresh
operator-owned window, not an application-only guess. Prepare and focus only
that tab. `scope` must revalidate its tab/surface/native lifetime, verified
selected runtime, unlocked owner and preservation of earlier user tabs before
every operation. Reuse the maintained tracked job adapter for `call`:

```python
from acceptance.action_jobs import invoke
from settings_effects import settings_search_navigation
result = settings_search_navigation(
    owner_vm, bound, pinned_scope,
    lambda row, args, name: invoke(owner_vm, row, args, private_out, name))
```

The runner requires the exact declared action identity exposed by the palette's
`selected_action` before Enter. It reads the bound `settings.navigation`
`inspect-navigation` capability to verify the requested mounted page. It does
not close or select existing user Settings windows. Keep operator receipts in a
private directory. Missing/replaced scope or ambiguous actions refuse input.

This is navigation evidence only: `latency_available` remains false. Settings
hover and page changes do not yet have the trusted changed-state/frame-to-KMS
attribution needed for physical latency percentiles; CLI RTT and polling time
are not substitutes. The p50 <20ms / p99 <50ms goal remains unverified.

### Native Settings key-to-pane presentation evidence

`input_latency.summarize_physical_settings_latency(lines, expected_pane)` joins
existing physical input, drawn frame and standard Wayland presentation rows.
Use only a fresh pinned, runtime-verified owner Settings worker, as for the
navigation recipe above. Focus an actual sidebar destination before the measured
key; activation must use its ordinary native keyboard route. Collect the existing
content-free opt-in physical trace and actual compositor delivery/presentation
rows. The expected destination is an explicit canonical pane slug, never a label.

The analyser requires an admitted native key with trusted original libinput time,
one worker lifetime, exact serial/frame ownership, an intended changed pane, and
agreement between current mounted navigation and the navigation cached after the
actual NavShell draw. Positive KMS feedback must belong to that submitted frame.
It rejects undrawn/pending/hidden/wrong/unchanged panes and missing origins or
feedback. Renderer/presentation stages overlap; do not add their durations.

Ctrl+K palette activation is compositor-consumed and does not deliver that key to
the native Settings worker. This native analyser cannot qualify it. Pointer
motion has no immutable native Wayland origin stamp yet, so neither native hover
nor compositor palette hover is qualified by this increment. Those require their
own exact origin and actually drawn state witness. No synthetic event, nearest
timestamp, CLI RTT or headless GPU test is physical latency certification. No
Yoga measurement is included here; opt-in diagnostic overhead remains a limit.
