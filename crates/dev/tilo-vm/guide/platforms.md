# Platforms

A dev VM runs under the host's native hypervisor, so a human can open it in
the native UI, and the host's native architecture by default. Emulating
another architecture needs `--arch <arch> --emulated` and is many times slower.

| Host | Adapter (default) | Humans watch it in | Builds | GPU in guest |
|---|---|---|---|---|
| Linux x86_64 / aarch64 | libvirt, `qemu:///session` (`virsh`, `virt-install`) | virt-manager, GNOME Boxes | VM or host | virgl, else llvmpipe |
| macOS arm64 | UTM (`utmctl`, Apple Virtualization) | UTM | VM only | none (llvmpipe) |
| Windows x86_64 / arm64 | Hyper-V (PowerShell) | Hyper-V Manager | VM only | none (llvmpipe) |

In-VM builds sync the worktree with rsync over SSH, so the host needs
`rsync` (on Windows: from WSL, MSYS2 or cwRsync on `PATH`). Headless and vkms
slots need no GPU, so every row runs them.

## Linux: libvirt's user session

VMs live in libvirt's rootless per-user connection, `qemu:///session`: no
root, no system daemon config. To watch one: virt-manager, File > Add
Connection > QEMU/KVM user session (GNOME Boxes shows session VMs too).
`tilo-vm doctor` checks `virsh` and `virt-install`, that `qemu:///session`
answers, and that you can open `/dev/kvm`, and prints the install line for
your host:

- dnf: `sudo dnf install libvirt-daemon-kvm virt-install virt-manager qemu-img`
- apt: `sudo apt install libvirt-daemon-system libvirt-clients virtinst virt-manager qemu-utils`
- image-based (rpm-ostree): `rpm-ostree install libvirt-daemon-kvm virt-install virt-manager qemu-img`, then reboot

`doctor` prints the resolved path of every tool it will run. The `qemu-img`
used is the one beside `virsh`; a `qemu-img` from elsewhere on `PATH` (an
Android SDK's, say) is named and refused, never silently used.

The raw `qemu` adapter (`--hypervisor qemu`) drives `os/dev/vm.sh` with that
script's own conventions. It is the headless adapter for CI and scenario
tests: its VMs do not appear in virt-manager or Boxes, so it is never the
default, and it runs only from a Tilo worktree.

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
for the download and the VM. Then it downloads `tilo-vm-disk.json` for the
tag from the forge's generic packages, picks the disk (qcow2, or VHDX for
Hyper-V), resumes an interrupted download, checks its SHA-256 and caches it.
Last, it makes the VM's root SSH key and creates the VM
([The root SSH key](#the-root-ssh-key)).

The download needs a **forge token** with package read access:
`FORGEJO_TOKEN`, or an fgj (`~/.config/fgj/config.yaml`) or tea login for
code.podesta.ai. That token is read only for disk downloads; image pulls
(`swap --from ci:`/`sha:`, `vm upgrade`) use podman's registry login instead
(`tilo-vm guide troubleshooting`).

### No disk published yet

`tilo-vm doctor` says whether one is. If none is:

- ask a human to dispatch the "Release / Dev VM disk" workflow
  (`.forgejo/workflows/dev-disk.yml`); it needs the commit's production
  image to be published;
- or, if you may dispatch CI yourself: `fgj workflow run dev-disk.yml -R qos/qos`,
  then `fgj run watch`;
- or ask a human for a shared VM and use it: `tilo-vm vm use <name>`;
- or, when this checkout has `os/dev/build-dev-image.sh`, build a disk
  locally and pass it as `--image`. The error says whether it exists here.

## The root SSH key

`tilo-vm` reaches the VM as root with a key, never a password. `vm create`
makes one ed25519 key per VM, `keys/<name>_ed25519` beside `vms.toml` (an
existing key is kept), records it as the VM's `key` in `vms.toml`, and uses
it for every connection. The dev image installs root's `authorized_keys`
from the systemd credential `ssh.authorized_keys.root`, so the key goes in
at creation where the hypervisor has a channel for credentials:

| Adapter | How the key gets in |
|---|---|
| libvirt | at creation: an SMBIOS type 11 OEM string (`virt-install --sysinfo type=smbios,oemStrings.entry0=io.systemd.credential.binary:ssh.authorized_keys.root=<base64>`) |
| UTM, Hyper-V, qemu | once, after the first boot (below). Apple Virtualization and Hyper-V offer no SMBIOS strings or fw_cfg, the image has no cloud-init, and `os/dev/vm.sh` takes no extra QEMU arguments |

The one-time step logs in with the dev account's password (`quickuser`,
password `tilo`, from `os/account.env`) and appends the key for root:

```sh
tilo-vm vm start      # SSH refuses the key, and the error prints the exact command:
ssh -t -p <port> quickuser@<vm address> 'sudo install -d -m 0700 /root/.ssh && echo <key, base64> | base64 -d | sudo tee -a /root/.ssh/authorized_keys >/dev/null && sudo restorecon -R /root/.ssh'
```

It asks for that password twice (SSH, then sudo). The same command fixes a
libvirt VM created before keys were passed at creation.

## Registry and files

`vms.toml` sits in the platform config dir (`~/.config/tilo-vm/` on Linux,
`~/Library/Application Support/tilo-vm/` on macOS, `%APPDATA%\tilo-vm\` on
Windows); `tilo-vm vm list` prints its path. UTM cannot import disks from the
command line: import the downloaded disk once in UTM and pass its name as
`--template`.

`vm stop` waits for the VM lock, so it never cuts off a swap, gc or upgrade;
`--force` skips that.
