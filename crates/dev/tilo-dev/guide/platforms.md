# Platforms

A dev VM runs the host's native architecture by default. Emulating another
architecture needs `--arch <arch> --emulated` and is many times slower.

| Host | Adapter | File sync for in-VM builds | GPU in guest | Other-arch emulation |
|---|---|---|---|---|
| Linux x86_64 | libvirt (`virt-install`, `virsh`); raw `qemu` via `os/dev/vm.sh` | virtiofs | virgl, else llvmpipe | aarch64 via TCG (slow) |
| Linux aarch64 | libvirt; raw `qemu` | virtiofs | virgl, else llvmpipe | x86_64 via TCG (slow) |
| macOS arm64 | UTM (`utmctl`, Apple Virtualization) | virtiofs | none (llvmpipe) | x86_64 via UTM's QEMU backend (slow) |
| Windows x86_64 | Hyper-V (PowerShell) | rsync over SSH | none (llvmpipe) | not supported |
| Windows arm64 | Hyper-V (PowerShell) | rsync over SSH | none (llvmpipe) | not supported |

Headless and vkms slots need no GPU, so every row runs them.

## Registering a VM

```sh
tilo-dev vm create dev --image ~/images/tilo-dev.qcow2 --disk ~/vms/dev.qcow2
tilo-dev vm use dev            # this worktree (writes .tilo-dev)
tilo-dev vm use dev --global   # the default for every worktree
```

`vms.toml` sits in the platform config dir (`~/.config/tilo-dev/` on Linux,
`~/Library/Application Support/tilo-dev/` on macOS, `%APPDATA%\tilo-dev\` on
Windows); `tilo-dev vm list` prints its path. UTM cannot import disks from the
command line: import the image once in UTM and name it as `template`.
Building a component on the host (`swap --from worktree`) needs a Linux host.
