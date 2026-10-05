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

## Run it in CI

`.forgejo/workflows/vm-acceptance.yml` (Dev / VM acceptance) runs the suite on a
`runner-disk-100g` runner, whose KVM the dev disk and legacy guest boots already use, so
a run needs no laptop memory:

```bash
fgj workflow run vm-acceptance.yml -r main                       # every check on main
fgj workflow run vm-acceptance.yml -r main -f ref=my-branch -f only=window -f gl=off
```

`fgj -f` splits values on commas, so a list (`only=B06,A13`, `swap=shell,app:notes`,
`outputs`) goes through the forge's "Run workflow" form or its dispatch API instead.

It also runs nightly on main, and on pull requests that change this directory: with only
the checks of the changed check modules (and of the modules importing from them), just
`provenance` when only this README or unit tests changed, and every check when anything
else changed (`ci_select.py`).

The base disk is what CI published for the newest main commit at or before `ref`: its
dev disk (`tilo-vm-disk/main-<sha7>-ci`) when a lane published one, else its dev image
(`tilo-vm:main-<sha7>-ci-x86_64`, pushed by Build / OS dev for main commits), installed
with `os/dev/build-image.sh` exactly as Release / Dev VM disk installs it. The workflow
writes the provenance record itself.

`ref`'s own code reaches the VM the way it does on a laptop: after the slot is up, run.py
`--swap` runs `tilo-vm swap <component> --builder host` from a worktree of `ref`
(`--swap-from`), session components and apps into the run's slot, VM-wide ones into `vm`.
`swap=default` is `shell` plus every `app:<dir>` the ref changed: a branch's own diff
from main, or for a main commit what changed since the base (nothing when `ref` is the
base commit), `all` every userspace component in
`os/components.toml`, `none` nothing, or a list. The job runs in the CI builder image (the
dev image's Fedora, with the runner's sccache), so swapped binaries link against the
libraries the VM has. P01 then reads `base <commit> + <components> swapped from <ref>` and
fails unless every requested swap is the layer its slot runs; the report's header lists
the swaps. A base more than 20 commits behind `ref` gets a warning at the top of the
summary: everything not swapped is the base's code.

The VM is a libvirt session VM, the way `tilo-vm` runs on a laptop: the job installs
libvirt's QEMU driver, passt, OVMF and Pillow (pixel checks), and runs `run.py` as an
unprivileged user (root cannot hand passt its socket in a session). The runners have no
GPU, so `gl=auto` creates the VM without virgl and the report records the software
renderer; frame-timing checks gated to real display hardware report that. The report
(`report.md`, `report.json`, shots, frame traces) is the run's artifact, `report.md` its
job summary, and the job fails unless every check passes.


### Startup and GPU (A14, R05)

A14 opens Files before anything else in the run does and times it from the app's own
startup marks (`<data home>/tilo-rooms/<room>/so.tilo.file-explorer/.local/state/startup-<N>.json`,
process start to `first-frame-presented`): `files_first_frame_s` (3 s) in a VM, which
renders on a CPU rasterizer or through virgl, `files_first_frame_hw_s` (300 ms) when
frames reach real display hardware. The compositor's request-to-present time is recorded
too when the session logs `tilo::launch_latency` at info. A frame is not a working
window, so A14 also walks Files' AT-SPI tree in the guest with the image's external
client (`/usr/libexec/tilo/atspi-external-client.py`) and requires its folder listing to
hold entries; it creates an `acc-files-*` folder in home first, so home is never empty.

R05 runs `tilo system health --gpu --json` in the guest and reads every app's `tilo-gpu:
window adapter` line from the session log. A machine without a hardware render node
cannot show a CPU fallback, so a plain software VM leaves R05 unobservable; `--gl` boots
virgl, where a degraded health report, a window on a `type=Cpu` adapter or a reported CPU
fallback fails it. `gpu_check.py` is the same judgement for gates that boot an installed
image without tilo-vm (the installed-desktop gate in `build-image.yml`):

```bash
./os/dev/vm.sh ssh "$(python3 scripts/vm-acceptance/gpu_check.py --probe)" \
  | python3 scripts/vm-acceptance/gpu_check.py
```

### Cold start of every app (installed-desktop gate)

Every app logs one `APP-STARTUP app=<pkg> total_ms=<ms> phases=<mark>:<ms>,...` line at
its first presented frame, and `HISTORY-OPEN listed=<n> earlier=<bool> ms=<ms>` when its
History opens. `app_startup_check.py` reads them from the session log, prints one row per
app (slowest first, with the phase that took longest) and fails when a launch's first
frame takes longer than its budget: 1 s, or 600 ms for small single-view apps
(`acceptance/app_startup.py`), and 1 s for opening History. A launch whose window
adapter is a CPU rasterizer (llvmpipe in a VM without a GPU) compiles vello's shaders and
renders on the CPU, so it is held to the VM checks' software budget, 3 s, instead. The
judgement prints every `APP-STARTUP` line it read, with all its phases. The installed-desktop gate in
`build-image.yml` runs it after its app checks, having typed a ~2000-character note on the
real keyboard and reopened Notes on it (`reopen-history`), so a note with thousands of
changes in its history is part of every judged run. Under TCG it reports without judging.

```bash
./os/dev/vm.sh ssh "$(python3 scripts/vm-acceptance/app_startup_check.py --probe)" \
  | python3 scripts/vm-acceptance/app_startup_check.py --require so.tilo.notes
```

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
| window | W01–W21: open/focus, move, resize, maximize, full screen, tabs, strips, workspaces, two outputs, overview, workspace wheel, touch, touchpad, animation pacing (frame trace), real keyboard typing, touch that never drives the cursor (a launchpad card tapped open, the cursor hidden until the mouse moves), the on-screen keyboard (shown for a tapped field, typing into it, hidden by the physical keyboard), the on-screen keyboard typing into the shell's own search field, unlocking the session by touch on the lock screen's own keyboard |
| ui | U01 theme and language through actions, U02/U03 Settings, quick settings, workspace settings, overview in light/dark/high-contrast (en, de) with layout checks on `list-targets` rects, U04 German action titles |
| apps | A01–A07: Terminal (live shell), Files, Settings, Notes, Store, a host Wayland app, an X11 app — first frame, placement, focus, exit on close, no false toasts. A08–A13, the apps used every day, one per toolkit and protocol path: Chromium on Wayland (A08, the engine Electron apps ship) and forced onto X11 (A09), GNOME Text Editor (A10, GTK4/libadwaita), FeatherPad (A11, Qt6 Wayland), alacritty (A12, winit/OpenGL), glxgears (A13, OpenGL X11 like games) — window in the current strip with focus, a first frame with content, the right buffer scale on both outputs as it moves between them, real-keyboard typing, copy to and paste from Tilo's clipboard, exit on close; A13 also full screen and back. A14 Files' first launch: process start to first presented frame within budget, and its folder listing read off its AT-SPI tree |
| search | S01–S09: empty-query view, English and German names, launch from search, windows, actions, files, room activity, workspace history |
| resources | R01/R02 idle CPU, wakeups and frames with no apps and with several, R03 memory per open app from its own cgroup (memory.current, anon/file) plus the compositor's anonymous RSS, R04 frame CPU p50/p95/p99 during interactions (wall time on real display hardware), R05 every app window renders on the machine's GPU, not a CPU fallback; the renderer and whether the display is real hardware are recorded |
| basics | B01 brightness, B02 Wi-Fi, B03 Bluetooth (unavailable without hardware, no fake controls), B04 de nodeadkeys + compose applied to the physical keys, B05 display arrangement, B06 hotplug, B07 GNOME keyboard import (host-desktop installs only, so unobservable in a VM) |

The everyday apps (A08–A13) come with the dev image (`os/Containerfile.dev`, about 600 MiB,
two thirds of it Chromium; the desktop entries for the Chromium variants and glxgears are in
`os/dev-image/`). A disk without them fails those checks as unobservable. The run's second
output is scaled by 1.25, so each app is checked at a fractional scale as well: Wayland clients
must draw at exactly their output's scale (a whole-number buffer on it means the client fell
back from `wp_fractional_scale_v1`), X11 clients at 1 or a whole scale, sized to their tab.
Text goes in on the real keyboard (see W17), and the clipboard is read and written through
the control socket's `clipboard_paste`/`clipboard_copy`, the in-process route Tilo's own apps
use; alacritty copies through OSC 52.

Budgets live in `run.py` (`BUDGETS`). The German pass (U03, U04, S03) runs after the
English one: the shell takes its language from the session's environment, so the run
sets the system locale the way the installer seeds it (`localectl set-locale
LANG=de_DE.UTF-8`, inherited through the session's PAM login) and restarts the slot;
if the restarted compositor is not German those checks fail as unobservable.

An app's processes (A01–A07) are the `tilo-launcher` the open started for that app, its
descendants, the sessions they lead (a host launch `setsid`s its app; Wine's prefix stays
in it) and the launcher's `app-tilo-<id>-<pid>.scope`. Session services the app woke up
— D-Bus-activated portals, socket-activated Tilo services, the workspace's Xwayland — are
children of the user manager or the compositor and never count as leftovers. A first
frame is sampled for up to 5 s until it is not a flat fill, so a clear presented before
the client's first real buffer does not fail it; a client that stays blank does. The pure
parts are unit-tested: `python3 -m unittest discover -s scripts/vm-acceptance -p 'test_*.py'`.

Frame checks (R04, W16) read the session's frame trace, one row per compositor loop
tick. The recorder writes its buffered rows out about once a second, on a tick, so every
window is cut by tick `seq` between two reads that first wake the loop through the
control socket until the buffer is on disk; a window never trusts a stale file.

They keep apart what the compositor controls and what the display does. A transition
(overview open and close, workspace switch and back) runs from the first tick that has a
frame to show until the compositor has asked for none for 500 ms. Judged everywhere:
frames painted (a transition held while the presenter is full paints every step, however
slowly the display takes them), lateness of each frame against the compositor's own
deadline, CPU time of each loop tick (`cpu_us`, the thread's CPU clock, so time the
thread was descheduled or blocked is not counted as work), and input to first frame
with time spent waiting on a full presenter taken out. Painted-to-painted intervals and
wall time per frame are the display's and the host's as much as the compositor's: they
are always recorded, and judged only when frames reach real display hardware (the run's
`display` fact, from the DRM drivers). In a VM, QEMU retires frames on its own display
timer (tens of milliseconds, seconds once it has idled), so those intervals say little
about Tilo. A virtual display that takes no frame of a transition for 20 s makes it
unobservable, not passed. The trace reading is unit-tested beside the app checks (`test_frames.py`).

Touchpad gestures (W15) are driven through the control socket's `swipe` and `pinch`, which
feed the shell's gesture tracker; libinput's own touchpad decoding needs a uinput touchpad
`tilo-vm input` does not have yet. Not yet drivable, so failing as unobservable: output
hotplug (B06; a slot's outputs are fixed at creation).
