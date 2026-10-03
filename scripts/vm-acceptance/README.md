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
# once per commit: image and disk are built on a remote builder, only the qcow2 is copied;
# TILO_DISK_CARGO is any cargo drop-in that runs `cargo run` there (see build-disk.sh)
TILO_DISK_BUILDER=fedora@builder TILO_DISK_CARGO="<drop-in> --on-builder" \
TILO_DISK_SCRATCH=/srv/scratch/devdisk-x86 TILO_DISK_MIRRORS='/srv/<drop-in>/p/*/w/*' \
  scripts/vm-acceptance/build-disk.sh origin/main ~/vm-images/tilo-dev-$(git rev-parse --short=9 origin/main).qcow2

# a throwaway VM from that disk, every check, VM stopped and removed afterwards
scripts/vm-acceptance/run.py --disk ~/vm-images/tilo-dev-<sha>.qcow2 --commit origin/main
```

Each disk costs its full ~15 GiB, so once a build finishes, `build-disk.sh` deletes the older
disks it built in the same directory, keeping the newest three (`TILO_DISK_KEEP=N`, or `all`
to keep every one). A disk a registered VM uses as its image or backing file, any other
qcow2's backing file, or one a process holds open is never deleted, nor is a disk without
`build-disk.sh`'s provenance record. To prune without building:

```bash
scripts/vm-acceptance/prune_disks.py --dry-run ~/vm-images   # then without --dry-run
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
| basics | B01 brightness, B02 Wi-Fi, B03 Bluetooth (unavailable without hardware, no fake controls), B04 de nodeadkeys + compose applied to the physical keys, B05 display arrangement, B06 hotplug, B07 GNOME keyboard import (host-desktop installs only, so unobservable in a VM) |

Budgets live in `run.py` (`BUDGETS`). The German pass (U03, U04, S03) runs after the
English one: the shell takes its language from the session's environment, so the run
sets the system locale the way the installer seeds it (`localectl set-locale
LANG=de_DE.UTF-8`, inherited through the session's PAM login) and restarts the slot;
if the restarted compositor is not German those checks fail as unobservable.

Animation pacing (W16) is read from the session's frame trace per transition: a burst
starts at the first painted frame after an idle park and spans the transition's length,
and its painted-to-painted intervals are held to the budgets, so the park itself never
counts as a stall.

Touchpad gestures (W15) are driven through the control socket's `swipe` and `pinch`, which
feed the shell's gesture tracker; libinput's own touchpad decoding needs a uinput touchpad
`tilo-vm input` does not have yet. Not yet drivable, so failing as unobservable: output
hotplug (B06; a slot's outputs are fixed at creation).
