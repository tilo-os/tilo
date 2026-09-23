# Dev VMs with per-agent slots

## Outcome

Humans and agents test Tilo OS in one to a few shared dev VMs on macOS, Windows or Linux
hosts. Each agent gets an **overlay** inside a shared VM: its own Linux user, its own `/usr`
layer stack and its own session. Switching a component to another build is one command and
never needs a new VM or a reboot. Humans view and operate VMs through the host's native
hypervisor UI (UTM, Hyper-V Manager, virt-manager/Boxes); we build thin adapters, not a VM
manager.

## Principle: no dev/prod drift

Dev tooling reuses production mechanisms. Where a dev-only piece is unavoidable, it is
small, sits in front of the production path rather than beside it, and a contract check
catches divergence.

- A session starts via PAM and then `/usr/libexec/tilo-session`, exactly as greetd starts it.
  Everything after the PAM session is production code, so any future profile or session
  restore behaves identically in dev.
- A component swap is a **normal session restart** (shell logout, then a fresh login). There
  is no hot-swap path.
- One component manifest drives both the production Containerfile and the dev layers.

## Session topology

| Kind | Display | Seat | Entered via | Driven by |
|---|---|---|---|---|
| Seat 0 | the VM's real virtio-gpu, shown in the hypervisor UI | `seat0` + VT | production greetd, then greeter, then `tilo-session --backend drm` | human, agents |
| Headful overlay | a `vkms` card created via configfs; costs no GPU | own logind seat (`ID_SEAT` udev tag) | `tilo-vm-session@<overlay>.service` (`PAMName=login`, `Restart=always`), then `tilo-session --backend drm` | per-seat `uinput` devices (real libinput path) + `tilo-control` |
| Headless overlay | offscreen | none | the same unit, then `tilo-session --backend headless` | `tilo-control`, screenshots |

- The DRM backend already scans out through CPU-written dumb buffers
  (`crates/session/shell/src/compositor/backend/drm/mod.rs`), so vkms cards need no
  multi-GPU or PRIME work. Rendering uses the shared render node, or llvmpipe when there is none.
- `tilo-vm seat claim <overlay>` stops greetd and restarts that overlay's session unit on
  `seat0`/vt1. `release` hands the seat back to greetd. Both are normal session restarts.
- vkms connectors can be added and removed at runtime to exercise hotplug.

## Overlays and layers

- **Overlay** = a Linux user `tdo-<name>` plus an ordered list of layers on top of base `/usr`,
  plus a kind (`headful`, `headless`), with numbered generations under
  `/var/lib/tilo-vm/overlays/<name>/`. The overlay records its owner (host, worktree, branch).
- **Layer** = an immutable, content-addressed sysext directory at
  `/var/lib/tilo-vm/layers/<sha256>/` (`usr/…` + `usr/lib/extension-release.d/`), with
  `provenance.json` recording component, source commit, dirty-tree hash, builder image
  digest and cargo profile. Files are deduplicated with reflinks where the filesystem supports them.
- The overlay's layers are applied with `ExtensionDirectories=` to **both** its session unit
  and `user@<uid>.service` (drop-ins), so the compositor and its user services see one build.
- **VM-wide components** (`fs`, local model, greeter) live in the pseudo-overlay `vm`.
  Its layers are merged into the VM's `/usr` with `systemd-sysext refresh`, and then the affected
  system units restart.

### Swap semantics

`tilo-vm swap [<overlay>] <component> --from worktree|sha:<commit>|layer:<hash>` resolves the
source to a layer, writes a new generation and applies it:

- Session components (shell, launcher, user services, runtime) restart the session normally.
- Apps alone are installed as self-signed `.tapp` packages into the overlay user's store. User-store
  shadowing is a production mechanism, so the change is live at the next launch with no restart.
- VM-wide components are merged with `systemd-sysext refresh` and the affected unit restarts.

`tilo-vm rollback` re-applies the previous generation the same way.

### Component manifest

`os/components.toml` maps each component to cargo package(s), bins, features and install
paths. The production Containerfile installs from it, and dev layers are assembled from it. A
policy check fails if the image contains Tilo binaries that are not in the manifest, or if the
manifest names paths the image lacks. `apps+runtime` is one component, because thin apps must match
`libruntime_dylib.so`.

### Builders

Every builder produces the same layer format.

- `vm` (default): the worktree is synced into the VM (virtiofs where the hypervisor offers it,
  otherwise rsync over SSH), then built with the dev image's toolchain, which is the same builder
  as the base image. The sccache is shared, and each worktree gets its own target dir.
- `host` (Linux): runs the pinned builder image in rootless podman.
- `ci`: extracts the manifest paths for a component from the published
  `code.sandbox.example/qos/tilo:<tag>` image of a commit.

An ABI check refuses a layer whose builder digest differs from the base image's. It also
refuses apps whose runtime does not match.

### Dev binaries

Environment overrides such as `TILO_TEST_CONTROL` are currently gated on
`cfg!(debug_assertions)`. They move behind a cargo feature, `dev-overrides`. Dev binaries are
release builds with that feature, so performance is realistic, and `--profile dev` gives faster
compiles. A contract check guarantees that production images never enable the feature.

## CLI and guest daemon

- `tilo-vm` is a host CLI, one Rust binary for macOS, Windows and Linux
  (`crates/dev/tilo-vm`). It reaches the guest daemon `tilo-vmd` by JSON-RPC over SSH stdio
  (`ssh <vm> tilo-vmd rpc`). The same transport works on every hypervisor.
- `tilo-vmd` is a system service in the dev image. It owns layers, overlays, generations,
  overlay users, vkms seats, `uinput` devices and the session units.
- Commands:
  - `vm create|list|start|stop|use|ssh|status|upgrade`
  - `overlay create|list|restart|rm`, plus `swap`, `rollback` and `logs`
  - `seat claim|release`, `shot`, `input`, and `control …`, which passes through to the in-VM
    `tilo-control` because AGENTS.md allows only one driving story.
- Defaults: the overlay name derives from the worktree/branch. The VM comes from
  `TILO_VM`, then `.tilo-vm` in the worktree, then `~/.config/tilo-vm/vms.toml`, which a
  human edits to tell agents which VM to use.
- Every command takes `--json`, is idempotent and writes a receipt under `target/dev-runs/`.

## Dev image and hosts

- `os/Containerfile.dev` builds `FROM` the production image of the same commit and adds
  `tilo-vmd`, sshd, the builder toolchain, sccache and `dev-overrides` binaries (installed via
  the manifest). Its `/usr` is unsealed, so sysext can merge into it. Upgrades are
  `bootc switch` to a newer `tilo-vm:<tag>`, and overlays survive because they live in `/var`.
- CI publishes disk artifacts for x86_64 and aarch64: qcow2 (UTM, libvirt) and VHDX (Hyper-V).
- Hypervisor adapters are thin and native, and each implements only
  create/start/stop/destroy/ssh-endpoint:
  - libvirt (`virt-install`/`virsh`) on Linux
  - UTM (`utmctl`, Apple Virtualization backend) on macOS
  - Hyper-V (PowerShell) on Windows
- The SSH key is injected at first boot.
- `os/dev/vm.sh` (raw QEMU) stays for CI and scenario tests.

## Early spikes

1. vkms configfs: several devices in the image's kernel, each assigned to its own logind seat,
   plus a logind session opened on each by a `PAMName=` unit.
2. `ExtensionDirectories=` on `user@<uid>.service` and on the session unit: user units and
   the bwrap-based launcher see the overlay's `/usr`.

## Revisions after the spike and review

- The CLI is `tilo-vm` (daemon `tilo-vmd`), because `tilo-dev` is taken by the `tilo-dev.desktop`
  session. Slots replace the Fedora 44 loop over time.
- The per-agent object is called a **slot** (`tilo-vm slot …`, user `tds-<name>`, seat
  `seat-<slot>`). "Overlay" below means slot.
- The session unit uses `PAMName=greetd` and `SyslogIdentifier=tilo-session-%i`. A swap stops the
  session unit, restarts `user@<uid>`, then starts the session unit. A VM-wide swap also restarts
  every slot. Layers are SELinux-labelled at import and listed bottom-first. vkms cards have a
  fixed connector count, and hotplug means toggling connector status. See the spike results.
- Adapters also expose serial, screenshot, reset and native control (QMP/virsh/COM pipe) for
  debugging before the network is up.
- Storage: allocated-byte accounting, caps, a free-space floor, and `tilo-vm gc [--dry-run]`.
  In-VM target dirs are disposable and evicted least-recently-used first. sccache stays the shared cache.
- Layer provenance records the base digest, source and dirty hashes, the target triple, the
  Cargo.lock hash and the toolchain. Local layers are never promoted, because CI rebuilds from source.
  "Same commit" applies only to CI building the dev image.
- Locks and leases cover VM-wide operations and the seat0 claim, and the daemon reconciles its
  state after a crash.
- Native architecture is the default. Emulation is explicit and slow.
- Receipts carry per-phase timings and cache hits, compared against these goals: warm build
  < 60s, swap + restart < 10s, cached boot < 30s.
- Boot-level components (kernel, UKI, bootloader, initramfs, installer) are not swappable. `swap`
  redirects them to the existing `os/dev/` scenario drivers.

## Later: cloud dev VMs (not in this phase)

Two invariants keep this cheap later:

- The hypervisor adapter interface stays narrow (create/start/stop/destroy/ssh-endpoint).
- The `vm` builder can fetch sources by commit, not only by file sync.

A cloud VM then needs three things:

- A provider adapter, with cloud-init injecting the key.
- An image import per provider, from the published raw/qcow2.
- A way for humans to see `seat0` remotely, since there is no local hypervisor window.

Headless and vkms overlays need no GPU and work unchanged. Remote display is the only real
gap; it needs frame streaming or a remote-desktop path, which this phase excludes.

## Out of scope

Cloud VMs, frame streaming or a remote display, and hot reload of a running compositor.
