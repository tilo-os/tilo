# Platforms

A dev VM runs under the host's native hypervisor, so a human can open it in
the native UI, and the host's native architecture by default. Emulating
another architecture needs `--arch <arch> --emulated` and is many times slower.

| Host | Adapter (default) | Humans watch it in | Builds | GPU in guest |
|---|---|---|---|---|
| Linux x86_64 / aarch64 | libvirt, `qemu:///session` (`virsh`, `qemu-img`, `passt`) | virt-manager, GNOME Boxes | VM or host | virgl, else llvmpipe |
| macOS arm64 | UTM (`utmctl`, Apple Virtualization) | UTM | VM only | none (llvmpipe) |
| Windows x86_64 / arm64 with Hyper-V | Hyper-V (PowerShell) | Hyper-V Manager | VM only | none (llvmpipe) |
| Windows ARM64 Home | QEMU with WHPX | local VNC | VM only | none (llvmpipe) |

In-VM builds sync the worktree with rsync over SSH, so the host needs
`rsync` (on Windows: MSYS2 in `%USERPROFILE%\msys64\usr\bin`, or a native
rsync on `PATH`). Headless and vkms
slots need no GPU, so every row runs them.

## Linux: libvirt's user session

VMs live in libvirt's rootless per-user connection, `qemu:///session`: no
root, no system daemon config. To watch one: virt-manager, File > Add
Connection > QEMU/KVM user session (GNOME Boxes shows session VMs too).

`vm create` writes the VM's libvirt domain XML itself, beside the VM disk
(`<disk>.xml`), and registers it with `virsh define`. That needs only the
libvirt client and libvirt's QEMU driver: no virt-install, and no libvirt
storage or network driver (a broken `virtstoraged` does not matter). The
domain is UEFI without Secure Boot, KVM with the host CPU (TCG with
`--emulated`), a virtio disk, passt networking with the SSH port forwarded,
and SPICE with virtio video for virt-manager.

`tilo-vm doctor` checks `virsh`, the `qemu-img` beside it and `passt`; that
you can open `/dev/kvm`; and that `virsh --connect qemu:///session
capabilities` succeeds and lists a KVM guest for your architecture (when it
fails, the fix quotes virsh's own error). It prints the install line for
your host:

- dnf: `sudo dnf install libvirt-daemon-kvm libvirt-client virt-manager qemu-img passt`
- apt: `sudo apt install libvirt-daemon-system libvirt-clients virt-manager qemu-utils passt`
- pacman: `sudo pacman -S libvirt virt-manager qemu-full passt`
- zypper: `sudo zypper install libvirt-daemon-qemu libvirt-client virt-manager qemu-tools passt`
- image-based (rpm-ostree): `rpm-ostree install libvirt-daemon-kvm libvirt-client virt-manager qemu-img passt`, then reboot

virt-manager is only the viewer; tilo-vm does not run it.

`doctor` prints the resolved path of every tool it will run. The `qemu-img`
used is the one beside `virsh`; a `qemu-img` from elsewhere on `PATH` (an
Android SDK's, say) is named and refused, never silently used.

The raw `qemu` adapter (`--hypervisor qemu`) drives `os/dev/vm.sh` with that
script's own conventions. It is the headless adapter for CI and scenario
tests: its VMs do not appear in virt-manager or Boxes, so it is never the
default, and it runs only from a Tilo worktree.

## Windows ARM64 Home: QEMU with WHPX

Install the MSYS2 CLANGARM64 QEMU package and enable **Windows Hypervisor
Platform** in Windows Features, then reboot Windows. `tilo-vm doctor` finds
`qemu-system-aarch64.exe`, its matching `qemu-img.exe`, and EDK2 firmware in
`%USERPROFILE%\msys64\clangarm64` (or QEMU on `PATH`). On Windows ARM64 without
the Hyper-V PowerShell module, `vm create` selects `whpx` automatically; use
`--hypervisor whpx` to select it explicitly. It never falls back to software
CPU emulation.

For component swaps that build inside the VM, install MSYS2's `rsync` and
`openssh` packages too (`pacman -S rsync openssh`). `tilo-vm` discovers their
default per-user location without changing `PATH`, uses the matching MSYS2
SSH transport, and passes the Windows worktree as an MSYS2 `/c/...` path so
`rsync` treats it as a local source.

The published QCOW2 remains unchanged. Each VM gets a small QCOW2 child and
its own copy of the UEFI variables template beside that child. Before a
download and before starting QEMU, at least 15 GiB must remain free on the
respective volume. The guest gets the host CPU through WHPX, a PCI serial
device for Linux `console=ttyS0`, a separate firmware UART, a user network
with SSH forwarded to `127.0.0.1:<ssh_port>` (default 2222), and a local VNC
display at `127.0.0.1:5900 + (<ssh_port> % 1000)`. The serial logs are
`<disk>.ttyS0.log` and `<disk>.uart.log`. `vm stop` sends an ACPI power button
through QMP and waits for QEMU to exit.

For SSH-only development, pass `--headless` to `vm create`: WHPX omits the
virtual GPU, USB devices and VNC. WHPX defaults to 3 GiB of guest RAM; use
`--memory-mb 8192` when an in-guest build needs more. On a Snapdragon X Elite
Windows ARM64 host, the same cached direct-boot disk reached SSH in 22 seconds
with 3 GiB, versus 44 seconds with 8 GiB; a full stop and restart with 3 GiB
took 28 seconds. A 2 GiB VM is suitable only for light service work. Neither option
changes the disk image or the installer test path.

### Direct kernel boot for a fast service VM

WHPX can boot a development kernel and initrd directly, without EDK2. Prepare
`Image`, `initrd` and the UKI's `.cmdline` from the **same dev disk** once,
and read that disk's ESP GPT partition UUID. The dev-disk CI pipeline
publishes the three checksum-listed companion files under `directBoot` in
`tilo-vm-disk-aarch64.json`. On WHPX, `vm create` with a published disk
downloads and verifies the matching companions automatically. The source
revision, architecture, package URLs, file names and SHA-256 values must agree
with the selected disk manifest. For a local disk, an operator may extract the
same files from its UKI.
The bundle JSON beside `Image` and `initrd` has this shape:

```json
{
  "kernel": "Image",
  "initrd": "initrd",
  "cmdline": "<the complete UKI .cmdline section>",
  "loader_partuuid": "<the source disk ESP partition UUID>"
}
```

Create a published COW VM with direct boot automatically:

```powershell
tilo-vm vm create dev --hypervisor whpx --tag main-1a2b3c4 --headless --memory-mb 2048
```

For a local disk, pass `--direct-boot-bundle` to validate and register its
manually prepared files before disk creation:

```powershell
tilo-vm vm create dev --hypervisor whpx --image C:\tilo-vm\disk.qcow2 --direct-boot-bundle C:\tilo-vm\boot\boot.json --headless --memory-mb 2048
```

Alternatively, add a `direct_kernel` table to an existing VM's
`%APPDATA%\tilo-vm\vms.toml` entry:

```toml
[vms.dev.direct_kernel]
kernel = 'C:\tilo-vm\boot\Image'
initrd = 'C:\tilo-vm\boot\initrd'
cmdline = 'root=tilo-auto composefs=<the UKI cmdline, unchanged>'
loader_partuuid = '<the VM disk ESP partition UUID>'
```

`cmdline` must be the complete `.cmdline` section, not the abbreviated example
above. `loader_partuuid` is the GPT **partition** UUID of the ESP in the
source disk, not the disk GUID or FAT volume ID. WHPX passes it through
`systemd.setenv=TILO_ROOT_LOADER_PARTUUID=...`, preserving Tilo's existing
ESP-to-root binding when systemd-stub is absent. Without the correct value,
Tilo root discovery stops in the initrd. This mode is for local development:
it bypasses firmware and UKI boot verification, so installer and release
acceptance still use the normal firmware boot. Remove the table to restore
normal boot.

## Creating a VM

```sh
tilo-vm vm create dev                        # newest published main/release disk
tilo-vm vm create dev --tag main-1a2b3c4     # a specific one
tilo-vm vm create dev --image ~/tilo-vm.qcow2 --disk ~/vms/dev.qcow2   # a local disk
tilo-vm vm use dev            # this worktree (writes .tilo-vm)
tilo-vm vm use dev --global   # the default for every worktree
```

`vm create` checks everything before it downloads or creates anything, and
stops at the first missing piece with its fix: the hypervisor and its tools,
an `--image` that exists, the published disk for the tag (for your
architecture and format), a VM disk path that is not taken, and free space
for the download and the VM. Then it downloads the tag's manifest for your
architecture, `tilo-vm-disk-<arch>.json`, from the forge's generic packages,
picks the disk (qcow2, or VHDX for Hyper-V), resumes an interrupted download,
checks its SHA-256 and caches it. Each architecture's CI lane publishes its
disk on its own, so a new tag can hold one architecture before the other:
without `--tag`, `vm create` takes the newest tag that has yours, and says
so when none of the newest does.
Last, it makes the VM's root SSH key and creates the VM
([The root SSH key](#the-root-ssh-key)).

The download needs a **forge token** with package read access:
`FORGEJO_TOKEN`, or an fgj (`~/.config/fgj/config.yaml`) or tea login for
code.tilo.so. That token is read only for disk downloads; image pulls
(`swap --from ci:`/`sha:`, `vm upgrade`) come from the public registry.tilo.so
and need no login (`tilo-vm guide troubleshooting`).

### No disk published yet

`tilo-vm doctor` says whether one is. If none is:

- wait for the main "Release / OS" run (`.forgejo/workflows/build-image.yml`):
  each native lane publishes its own architecture's disk as soon as it
  passes, without waiting for the other. An integration run builds on an
  unsigned `tilo-ci` image and its disk tag ends in `-ci`; a publication
  requests the disk from its signed image;
- or dispatch "Release / Dev VM disk" (`.forgejo/workflows/dev-disk.yml`)
  yourself for an existing same-commit base. Set `architectures=aarch64` for
  an ARM host; the workflow's manual default is `x86_64`. Use
  `fgj actions workflow run dev-disk.yml -R tilo/tilo -r main` with
  `-f source_revision=<full SHA> -f base_image=<same-commit OCI ref>
  -f tag=<disk tag> -f architectures=aarch64 -f formats=qcow2`;
- or ask a human for a shared VM and use it: `tilo-vm vm use <name>`;
- or, when this checkout has `os/dev/build-dev-image.sh`, build a disk
  locally and pass it as `--image`. The error says whether it exists here.

## A local disk, and throwaway VMs

`--image` takes a qcow2 dev disk or a raw disk with a GPT, such as
`os/dev/disk-image.sh --raw` writes. `vm create` reads the file's first bytes
and refuses anything else (a blank or half-written disk) before it creates
anything. The image is never written: the VM's disk is a qcow2 child of it,
with a fresh UEFI variable store, so every VM made from one image is a clean
boot of it. That makes a VM per attempt cheap:

```sh
tilo-vm vm create try1 --image ~/tilo-vm/media/disk.raw
tilo-vm vm start try1         # waits up to 180s for SSH; an early-boot failure ends with its serial command
tilo-vm vm serial try1
tilo-vm vm rm try1 --force    # power off, unregister, delete its disk, variables, logs and key
```

`vm rm` keeps the image, and forgets a VM it did not create (one registered by
hand) without touching its hypervisor, disk or key.

On WHPX, an image on a network path (`\\wsl.localhost\...` from WSL, or an SMB
share) is imported once into the cache (`tilo-vm/imports/` in the platform
cache dir) and the VM is made from that copy: QEMU on Windows reads a backing
file behind a UNC path unreliably. The import converts it to qcow2 and
compares every guest-visible byte with the source before it is used; it is
reused until the source's size or modification time changes.

### Booting a candidate ESP

To try a candidate UKI or boot loader without rebuilding the disk, build a
small override ESP with `os/dev/make-boot-esp.sh` (`os/dev/local-boot-esp.md`)
and attach it ahead of the VM's disk:

```sh
tilo-vm vm create esp1 --image base.raw --boot-esp tilo-boot-override.raw
```

The ESP is attached read-only with the first boot index, so firmware boots its
loader and UKI, which mounts the root from the VM's disk. libvirt and WHPX
support it; keep the ESP on a local drive.

## The root SSH key

`tilo-vm` reaches the VM as root with a key, never a password. `vm create`
makes one ed25519 key per VM, `keys/<name>_ed25519` beside `vms.toml` (an
existing key is kept), records it as the VM's `key` in `vms.toml`, and uses
it for every connection. The dev image installs root's `authorized_keys`
from the systemd credential `ssh.authorized_keys.root`, so the key goes in
at creation where the hypervisor has a channel for credentials:

| Adapter | How the key gets in |
|---|---|
| libvirt | at creation: an SMBIOS type 11 OEM string in the domain XML (`<sysinfo type='smbios'><oemStrings><entry>io.systemd.credential.binary:ssh.authorized_keys.root=<base64></entry>`, with `<smbios mode='sysinfo'/>`) |
| whpx | at boot: direct-kernel ARM64 passes the public key with `systemd.set_credential_binary=` on the kernel command line; firmware boots use QEMU's SMBIOS type 11 OEM string |
| UTM, Hyper-V, qemu | once, after the first boot (below). Apple Virtualization and Hyper-V offer no SMBIOS strings or fw_cfg, the image has no cloud-init, and `os/dev/vm.sh` takes no extra QEMU arguments |

The one-time step logs in with the dev account's password (`quickuser`,
password `tilo`, from `os/account.env`) and appends the key for root:

```sh
tilo-vm vm start      # SSH refuses the key, and the error prints the exact command:
ssh -t -p <port> quickuser@<vm address> 'sudo install -d -m 0700 /root/.ssh && echo <key, base64> | base64 -d | sudo tee -a /root/.ssh/authorized_keys >/dev/null && sudo restorecon -R /root/.ssh'
```

It asks for that password twice (SSH, then sudo). The same command fixes a
libvirt VM created before keys were passed at creation.

Each VM also has its own `known_hosts`, `keys/<name>_ed25519.known_hosts`,
so local VMs that share `127.0.0.1` and a forwarded port never collide in
`~/.ssh/known_hosts`; `vm create` clears it, since a new VM has new host
keys. For a shell or one command as root: `tilo-vm vm ssh [<name>] [-- <command>]`.

## Registry and files

`vms.toml` sits in the platform config dir (`~/.config/tilo-vm/` on Linux,
`~/Library/Application Support/tilo-vm/` on macOS, `%APPDATA%\tilo-vm\` on
Windows); `tilo-vm vm list` prints its path. UTM cannot import disks from the
command line: import the downloaded disk once in UTM and pass its name as
`--template`.

`vm stop` waits for the VM lock, so it never cuts off a swap, gc or upgrade;
`--force` skips that.
