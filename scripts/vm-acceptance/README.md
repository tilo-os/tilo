# VM acceptance suite

A scripted acceptance run of the desktop essentials in a real KVM session: a headful
`tilo-vm` slot on two outputs with a test identity, driven only through `tilo-vm` — the
control socket's semantic ops first (`actions`, `state-dump`, `list-targets`), uinput
input (`tilo device input`) where the input path is what is under test. Every run writes
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

## Harness regression tests

```bash
python3 -m unittest discover -s scripts/vm-acceptance/tests -v
```

These host-only tests exercise check results, boot identity validation, service-query
shell exit handling and app-workload prerequisites without starting a VM.

## Run it in CI

t's `vm-acceptance` check (`tools/ci/vm-acceptance.sh`) runs the suite in micro-VMs with
KVM on `disk-100g` x86-64 nodes, so a run needs no laptop memory. It runs in shards
(`SHARDS` in run.py: `session`, `apps`, `window`, `shell`), each a check of its own,
`vm-acceptance/<shard>`, booting a VM of its own: t runs them side by side where there is
room, caches each on its own and runs only a failed one again. The checks that judge a whole
session (P03, R07, A16) judge each shard's.

```bash
t test vm-acceptance                                          # the checks your tree's changes select
t dispatch vm-acceptance --ref main                           # every check on main
t dispatch vm-acceptance --ref my-branch --input only=window --input gl=off
t outputs vm-acceptance/window                                # a shard's report
```

A t that dispatches one check at a time takes a shard by its name
(`t dispatch vm-acceptance/window --input only=W08`).

A working tree's run checks only what its changes reach (`ci_select.py`): the checks of the
changed check modules (and of the modules importing from them), just `provenance` when only
this README or unit tests changed, and every check when anything else in this directory or
`tools/ci/vm-acceptance.sh` changed. A dispatched run, and `vm-acceptance-perf`, run every
check unless `only` names some.

The disk is the tree's own dev image as `os-dev-disk-x86_64` installed it with
`os/dev/build-image.sh` (the disk the dev channel publishes, which nothing boots
itself); each run boots a throwaway overlay of it, so R06 times a cold boot. tilo-vm is
`tilo-vm-binary-x86_64`'s. Both come from t's cache when their inputs are unchanged, and
so does the run itself when the suite, its driver and the files the scenarios read are
too. The script writes the provenance record itself, so P01 reads the tree's commit.

`swap` (default `none`) exercises the swap path on top: run.py `--swap` runs `tilo device
swap <component> --builder host` from a copy of the tree (`--swap-from`) after the slot is
up, session components and apps into the run's slot, VM-wide ones into `vm`; `all` swaps
every userspace component in `os/components.toml`, or name a list (`--input
swap=shell,app:notes`). P01 then reads `base <commit> + <components> swapped from <ref>`
and fails unless every requested swap is the layer its slot runs.

`vm-acceptance-cohort` installs the commit's launcher, install, shell and apps+runtime as
prebuilt signed layers through verified `swap-artifact` (`docs/layer-artifacts.md`), the
route for a change to the shared app runtime (a standalone `app:<dir>` swap is refused
because an app needs the exact runtime it was built with):

```sh
t dispatch vm-acceptance-cohort --ref <branch> --input only=P01,A15
```

Its needs do the handoff (`tools/ci/vm-cohort.sh`): `vm-cohort-x86_64` packs the full layer
streams with the tree's own `tilo-vm` (`layer_artifacts.py produce`), and
`vm-cohort-verifier-x86_64` builds the `tilo-vm` that verifies and imports them from main's
head, fetched from the forge, and checks the handoff with main's `layer_artifacts.py`: the
code under test never verifies its own artifacts. The receipt names an exact clean commit,
so the producer refuses a working tree with uncommitted changes. The handoff's identity (the
source commit, the builder image t.toml pins, a random handoff number standing for the
workflow run, and the receipt's SHA256) travels in the producer's outputs, which t hands
only to the checks that need that result; the consumer checks them, the architecture and the
component completeness before the VM is created, and verifies and imports through the normal
signed cohort path. Any artifact error fails without a build fallback; slot readback and P01
still describe the installed layers, and each swap entry names the consumer that installed
it.

The VM is a libvirt session VM, the way `tilo-vm` runs on a laptop: the script installs
libvirt's QEMU driver, passt, OVMF and Pillow (pixel checks), and runs `run.py` as an
unprivileged user (root cannot hand passt its socket in a session). The nodes have no GPU,
so `gl=auto` creates the VM without virgl and the report records the software renderer;
frame-timing checks gated to real display hardware report that. The report (`report.md`,
`report.json`, shots, frame traces) is the check's output, and the check fails unless every
check passes.


### Agent door (M01-M04) and input fidelity (I01-I08)

M01-M04 (`checks/agent.py`) run `guest/mcp_client.py` in the guest, as Claude Code would run
`tilo-runtime-mcp --connect`: the person's consent is answered through the control socket's
`permission-prompts` actions, the door must offer the path-addressed agent tools, `tilo_ls`
must list Notes as a running app (it is open), a note made with `notes#new-note` (the harness
grants the test agent Notes for the session) is read back through the desktop, and refusals
must reach the agent as `isError` / a JSON-RPC error. Until the image serves `$XDG_RUNTIME_DIR/tilo-session/agent.sock`
(fix/agent-door) they report **xfail** with that reason: an expected failure does not fail the
run (`core.ExpectedFailure`), is listed in the report, and becomes an ordinary check when the
socket appears.

I01-I08 (`checks/input_devices.py`) plug uinput devices copied from the Lenovo Yoga Slim 7x
(`os/hardware/profiles/lenovo-83ed/input/*.json`: I2C bus, 048D:8987 keyboard, 0488:1054
touchpad; `tilo device input device plug lenovo-83ed-keyboard`) and check German keysyms,
layout state over hotplug, no stuck modifiers after a lock, and touchpad tap/click/scroll and
disable-while-typing. I08 starts a terminal that keeps printing, blanks the display, forces the next wake to read its CRTC as dark,
suspends the VM through logind and checks the session log: the wake check saw the dark CRTC,
redid the modeset and lit the output, the session still holds the keyboard, and a key after
the resume arrives and unlocks. I05-I08 are `calibrating`: their observables come from the compositor's
code and have not yet been seen in a CI run, so a failure is **xfail** (with its reason) until
the marker is removed. hwdb `KEYBOARD_KEY_` remaps need the real driver and are not covered.
Capture a new profile from a device's `/sys/class/input/eventN/device/capabilities/*` and HID
report descriptor (see `crates/dev/tilo-vm-protocol/src/profile.rs`).

Pull requests touching the input path, tilo-vm(d), the profiles or the agent door run only
these areas (`ci_select.py`).

### Secret custody (C01, C02)

The compositor loads the device key and is therefore non-dumpable, so the store cannot read
its executable; it proves the device key on each bus connection instead
(docs/identity-security.md). C01 (`checks/secrets.py`) judges that in the slot: its
`/proc/<pid>/exe` is hidden from its own user, `tilo-secret-service apps list` names
`binary:/usr/bin/shell` with the MLS key it keeps, the sealed MLS state sits in an
owner-only directory with no custody warning in the session log, and Notes' portal secret
(`org.freedesktop.portal.Secret`, asked from an `app-tilo-so.tilo.notes-<n>.scope` as a
host-launched app asks) comes from the store, and comes back the same after
`tilo-secret-service` restarts under the running compositor. C02 takes the dev account
R08 signed in at seat0 through first use on its own control socket's `onboarding` actions:
its identity must name `secret_ref.backend = secret-service-v1` with no key file beside it,
and after signing out and in again the store still holds what the compositor keeps and
the MLS state is intact. A slot's throwaway identity uses owner-only files by design, so
only a person's login shows the identity part. No secret is printed: the portal secret is
compared with itself inside the guest. Changes to the store, its client, the MLS vault,
the portal backend or the identity's secret providers select these checks (`ci_select.py`).

### Startup and GPU (A14, R05)

A14 opens Files before anything else in the run does and times it from the app's own
startup marks (`<data home>/tilo-rooms/<room>/so.tilo.file-explorer/.local/state/startup-<N>.json`,
process start to `first-frame-presented`): `files_first_frame_s` (3 s) in a VM, which
renders on a CPU rasterizer or through virgl, `files_first_frame_hw_s` (300 ms) when
frames reach real display hardware. The compositor's request-to-present time is recorded
too when the session logs `tilo::launch_latency` at info. A frame is not a working
window, so A14 also walks Files' AT-SPI tree in the guest with the external AT-SPI
client (`tools/atspi_external_client.py`, copied from the checkout into the slot user's
`~/.cache/acceptance/`; the image does not ship test tooling) and requires its folder listing to
hold entries; it creates an `acc-files-*` folder in home first, so home is never empty.

R05 runs `tilo device health --gpu --json` in the guest and reads every app's `tilo-gpu:
window adapter` line from the session log. A machine without a hardware render node
cannot show a CPU fallback, so a plain software VM leaves R05 unobservable; `--gl` boots
virgl, where a degraded health report, a window on a `type=Cpu` adapter or a reported CPU
fallback fails it. `gpu_check.py` is the same judgement for gates that boot an installed
image without tilo-vm (the installed-desktop gate of t's `os-acceptance-{arch}` check):

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
judgement prints every `APP-STARTUP` line it read, with all its phases. The installed-desktop gate of
`os-acceptance-{arch}` runs it after its app checks, having typed a ~2000-character note on the
real keyboard and reopened Notes on it (`reopen-history`), so a note with thousands of
changes in its history is part of every judged run. Under TCG it reports without judging.

```bash
./os/dev/vm.sh ssh "$(python3 scripts/vm-acceptance/app_startup_check.py --probe)" \
  | python3 scripts/vm-acceptance/app_startup_check.py --require so.tilo.notes
```

### Installed-desktop gates (R06, R07, R08, A15, A16)

The checks `os-acceptance-{arch}` runs on an installed image, which per-push dev builds skip,
run here on every full run instead, on the dev disk under KVM:

- R06 runs the boot gate (`os/tests/boot_performance_gate.sh`, sent from the checkout in
  the command, since the image ships no test tooling) as root: kernel and userspace to greetd's login
  endpoint within `boot_to_greeter_s` (20 s), with the image's boot contract. It reads the
  VM's boot timestamps, so where it runs in the order does not matter. Every run also
  writes where the boot's time went (`R06-boot-analysis.txt`: `systemd-analyze time`,
  `critical-chain` of greetd and of graphical.target, `blame`; `R06-boot-plot.svg`; and
  the whole kernel log with the journal's boot list, `R06-dmesg.txt`),
  holds userspace start to greetd active to `userspace_to_greeter_s` (10 s), and fails
  when the network (NetworkManager, firewalld, `network.target`) is on greetd's critical
  chain: greetd waits for `systemd-user-sessions.service`, which upstream orders after
  `network.target`, so a network start queued before logins are allowed holds the greeter
  until NetworkManager is up.
- R07 runs `tilo-session-report --render-gate` in the slot's session with the KVM
  budgets (`render_frame_ms` 1000, `render_frame_work_ms` 100).
- A15 opens Files, Notes and Settings once unmeasured and then ten times each. Every
  open is requested from inside the session (`date +%s%3N; tilo-control actions invoke
  applications <app>`) and timed to the app's own `first-frame-presented` startup mark,
  so the time holds the compositor's dispatch, the launcher and the sandbox as well as
  the app's start; it waits for the app's processes to exit before the next open. The
  median is held to the app's warm-open budget on the session's renderer
  (`app_startup.warm_open_budget_ms`: 1.5 s on a CPU rasterizer, tighter than a launch's
  3 s, otherwise the app's budget); on a GPU the p95 is also held to the KPI's
  target (`tools/perf/kpi-budgets.toml`).
  Each open is split into segments that add up to it (`open_segments`): request to the
  launcher's start, the launcher's own phases (the marks it hands the app in
  `TILO_STARTUP_LAUNCHER_MARKS`), the launcher's hand-off to the app's `main` (bubblewrap,
  the Landlock shim and the dynamic loader: the shim marks `sandbox-entered` and
  `sandbox-exec`, so `exec_to_main` is the loader alone) and `main` to the first frame.
  The compositor's
  `tilo::launch_latency` lines, which it logs whatever `RUST_LOG` says, add its own
  request→spawn→map→first-present split. An open without either is unobservable.
  After the series Files opens and closes twenty times more, unmeasured: each of those
  must draw its first frame, and the session must outlive all of them (below).
- Every check: the slot's desktop runs as the system unit `tilo-vm-session@<slot>`, whose
  `Restart=always` brings a fresh session up two seconds after its compositor ends, with
  every window gone. `run_check` reads the unit's last exit of its main process and
  `NRestarts` after each check (A15 after each open) and fails the check in which the
  session ended without the harness asking with "the slot's desktop session restarted on
  its own", how its main process ended, and `data/<check>-<n>-session-restart.log`: the
  unit's state, the ended run's flight record (`previous.log`) and panic reports, and the
  kernel's, coredump's and user manager's lines (`acceptance/session_unit.py`). P03 reads
  the same unit at the end of the run.
- A16 is `app_startup_check.py`'s judgement over every app the run launched.
- A17 opens and closes Files, ends the slot's compositor with SIGSEGV from root (as the
  crash A15 caught did), waits for `Restart=`'s next session to answer and opens Files
  twice: each must draw its first frame. A restart once left every later Files open
  refused for the rest of the session.
- R08 signs the dev account (`os/account.env`) in at seat0's greeter through its console
  action `sign-in@greeter`, as a person's passcode does, and times each step from the
  request: the user manager, the `tilo-session` wrapper, the compositor process, its first
  presented frame (the `FRAME-LIFECYCLE serial=1` line's `monotonic_us`, on the clock the
  probe reads from `/proc/timer_list`) and its control socket answering `state-dump`.
  Usable is the later of the last two. It does so twice, the account's first sign-in (cold
  caches and first-login setup) and, after signing out, the next one, held to
  `sign_in_first_usable_s` and `sign_in_again_usable_s` (10 s each). It starts a second
  compositor, so it runs after everything that measures the slot. A disk whose compositor
  predates `monotonic_us` leaves the first frame untimed, and R08 unobservable.

The greeter keystroke check (`os/dev/greeter-acceptance.py`) needs QEMU's monitor, which
a `tilo-vm` VM does not expose, so it stays in `os-acceptance-{arch}`.

### An idle shared workspace (R09)

R01 and R02 measure workspaces that only this device holds. A shared workspace's apps read
their records through the compositor's collaboration broker, which holds each committed
read's wait and re-checks it only when an identity file, the membership or a timer it
registered announces a change; when one of those cannot be followed it re-checks every
200 ms, five times a second per read, forever. R09 measures that on the installed desktop.

A workspace is natively committed only when a relay of another of the person's devices
governs it: a host that may not own workspaces (one that may is a node, whose rooms are
stewarded, not committed). One VM has no second device, so R09 makes one the way a person
enrolls a home server out of band, as the slot user and only into its throwaway identity
(`TILO_IDENTITY_ROOT`; a slot without one leaves R09 unobservable):

1. `tilo-enroll device-key` makes the relay's key in an owner-only directory of the slot
   user's runtime dir.
2. `tilo-enroll certify --capability host --capability read-all --home-peer-service <relay
   endpoint id> --home-peer-addr 127.0.0.1:<port>` certifies it with the identity's root,
   lists it in the identity's devices and records it in `$TILO_IDENTITY_ROOT/peers/home-peers.json`
   as a home peer listening on loopback (`24000 + uid % 8000`); `tilo-enroll install-cert`
   gives the relay its certificate.
3. `tilo-relay run` starts as the transient user unit `tilo-acceptance-home-relay`.
4. `workspace.create` makes a workspace and `invitation.issue` invites into it, which shares
   it on the home relay and enrolls this device into it; R09 waits until the state dump's
   `native_authority` holds the workspace's room under the relay's governance.

It then opens Notes and Files there (R02's opener) and, over the idle window, sums the
context switches of the compositor's `tilo-collab-bro` threads and of every app's
`ui-collab-shove` threads (`R09-idle-shared.json` `named_threads`): the broker within
`idle_shared_broker_wakeups_per_s` (1/s), the shovels within
`idle_shared_shovel_wakeups_per_s` (0). A compositor with no broker thread or apps with no
shovel leave it unobservable. Afterwards it closes the windows, returns to the workspace the
run was in, stops the relay (its journal is `R09-home-relay.log`) and removes the relay from
the home-peer registry. It runs before R07, A16 and R05, which judge the apps it opens too.
An image whose `tilo-enroll` predates `certify --home-peer-service` leaves R09 unobservable.

### Performance records

`run.py --records FILE` writes one record per KPI (`tools/perf/record.py`) from what the
checks measured, whether they passed or not; t's `vm-acceptance-perf` compares them with
the store (docs/measurements/performance.md) and publishes them where the run holds the
store's token.

| KPI | Unit | Check | What |
|---|---|---|---|
| `boot.to_greeter_s`, `boot.{kernel,initrd,userspace}_s` | s | R06 | kernel+userspace to a usable greeter, and its phases |
| `boot.greeter_s`, `boot.userspace_to_greeter_s` | s | R06 | kernel start, and userspace start, to greetd.service active |
| `boot.kernel_meminit_s` | s | R06 | the kernel log's silence ending at `SLUB: HWalign`, while the boot CPU alone frees all RAM to the page allocator (from `R06-dmesg.txt`) |
| `boot.first_boot` | count | R06 | 1 when this boot is the only one the persistent journal holds: fresh host pages and first-boot units, so not comparable with a reboot |
| `session.sign_in_{first,again}.{shell,first_frame,usable}_s` | s | R08 | greeter sign-in request to the compositor process, its first presented frame, and a usable desktop |
| `idle.shell_cpu_pct`, `idle.session_cpu_pct` | percent | R01 | compositor and whole session CPU over the idle window |
| `idle.gpu_driver_cpu_pct` | percent | R01 | the part of `idle.shell_cpu_pct` on threads the GPU driver started (`llvmpipe-N`, and `tilo-gpu-driver`, the name the compositor gives threads started while it opens the device). On lavapipe this is the window's frames rasterised, a GPU's work on real hardware, so R01 and R02 judge a software renderer's compositor CPU without it; with no frame painted it must stay under `idle_software_raster_cpu_pct` |
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
| `idle.wakeups_per_s.shared_broker`, `idle.wakeups_per_s.shared_shovels` | per_s | R09 | context switches of the compositor's collaboration broker threads, and of every app's collaboration shovel, with Notes and Files idle in a natively committed workspace |
| `idle.ram_mib.after_close`, `idle.ram_mib.*.after_close`, `memory.idle_*.after_close` | MiB | R03 | the same once R03 has closed the four apps |
| `memory.shell_anon_mib.per_open_close` | MiB | A15 | least-squares growth of the compositor's anonymous RSS per warm open and close |
| `memory.shell_anon_mib`, `memory.session_mib`, `memory.app_mib.<app>`, `memory.app_anon_mib.<app>` | MiB | R03 | the compositor's anonymous RSS; the compositor's and apps' cgroups |
| `disk.runtime_mib.per_user`, `disk.runtime_builds.per_user`, `disk.app_store_mib.per_user` | MiB, count | R03 | the slot user's downloaded runtime builds (`$XDG_DATA_HOME/tilo-runtime`) and app store (`$XDG_DATA_HOME/tilo-apps`) after the apps closed: what collection after each change leaves |
| `memory.shell_anon_mib.empty` | MiB | R01 | the compositor's anonymous RSS with no app open |
| `memory.compositor_memfd_mib{,.four_apps,.after_close}` | MiB | R01, R03 | shmem allocated to the memfds and `/dev/shm` files the compositor holds open, mapped or not (`stat -L` through `/proc/PID/fd`); `R0x-shell-smaps*.json` `shm_by_name` splits it by memfd name and `shm_fds` lists every memfd, dma-buf and DRM fd with its fdinfo |
| `memory.shell_parts_mib.<class>`, `memory.shell_parts_mib.malloc_{in_use,free}`, `memory.shell_threads` | MiB, count | R01 (and `.after_close`, R03) | the compositor's own resident memory per kind of mapping (`heap_main`, `heap_arenas`, `thread_stacks`, `anon_other`, `anon_exec`, `gpu_memfd`, `fonts`, `binary`, `gpu_driver_libs`, `libraries`, `files_other`, …), from `mincore` over `/proc/self/maps` in the compositor (`tilo-control raw '{"op":"process_memory"}'`; Tilo's kernel has no smaps); glibc's heap in use and free (`mallinfo2`); its thread count. The whole report is `R01-shell-memory-map.json` |
| `app.open.warm_ms.<app>` | ms | A15 | open request to first presented frame, ten warm opens |
| `app.startup_ms.<app>`, `app.startup_ms.file-explorer.first_in_session` | ms | A15, A14 | process start to first presented frame |
| `app.open.{request_to_launcher,launcher,sandbox_to_main,exec_to_main}_ms.<app>` | ms | A15 | the open before the app's `main`, by segment |
| `app.open.launcher_phase_ms.<app>.<mark>` | ms | A15 | each launcher phase, the interval ending at its mark |
| `app.open.{request_to_spawn,spawn_to_map,map_to_first_present,request_to_first_present}_ms.<app>` | ms | A15 | the compositor's own split of the open |
| `render.frame_cpu_ms` | ms | R04 | compositor CPU per painted frame during interactions |

`idle.ram_mib` stays the guest-wide figure the KPI defines. Beside it, R01, R02
and R03 write `R0x-footprint*.json` (`resources.footprint`): every user's processes by RSS
split into anon, file and shmem (Tilo's kernel has no smaps, so there is no PSS), every
cgroup's anon, file, shmem and kernel charge grouped into the buckets above, each tmpfs's
use, and the guest's meminfo and zoneinfo split. A tilo-vm guest runs seat0's greeter
beside the slot, which an installed machine ends at login, so `idle.ram_mib.installed` is
the number to hold against the target.

The records name the commit whose code ran (the swapped ref's, else the disk's) and keep
the base disk's commit, the swapped components and the booted image digest in `meta`.

## Provenance

### Component replacement evidence

`component-replacement.py` checks captures from a **fresh disposable headless
slot**, named `replace-<12 random hex>`. It performs no VM operations and makes
no claim that the evidence is a remote attestation. Its success status is
`captured_evidence_consistent`; live execution and artifact authentication must
be established separately. Fixtures in its unit tests are not live evidence.

The acceptance operator first obtains an owned VM QA window, prepares a known
working shell, the candidate and a shell that fails session readiness using the
existing import/build pipeline, and selects the authenticated `apps+runtime`
cohort through the existing manager. Do not create new trust grants, replace
publisher pins, or roll back that runtime independently of installed packages.
Use `tilo-vm` throughout; do not use raw SSH/SCP for deployment or removal.

Retain these **canonical `run.json` receipts** under the names below (the CLI's
`receipt` field gives their paths; its printed JSON is not the canonical file):

| File | Production operation captured |
| --- | --- |
| `create-run.json` | `tilo-vm --json slot create SLOT --headless`, with `created=true`, empty generation 0 |
| `before-run.json` | `slot show SLOT` after selecting the working shell and matched runtime |
| `activate-run.json` | `swap shell --from layer:CANDIDATE` targeting SLOT, with successful session restart |
| `active-run.json` | `slot show SLOT` after candidate readiness succeeds |
| `fail-run.json` | `swap shell --from layer:FAULT_FIXTURE` targeting SLOT, failing readiness and reporting successful recovery |
| `recovered-run.json` | `slot show SLOT` after recovery; generation, layers, inventory and provenance must match the active capture |
| `remove-run.json` | `slot rm SLOT`, only after fresh ownership was acknowledged |

Capture `fixture-before.json` before the fault injection and `fixture-after.json`
after recovery through `tilo device slot exec`. Each object contains `vm`, `slot`,
`user`, `uid`, `home`, `identity_root` and `sentinel_sha256`. Read only the named
environment fields and hash one sentinel created in this fresh user's home;
never dump the environment, keys, personal documents or existing identities.
The captures must be identical; the identity root must be the fixture user's
`HOME/.local/share/tilo/test-identity`. Identity minting is unnecessary.

After supported removal, independently observe teardown through manager slot
listing and read-only account/path/unit probes. `teardown.json` must carry the
same `vm`, `slot`, `user`, `uid`, `home`, and explicitly false values for
`slot_present`, `account_present`, `home_present`, `session_active`,
`user_manager_active`, `session_dropins_present`, `user_dropins_present`.
An unavailable observation fails; it cannot be replaced by assuming removal
succeeded. Keep the probe commands/output with the captures for review.

```sh
python3 scripts/vm-acceptance/component-replacement.py PATH_TO_CAPTURES \
  --vm OWNED_VM --slot replace-012345abcdef --source FULL_CANDIDATE_COMMIT
```

The result hashes every input file. An absent receipt, missing provenance,
ABI/source mismatch, no-op activation, preflight-only failure, failed recovery,
lost sentinel, or unobserved teardown fails the contract. This audit neither
substitutes for the native signed-cohort checks nor proves the live journey when
only source tests have run.

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
| provenance | P01 image ↔ commit, P02 headful slot on two outputs with an identity, P03 no failed or restart-looping session units, and no restart of the slot's session unit |
| window | W01–W21: open/focus, move, resize, maximize, full screen, tabs, strips, workspaces, two outputs, overview, strip switcher, touch, touchpad, animation pacing (frame trace), real keyboard typing, touch that never drives the cursor (a launchpad card tapped open, the cursor hidden until the mouse moves), the on-screen keyboard (shown for a tapped field, typing into it, hidden by the physical keyboard), the on-screen keyboard typing into the shell's own search field, unlocking the session by touch on the lock screen's own keyboard (and the lock refusing a control-socket snapshot) |
| ui | U01 theme and language through actions, U02/U03 Settings, quick settings, workspace settings, overview in light/dark/high-contrast (en, de) with layout checks on `list-targets` rects, U04 German action titles |
| apps | A01–A07: Terminal (live shell), Files, Settings, Notes, Store, a host Wayland app, an X11 app — first frame, placement, focus, exit on close, no false toasts. A08–A13, the apps used every day, one per toolkit and protocol path: Chromium on Wayland (A08, the engine Electron apps ship) and forced onto X11 (A09), GNOME Text Editor (A10, GTK4/libadwaita), FeatherPad (A11, Qt6 Wayland), alacritty (A12, winit/OpenGL), glxgears (A13, OpenGL X11 like games) — window in the current strip with focus, a first frame with content, the right buffer scale on both outputs as it moves between them, real-keyboard typing, copy to and paste from Tilo's clipboard, exit on close; A13 also full screen and back. A14 Files' first launch: process start to first presented frame within budget, and its folder listing read off its AT-SPI tree. A15 ten warm opens each of Files, Notes and Settings, request to first frame; A16 every launch of the run within its app's budget; A17 Files opens after the session's compositor ended abruptly |
| search | S01–S09: empty-query view, English and German names, launch from search, windows, actions, files, room activity, workspace history |
| resources | R01/R02 idle CPU, wakeups and frames with no apps and with several (`R01-idle-empty.json`/`R02-idle-apps.json` carry CPU and context switches per thread of the compositor, `shell_threads` and `shell_thread_groups`, and of any other process above 0.5%, plus the compositor's age when the window opened, `session_age_s`; the compositor is the slot user's `shell` with no `shell` above it, so its own shader-seed child is never taken for it; an over-budget compositor names its busiest threads), R03 memory per open app from its own cgroup (memory.current, anon/file) plus the compositor's anonymous RSS; R01 and R03 also write the compositor's `smaps_rollup`, status lines and 20 largest anonymous mappings (`R01-shell-smaps.json`/`R03-shell-smaps.json`), so R03 less R01 is what four windows cost and a growing `[heap]` or run of glibc arenas reads apart from one GPU buffer, R04 frame CPU p50/p95/p99 during interactions (wall time on real display hardware), R05 every app window renders on the machine's GPU, not a CPU fallback; the renderer and whether the display is real hardware are recorded; R06 boot to a usable greeter and where its time went, R07 the render-path gate, R08 greeter sign-in to a usable desktop, R09 an idle shared workspace's broker and app shovels |
| basics | B01 brightness, B02 Wi-Fi, B03 Bluetooth (unavailable without hardware, no fake controls), B04 de nodeadkeys + compose applied to the physical keys, B05 display arrangement, B06 hotplug. The GNOME keyboard import runs only in a host-desktop session, so the host-desktop VM test checks it (dist/session/vm-e2e, H05) |

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
session locked") and the slot's session is restarted (`tilo device slot restart`) before the next
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
`tilo device input` does not have yet. Output hotplug (B06) is driven by forcing one
connector's sysfs status off and back to `detect`, each followed by the card's change uevent, which is
the notification the compositor listens for.

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
owner console actions through `tilo device slot exec`. It opens no private app socket,
does not stage bytes or grant publisher trust, and submits no raw path to Store.
The owner console must admit the actual slot user; it never falls back to an
Agent, a synthetic actor, or a different socket when refused.

First acquire the public ARM package through a canonically verified official
catalogue using the independently pinned registry owner. Verify the signed full
archive and derive the thin archive with `tilo package thin`, preserving the
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
`tilo app verify`, then requires canonical `inspect` to match the reviewed
root, publisher, version and runtime. It reports update/reuse when already
installed (including the private cohort); it never closes an existing window.
A same-room mapped tab must have its public `local_app_records` association.
A reused window is accepted only with the same strict resident-SDK proof as a
new window. Ambiguous windows or a stale resident worker fail acceptance.
Missing capabilities, runtime incompatibility, publisher refusal, stale jobs or
unobservable completion fail the run; they are never replaced with CLI install,
clicks, retries, or trust changes. Review completion alone is not installation.

Mapped SDK verification reads only public runtime metadata through maintained
`tilo device vm ssh` maintenance transport, as root where Linux requires that access.
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
