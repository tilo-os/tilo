# Platforms

A dev VM runs the host's native architecture by default. Emulating another
architecture needs `--arch <arch> --emulated` and is many times slower.

| Host | Adapter | Builds | GPU in guest | Other-arch emulation |
|---|---|---|---|---|
| Linux x86_64 | libvirt (`virt-install`, `virsh`); raw `qemu` via `os/dev/vm.sh` | VM or host | virgl, else llvmpipe | aarch64 via TCG (slow) |
| Linux aarch64 | libvirt; raw `qemu` | VM or host | virgl, else llvmpipe | x86_64 via TCG (slow) |
| macOS arm64 | UTM (`utmctl`, Apple Virtualization) | VM only | none (llvmpipe) | x86_64 via UTM's QEMU backend (slow) |
| Windows x86_64 / arm64 | Hyper-V (PowerShell) | VM only | none (llvmpipe) | not supported |

In-VM builds sync the worktree with rsync over SSH, so the host needs
`rsync` (on Windows: from WSL, MSYS2 or cwRsync on `PATH`). Headless and vkms
slots need no GPU, so every row runs them.

## Creating a VM

```sh
tilo-vm vm create dev                        # newest published main/release disk
tilo-vm vm create dev --tag main-1a2b3c4     # a specific one
tilo-vm vm create dev --image ~/tilo-vm.qcow2 --disk ~/vms/dev.qcow2   # a local disk
tilo-vm vm use dev            # this worktree (writes .tilo-vm)
tilo-vm vm use dev --global   # the default for every worktree
```

`vm create` downloads `tilo-vm-disk.json` for the tag from the forge's
generic packages, picks the disk for your architecture (qcow2, or VHDX for
Hyper-V), resumes an interrupted download, checks its SHA-256 and caches it.
It needs a forge token with package read access: `FORGEJO_TOKEN`, or an fgj
(`~/.config/fgj/config.yaml`) or tea login for code.sandbox.example. No disk
published yet? Build one locally with `os/dev/build-dev-image.sh` and pass it
as `--image`.

`vms.toml` sits in the platform config dir (`~/.config/tilo-vm/` on Linux,
`~/Library/Application Support/tilo-vm/` on macOS, `%APPDATA%\tilo-vm\` on
Windows); `tilo-vm vm list` prints its path. UTM cannot import disks from the
command line: import the downloaded disk once in UTM and pass its name as
`--template`.

`vm stop` waits for the VM lock, so it never cuts off a swap, gc or upgrade;
`--force` skips that.
