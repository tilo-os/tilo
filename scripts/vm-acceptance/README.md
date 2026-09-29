# VM acceptance suite

A scripted acceptance run of the desktop essentials in a real KVM session: a headful
`tilo-vm` slot on two outputs with a test identity, driven only through `tilo-vm` — the
control socket's semantic ops first (`actions`, `state-dump`, `list-targets`), uinput
input (`tilo-vm input`) where the input path is what is under test. Every run writes
`report.json` and `report.md` with the commit, the booted image digest, pass/fail per
check, numbers and evidence (shots, frame traces).

A check that cannot observe its subject fails as **unobservable**; nothing is skipped.

## Run it

The acceptance run must exercise the commit that is installed, so start from a disk built
from that commit:

```bash
# once per commit: image and disk are built on the rcargo builder, only the qcow2 is copied
TILO_DISK_BUILDER=fedora@builder TILO_DISK_SCRATCH=/srv/rcargo/scratch/devdisk-x86 \
  scripts/vm-acceptance/build-disk.sh origin/main ~/vm-images/tilo-dev-$(git rev-parse --short=9 origin/main).qcow2

# a throwaway VM from that disk, every check, VM stopped and removed afterwards
scripts/vm-acceptance/run.py --disk ~/vm-images/tilo-dev-<sha>.qcow2 --commit origin/main
```

`--vm NAME` runs against an existing VM instead (`--keep` leaves it running), `--only
window,A03` narrows the run to areas or check ids (plus the checks they depend on), `--gl`
creates the VM with virgl. The report lands in `target/vm-acceptance/<stamp>-<sha>/`
unless `--out` says otherwise. The exit status is 0 only when every check passes.

The runner refuses to start while another VM is running or when the host lacks the
guest's memory plus 1 GiB of headroom, and always stops the VM it used.

## Provenance

A dev image does not carry the commit it was built from, so `build-disk.sh` writes
`<disk>.provenance.json` next to the qcow2:

```json
{"commit": "<sha>", "subject": "…", "commit_time": "…", "oci_digest": "sha256:…",
 "built_at": "…", "built_by": "scripts/vm-acceptance/build-disk.sh"}
```

The first run pins the booted image digest (`bootc status`) into the record; later runs
fail P01 if the VM booted anything else, was moved onto another image since, or if
`--commit` names a different commit.

## What it checks

| Area | Checks |
|---|---|
| provenance | P01 image ↔ commit, P02 headful slot on two outputs with an identity, P03 no failed or restart-looping session units |
| window | W01–W17: open/focus, move, resize, maximize, full screen, tabs, strips, workspaces, two outputs, overview, workspace wheel, touch, touchpad, animation pacing (frame trace), real keyboard typing |
| ui | U01 theme and language through actions, U02/U03 Settings, quick settings, workspace settings, overview in light/dark/high-contrast (en, de) with layout checks on `list-targets` rects, U04 German action titles |
| apps | A01–A07: Terminal (live shell), Files, Settings, Notes, Store, a host Wayland app, an X11 app — first frame, placement, focus, exit on close, no false toasts |
| search | S01–S09: empty-query view, English and German names, launch from search, windows, actions, files, room activity, workspace history |
| resources | R01/R02 idle CPU, wakeups and frames with no apps and with several, R03 RSS/PSS, R04 frame work p50/p95/p99 during interactions; the renderer is recorded |
| basics | B01 brightness, B02 Wi-Fi, B03 Bluetooth (unavailable without hardware, no fake controls), B04 GNOME keyboard import (de nodeadkeys + compose), B05 display arrangement, B06 hotplug |

Budgets live in `run.py` (`BUDGETS`). The German pass needs `tilo-vm slot create
--locale`; without it U03, U04 and S03 fail as unobservable.
