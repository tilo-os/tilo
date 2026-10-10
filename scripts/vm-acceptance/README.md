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

### Installed-desktop gates (R06, R07, A15, A16)

The checks `build-image.yml` runs on an installed image, which per-push dev builds skip,
run here on every full run instead, on the dev disk under KVM:

- R06 runs the image's boot gate (`/usr/libexec/tilo/boot-performance-gate.sh`, from
  `os/tests/boot_performance_gate.sh`) as root: kernel and userspace to greetd's login
  endpoint within `boot_to_greeter_s` (20 s), with the image's boot contract. It reads the
  VM's boot timestamps, so where it runs in the order does not matter.
- R07 runs `tilo-session-report --render-gate` in the slot's session with the KVM
  budgets (`render_frame_ms` 1000, `render_frame_work_ms` 100).
- A15 opens Files, Notes and Settings once unmeasured and then ten times each. Every
  open is requested from inside the session (`date +%s%3N; tilo-control actions invoke
  applications <app>`) and timed to the app's own `first-frame-presented` startup mark,
  so the time holds the compositor's dispatch, the launcher and the sandbox as well as
  the app's start; it waits for the app's processes to exit before the next open. The
  median is held to the app's budget on the session's renderer (`app_startup.budget_ms`);
  on a GPU the p95 is also held to the KPI document's target (`tools/perf/kpi-budgets.toml`).
  Each open is split into segments that add up to it (`open_segments`): request to the
  launcher's start, the launcher's own phases (the marks it hands the app in
  `TILO_STARTUP_LAUNCHER_MARKS`), the launcher's hand-off to the app's `main` (bubblewrap,
  the Landlock shim and the dynamic loader: the shim marks `sandbox-entered` and
  `sandbox-exec`, so `exec_to_main` is the loader alone) and `main` to the first frame.
  The compositor's
  `tilo::launch_latency` lines, which it logs whatever `RUST_LOG` says, add its own
  request→spawn→map→first-present split. An open without either is unobservable.
- A16 is `app_startup_check.py`'s judgement over every app the run launched.

The greeter keystroke check (`os/dev/greeter-acceptance.py`) needs QEMU's monitor, which
a `tilo-vm` VM does not expose, so it stays in `build-image.yml`.

### Performance records

`run.py --records FILE` writes one record per KPI (`tools/perf/record.py`) from what the
checks measured, whether they passed or not; the CI run's `perf` job compares them with
the store (DEVELOPMENT.md, "Performance results") and publishes the nightly run's.

| KPI | Unit | Check | What |
|---|---|---|---|
| `boot.to_greeter_s`, `boot.{kernel,initrd,userspace}_s` | s | R06 | kernel+userspace to a usable greeter, and its phases |
| `idle.shell_cpu_pct`, `idle.session_cpu_pct` | percent | R01 | compositor and whole session CPU over the idle window |
| `idle.gpu_driver_cpu_pct` | percent | R01 | the part of `idle.shell_cpu_pct` on threads the GPU driver started (`llvmpipe-N`, and `tilo-gpu-driver`, the name the compositor gives threads started while it opens the device). On lavapipe this is the window's frames rasterised, a GPU's work on real hardware, so R01 and R02 judge a software renderer's compositor CPU without it; with no frame painted it must stay under `idle_software_raster_cpu_pct` (TIL-739) |
| `idle.wakeups_per_s.shell`, `idle.loop_ticks_per_s` | per_s | R01 | context switches of the compositor's loop thread; loop ticks |
| `idle.wakeups_per_s.shell_threads` | per_s | R01 | context switches of every thread of the compositor process |
| `idle.frames` | count | R01 | frames painted while still |
| `idle.ram_mib` | MiB | R01 | guest `MemTotal - MemAvailable` with no app open |
| `idle.ram_mib.{anon,shmem,kernel}` | MiB | R01 | `/proc/meminfo` AnonPages, Shmem and unreclaimable kernel memory (SUnreclaim, KernelStack, PageTables, SecPageTables, VmallocUsed, Percpu) |
| `idle.ram_mib.harness`, `idle.ram_mib.installed` | MiB | R01 | anon + shmem + kernel of what only the dev VM runs (seat0's greeter, the dev image's sshd, tilo-device-admin and tilo-vmd units, root's ssh sessions); `idle.ram_mib` less it |
| `memory.idle_mib.<bucket>`, `memory.idle_mib.unattributed` | MiB | R01 | anon + shmem + kernel per bucket of cgroups (`compositor`, `apps`, `session_services`, `system`, `greeter`, `harness`); the guest's that no process's cgroup is charged |
| `idle.ram_mib.{reserve,untracked}` | MiB | R01 | what MemAvailable withholds (the zones' reserve from `/proc/zoneinfo`, and min(half, low watermarks) of the file LRU and reclaimable kernel memory); the rest, pages no meminfo counter names |
| `memory.idle_anon_mib.<process>` | MiB | R01 | anonymous RSS of the ten largest process names, every user's |
| `idle.*.four_apps`, `memory.idle_*.four_apps` | | R02 | the same with Terminal, Files, Settings and Notes open |
| `idle.ram_mib.after_close`, `idle.ram_mib.*.after_close`, `memory.idle_*.after_close` | MiB | R03 | the same once R03 has closed the four apps |
| `memory.shell_anon_mib.per_open_close` | MiB | A15 | least-squares growth of the compositor's anonymous RSS per warm open and close |
| `memory.shell_anon_mib`, `memory.session_mib`, `memory.app_mib.<app>`, `memory.app_anon_mib.<app>` | MiB | R03 | the compositor's anonymous RSS; the compositor's and apps' cgroups |
| `disk.runtime_mib.per_user`, `disk.runtime_builds.per_user`, `disk.app_store_mib.per_user` | MiB, count | R03 | the slot user's downloaded runtime builds (`$XDG_DATA_HOME/tilo-runtime`) and app store (`$XDG_DATA_HOME/tilo-apps`) after the apps closed: what collection after each change leaves |
| `memory.shell_anon_mib.empty` | MiB | R01 | the compositor's anonymous RSS with no app open |
| `memory.compositor_memfd_mib{,.four_apps,.after_close}` | MiB | R01, R03 | shmem allocated to the memfds and `/dev/shm` files the compositor holds open, mapped or not (`stat -L` through `/proc/PID/fd`); `R0x-shell-smaps*.json` `shm_by_name` splits it by memfd name and `shm_fds` lists every memfd, dma-buf and DRM fd with its fdinfo (TIL-731) |
| `app.open.warm_ms.<app>` | ms | A15 | open request to first presented frame, ten warm opens |
| `app.startup_ms.<app>`, `app.startup_ms.file-explorer.first_in_session` | ms | A15, A14 | process start to first presented frame |
| `app.open.{request_to_launcher,launcher,sandbox_to_main,exec_to_main}_ms.<app>` | ms | A15 | the open before the app's `main`, by segment |
| `app.open.launcher_phase_ms.<app>.<mark>` | ms | A15 | each launcher phase, the interval ending at its mark |
| `app.open.{request_to_spawn,spawn_to_map,map_to_first_present,request_to_first_present}_ms.<app>` | ms | A15 | the compositor's own split of the open |
| `render.frame_cpu_ms` | ms | R04 | compositor CPU per painted frame during interactions |

`idle.ram_mib` stays the guest-wide figure the KPI document defines. Beside it, R01, R02
and R03 write `R0x-footprint*.json` (`resources.footprint`): every user's processes by RSS
split into anon, file and shmem (Tilo's kernel has no smaps, so there is no PSS), every
cgroup's anon, file, shmem and kernel charge grouped into the buckets above, each tmpfs's
use, and the guest's meminfo and zoneinfo split. A tilo-vm guest runs seat0's greeter
beside the slot, which an installed machine ends at login, so `idle.ram_mib.installed` is
the number to hold against the target.

The records name the commit whose code ran (the swapped ref's, else the disk's) and keep
the base disk's commit, the swapped components and the booted image digest in `meta`.

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
| window | W01–W21: open/focus, move, resize, maximize, full screen, tabs, strips, workspaces, two outputs, overview, workspace wheel, touch, touchpad, animation pacing (frame trace), real keyboard typing, touch that never drives the cursor (a launchpad card tapped open, the cursor hidden until the mouse moves), the on-screen keyboard (shown for a tapped field, typing into it, hidden by the physical keyboard), the on-screen keyboard typing into the shell's own search field, unlocking the session by touch on the lock screen's own keyboard (and the lock refusing a control-socket snapshot) |
| ui | U01 theme and language through actions, U02/U03 Settings, quick settings, workspace settings, overview in light/dark/high-contrast (en, de) with layout checks on `list-targets` rects, U04 German action titles |
| apps | A01–A07: Terminal (live shell), Files, Settings, Notes, Store, a host Wayland app, an X11 app — first frame, placement, focus, exit on close, no false toasts. A08–A13, the apps used every day, one per toolkit and protocol path: Chromium on Wayland (A08, the engine Electron apps ship) and forced onto X11 (A09), GNOME Text Editor (A10, GTK4/libadwaita), FeatherPad (A11, Qt6 Wayland), alacritty (A12, winit/OpenGL), glxgears (A13, OpenGL X11 like games) — window in the current strip with focus, a first frame with content, the right buffer scale on both outputs as it moves between them, real-keyboard typing, copy to and paste from Tilo's clipboard, exit on close; A13 also full screen and back. A14 Files' first launch: process start to first presented frame within budget, and its folder listing read off its AT-SPI tree. A15 ten warm opens each of Files, Notes and Settings, request to first frame; A16 every launch of the run within its app's budget |
| search | S01–S09: empty-query view, English and German names, launch from search, windows, actions, files, room activity, workspace history |
| resources | R01/R02 idle CPU, wakeups and frames with no apps and with several (`R01-idle-empty.json`/`R02-idle-apps.json` carry CPU and context switches per thread of the compositor, `shell_threads` and `shell_thread_groups`, and of any other process above 0.5%, plus the compositor's age when the window opened, `session_age_s`; the compositor is the slot user's `shell` with no `shell` above it, so its own shader-seed child is never taken for it; an over-budget compositor names its busiest threads), R03 memory per open app from its own cgroup (memory.current, anon/file) plus the compositor's anonymous RSS; R01 and R03 also write the compositor's `smaps_rollup`, status lines and 20 largest anonymous mappings (`R01-shell-smaps.json`/`R03-shell-smaps.json`), so R03 less R01 is what four windows cost and a growing `[heap]` or run of glibc arenas reads apart from one GPU buffer, R04 frame CPU p50/p95/p99 during interactions (wall time on real display hardware), R05 every app window renders on the machine's GPU, not a CPU fallback; the renderer and whether the display is real hardware are recorded; R06 boot to a usable greeter, R07 the render-path gate |
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

Every check runs in the one session the run shares, so every check must leave it unlocked:
while locked the compositor refuses every action invocation and snapshot with
`session_locked` (on purpose; W21 asserts the snapshot refusal), and each later check would
report that instead of its subject. The harness enforces it: after every check `run_check`
reads the state dump, and a check that ended with the session locked fails ("left the
session locked") and the slot's session is restarted (`tilo-vm slot restart`) before the next
one; the report's `session_recovered_after` lists those checks. W21, the one check that locks
on purpose, unlocks in a `finally` the way a person would (the passcode on the real
keyboard), falls back to the restart, and records the route in its `unlock_recovery` metric.
Nothing goes around the lock. The guard is unit-tested in `test_lock_guard.py`.

For the same reason no check may leave a modal, overlay, prompt or dialog open: each takes
the keyboard from the window behind it, and the next check's typing lands in it (run 89618's
A01 typed its Terminal command into the Invite composer W20's first touch had opened on the
launchpad). After the lock guard, `run_check` reads the state dump again (`core.open_surfaces`:
the `overlays`, search, the window switcher, the Invite composer and Join card in `peer_sync`,
`portal_dialog`, pending permission prompts, the workspace-name editor, the `input_capture`
surfaces, and any other surface for which `input_capture.chrome` says the shell holds input,
pointer hover over a maximized window's controls aside). A check that ended with one open
fails ("left a dialog open (…)"), and the harness closes it the way a person cancels: Escape,
then a slot session restart if Escape does not do it. Nothing is ever confirmed to clean up. The
report's `dialog_recovered_after` lists each such check, what was open and how it was closed.
`ctx.reset()` dismisses the same surfaces. A touch that is not itself under test lands where it
activates nothing (W20 picks a point on no launchpad card). Unit-tested in `test_dialog_guard.py`.

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

## Terminal after a userspace cohort update

`terminal-cohort.py` exercises an already active owner-controlled slot without
power, session, permission, keyboard or screenshot operations. It uses the same
`TiloVm` control transport and bounded wait helper as this suite. First close any
existing **test** Terminal through its declared tab action: the runner refuses an
existing Terminal or ambiguous/stale binding rather than touching its contents.

```bash
TILO_VM_CONFIG=/absolute/path/to/vms.toml \
  python3 scripts/vm-acceptance/terminal-cohort.py \
  --binary /absolute/path/to/tilo-vm --vm sysapps \
  --slot yoga-app-acceptance-1006 --uid 1002 \
  --cohort-receipt /absolute/path/to/successful-cohort/run.json \
  --out target/terminal-cohort-new-run
```

The receipt must be the successful canonical `swap apps+runtime` receipt for
this VM and slot, with full source commit and inventory digest. The output
folder must be new. The runner checks that exact layer is installed, discovers
Open and the live `terminal.session` contracts, binds Inspect/Run/Read to one
room/record/opaque connection, and invokes a unique harmless `printf` exactly
once. Broker acceptance is insufficient: every job must reach success. Read
must report the same Default session, the marker as a standalone output line,
and a subsequent Default prompt. A slot process probe reads only UID, process
identity, runtime mappings and ELF hashes; the mapped runtime must equal the
SDK ELF in that installed layer, and the slot generation/components must remain
unchanged. Unobservable, ambiguous, stale, refused or truncated subjects fail.

`proof.json`, contracts, job receipts, installed file metadata and the maintained
CLI command log are saved under `--out`. The source association relies on the
operator's original canonical activation receipt and daemon layer inventory;
this script does not replace installer signature/closed-graph validation or
infer source ancestry from a `native.*` name. No Host permission is granted, and
no actor is relabelled. On a physical device, run it only after the owner has
confirmed the matched cohort is active and authorized this test.


## Store and Browser after a paired cohort update

`app-cohort.py` reuses the same maintained VM transport and tracked job helper
as `terminal-cohort.py`. It never creates a VM, changes trust, stages a file,
closes a window, reads Notes, or changes power/session state. Running it does
install/open the chosen test applications and changes Store filters; the
operator must authorize that acceptance after the matched cohort is active.
Do not run the broad `run.py` suite against the user's desktop.

For the independently observable Flatpak and Browser portion:

```bash
python3 scripts/vm-acceptance/app-cohort.py \
  --binary /absolute/path/to/qualified/tilo-vm \
  --vm yoga --slot device --uid 1000 \
  --cohort-receipt /absolute/path/to/actual-apps-runtime-receipt.json \
  --flow flatpak --flow browser --out target/yoga-app-acceptance-UNIQUE
```

Use the owned acceptance VM's name, slot and UID instead to qualify the recipe
before physical delivery. Each output directory must be new. This explicit
partial invocation proves only those two flows. Omitting `--flow` requires
all four flows and refuses missing package fixtures or trusted attestors
before application actions. There is no automatic skip or trust grant.

The receipt must identify the successful installed closed SDK layer and its
actual source commit/inventory digest. The matched Shell/control CLI must
support programmatic action discovery and numeric broker job status. Store
must include default Flathub browsing (PR2986), catalogue inspection and the
catalogue import union `ba5fc5c12d` then `be85891c24` (PR3170). Generic unlisted
import additionally needs `395c9e5f48` (PR3200), matching AppHost/capability SDK,
the canonical installer, and an **already configured** applicable trusted
attestor. A self-signed cohort package is not that prerequisite. The source
receipt and actual mapped ELF hashes, not a version label, establish the SDK
association; installer/daemon verification remains authoritative.

Flatpak acceptance refuses an already installed Calculator, disabled or
foreign Flathub collisions, unavailable remotes and ambiguous bindings. It
requires browse, search, readable details, completed install, account-level
Flatpak readback and a mapped Calculator window. It never removes an app to
manufacture a first-install claim. The initial account metadata and completed
receipts are saved, so a failed post-install Open must not be replayed as a
fresh installation.

Package flows require `--package-fixtures /absolute/path/to/fixtures.json`:

```json
{
  "catalogue": {"app": "so.tilo.calculator", "root": "EXACT_SIGNED_ROOT_HEX", "version": "EXACT_VERSION", "path": "/ALREADY_HANDED/official.tapp"},
  "local": {"app": "so.tilo.calculator", "root": "EXACT_SIGNED_ROOT_HEX", "version": "EXACT_VERSION", "path": "/ALREADY_HANDED/attested.tapp"}
}
```

These are placeholders, not fixtures or trust inputs. Use genuine signed
packages of the device's native architecture/runtime. Paths must already be
readable through the ordinary sandbox/file handoff; the runner grants no
access. Supply different utility apps or run the routes separately, because
each Open must map a previously unopened utility window. Notes and the Store
and Browser acceptance workers are refused as package subjects. Review must
match the exact expected app/root/version and independent trust route before
Install. Completed installation is followed by Store readback, canonical
`app list --installed --json` version readback, and normal mapped Open. The
CLI inventory has no root field: exact root is bound by review and the normal
Install action, not fabricated from that list. Existing cached/native entries
are reported as update/reuse rather than first installations.

Browser acceptance first invokes declared `tab.open-beside` without a URL.
It will not navigate until an additional mapped tab/surface and a distinct
same-room `browser.page` record appear while every previous Browser tab remains.
This requires the framework instance-record fix `b576c07ae5` (PR3215). A queued
Open receipt alone is insufficient. Only that new bound page receives Navigate,
WaitPageSettled and ReadPage; success requires actual settled HTTPS Example
Domain content, not a successful job carrying `settled: false`.

`proof.json` records only completed selected flows. Per-invocation contracts,
receipts/status, public baseline and mapped SDK hashes remain available on
failure. Accepted, lost, ambiguous or unobservable outcomes fail without
replaying the effect. Runner unit tests exercise these transitions and stale
record refusal; they do not claim physical installation or presentation.

### Official full/thin packages through Store's chooser

After the deployment owner confirms the matched apps/runtime, Shell/control,
launcher/install and AppHost cohort is active, `store-chooser.py` uses the normal
owner console actions through `tilo-vm slot exec`. It opens no private app socket,
does not stage bytes or grant publisher trust, and submits no raw path to Store.
The owner console must admit the actual slot user; it never falls back to an
Agent, a synthetic actor, or a different socket when refused.

First acquire the public ARM package through a canonically verified official
catalogue using the independently pinned registry owner. Verify the signed full
archive and derive the thin archive with `tilo-app-package thin`, preserving the
same signed root. Retain the canonical `verified-offer.json` receipt. The exact
runtime named by that offer must be resolvable through the normal independently
verified runtime resolver; an unrelated private SDK or a historical runtime tag
is not compatibility evidence. Hold/pin and installer checks remain authoritative.

The deployment owner stages only those public files through maintained `tilo-vm
vm ssh` stdin into their Downloads scratch directory. This runner consumes their
names from the chooser's declared entries, never invents entry IDs or navigates
an undeclared path. With no other chooser active, run:

```bash
python3 scripts/vm-acceptance/store-chooser.py \
  --tilo-vm /ABS/PATH/tilo-vm --vm yoga --slot YOUR_SLOT \
  --cohort-receipt /ABS/PATH/successful-apps-runtime-receipt.json \
  --fixture /ABS/PATH/verified-offer.json \
  --initial-up 1 --directories Downloads official-tapp-acceptance \
  --thin calculator-aarch64-thin.tapp --full calculator-aarch64-full.tapp \
  --out target/store-chooser-acceptance
```

The normal user picker starts at the existing XDG Documents directory, falling
back to HOME. Choose `--initial-up 1` only when Documents is directly under HOME;
use `0` when the initial directory is HOME. These are bounded declared `up`
actions with fresh inspected state and `can_up`, followed by declared entry IDs;
the runner does not infer filesystem paths or escape picker scope. Each new
Import starts from its independently supplied initial directory.

Both Import jobs must stay observable while a new unique chooser request is
inspected. Each operation rechecks the expected active workspace, unlocked
owner console, request UUID, and listing token. Confirmation is the declared
`confirm:true` action, followed by terminal status on the *original* Store job
and exact root/version review readback. PortalCaller binds application and
workspace; this is not a claim that it identifies a particular record. A
competing same-application chooser cannot be independently attributed, so do
not use the runner concurrently with human or automated portal requests.

The default performs two reviews only. Add `--install-open` only when the owner
has authorized the one install and Calculator Open. It installs once and replays
`tilo-install verify`, then requires canonical `inspect` to match the reviewed
root, publisher, version and runtime. It reports update/reuse when already
installed (including the private cohort); it never closes an existing window.
A same-room mapped tab must have its public `local_app_records` association.
A reused window is accepted only with the same strict resident-SDK proof as a
new window. Ambiguous windows or a stale resident worker fail acceptance.
Missing capabilities, runtime incompatibility, publisher refusal, stale jobs or
unobservable completion fail the run; they are never replaced with CLI install,
clicks, retries, or trust changes. Review completion alone is not installation.

Mapped SDK verification reads only public runtime metadata through maintained
`tilo-vm vm ssh` maintenance transport, as root where Linux requires that access.
This does not change the identity or authority of any semantic action: Store and
chooser actions still execute as the actual slot owner on the owner console.
The probe opens the exact `/proc/PID/map_files/START-END` kernel object and hashes
that descriptor; it never substitutes `/proc/PID/root/...` or a readlink pathname.
It checks process start-time, stable selected VMA identities and unchanged FD
metadata. Overlay files can expose different device numbers in `maps` and
`fstat`, so both are recorded rather than asserting a fabricated equality.

Denied `map_files` access, missing subjects, changed mappings or mismatched
verified layer hashes fail acceptance. A successful namespace pathname hash is
insufficient proof of resident code. The unit fixtures exercise descriptor
selection, remount differences and failure/race behavior; they do not claim a
live kernel capability. An addressed live maintenance probe must succeed before
hardware acceptance. Calculator Open additionally verifies its installed official
bundle with `/usr/bin/tilo-runtime-bundle verify --require-signature`, using a
publisher from the root-owned, non-writable OS runtime trust list, independently
of the offer. It records the verified inner bundle root and stable SHA inventory,
then requires every Calculator worker's exact mapped backing to match it. The
actual `/proc/PID/exe` descriptor must also match the installed executable bytes
verified by the normal package signature replay, so an older app using the same
SDK cannot be credited. Missing
canonical tools, a private-SDK worker still resident after an update, or an
unobservable public tab association fail; Open's immediate spawn receipt cannot
substitute for those proofs. The outer offer and inner bundle roots are distinct
identities. Unit tests validate runner ordering/refusal, not a hardware install
or cryptographic success on the fixture.

Browser fresh-open attribution uses the new tab's public
`local_app_records[tab].record_instance` to resolve its page binding. The tab ID
is not its record instance. Unrelated existing pages publishing late do not
become candidates, and missing/foreign launch associations fail without another
Open or navigation of an existing page.

When the published base disk uses a different management protocol, CI builds a
second `tilo-vm` from that exact base commit. It creates and starts the throwaway
VM with that client, then runs the ordinary `swap vmd --slot vm --builder host`
from the clean target checkout. The target client must read back the VM-wide
management layer before any session is created. P01 records this explicit
management swap even with `swap=none`; equal-protocol `swap=none` tests the base
without a management swap. Failures abort P00 instead of relaxing the protocol
check. For a local older fixture, pass `--bootstrap-tilo-vm <matching-client>`
and `--bootstrap-from <clean-target-checkout>` together. This bootstrap uses
the existing layer admission and activation path, and does not reboot.
