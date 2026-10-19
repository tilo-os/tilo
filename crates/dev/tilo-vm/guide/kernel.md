# Changing the kernel

```sh
$EDITOR os/kernel/config-fragments/tilo.config   # or add os/kernel/patches/*.patch
tilo-vm swap kernel                  # build incrementally, install VM-wide, reboot, confirm
tilo-vm swap kernel --jobs 4         # fewer compile jobs (default: half the CPUs)
tilo-vm swap kernel --direct         # boot the host build directly (libvirt); installs nothing
tilo-vm swap kernel --direct --cmdline 'root=UUID=... ro console=ttyS0'   # when the VM is not up
tilo-vm rollback --slot vm           # back to the previous kernel, or off the direct boot
tilo-vm vm ssh -- uname -a           # what is running
```

`swap kernel` ends with the running kernel's release and build (`uname -r`,
`uname -v`) and the time each step took. The kernel is VM-wide: it goes into
the `vm` slot whether or not you pass `--slot vm`, and the reboot restarts
every slot's session.

## Same kernel as the image

`os/kernel/dev-build.sh` (what `swap kernel` runs; you can run it by hand)
builds the kernel the image ships, from the same inputs:

- the builder is `os/kernel/Containerfile`'s own `build-toolchain` stage:
  the pinned base and frozen RPM snapshot, tilo-crypto, and the Fedora
  kernel SRPM after `%prep`;
- every step on the source tree is `os/kernel/build-support.sh`, which the
  Containerfile runs too: `prepare-source` (patches, the libcrypto-free
  signing shim, `tilo.config` + `<arch>.config` merged with
  `merge_config.sh`, `olddefconfig`, every required-option check), `compile`
  and `stage-modules` (vmlinuz, modules stripped and signed with
  tilo-crypto, `depmod`);
- the release build's environment (`KBUILD_BUILD_*`, `TZ`, `LC_ALL`).

Two differences, both deliberate: the module signing key lives as long as
the cache instead of one build, and the release carries a `+dev` suffix
(`7.2.5-200.fc44.x86_64+dev`). The suffix keeps the dev kernel's modules
out of the image kernel's `/usr/lib/modules/<release>/`: that kernel only
trusts its own key, and it has to stay bootable as the fallback.
`tools/policy/check_kernel_dev_build.py` (in `tools/policy/run.sh`) fails
when the two builds could diverge.

## Incremental

The first build copies the prepared source tree into
`~/.cache/tilo-kernel/<linux version>/` (override: `TILO_KERNEL_CACHE`) and
compiles everything: that is the long one. The tree and its objects stay, so
the next build recompiles only what the change touched: a config line
rebuilds the objects that depend on that symbol, a patch the files it
changes (the old patch set is reversed first). Mind what a symbol reaches:
turning off a leaf driver rebuilds nothing, but one that changes a core
structure (`CONFIG_IFB` selects `NET_REDIRECT`, a `struct sk_buff` field)
rebuilds most of the network stack. Every build re-installs and re-signs
all ~4,500 modules, so even a build with no change takes about a minute and
a half. Signing is deterministic: the same inputs give the same layer, and
swapping it again does nothing. The builder image is rebuilt only when its
inputs change (the Containerfile, the pins, tilo-crypto's sources).

Memory: each compile job needs up to about 1 GB. The default is half the
CPUs, because the host usually runs other builds; `--jobs` or
`TILO_KERNEL_JOBS` overrides it. The full log is
`~/.cache/tilo-kernel/<version>/build.log`.

## Into the VM

1. The staged `usr/lib/modules/<release>/` is synced to the VM (rsync, only
   what changed) and imported as a layer in the `vm` slot: a new generation,
   like any VM-wide swap. It is signed and merged as a signed layer by the
   system extension activation (`tilo-vm guide trust`), so the modules are
   in `/usr/lib/modules/<release>/`.
2. tilo-vmd builds the initramfs in the VM with the image's own dracut,
   against those modules, with the arguments `os/Containerfile` builds the
   image's initramfs with, plus overlayfs loaded early (`--force-drivers
   overlay`: the merge needs it to overlay the layer that carries it).
   vmlinuz and the initramfs go to
   `/boot/tilo-vm/<entry>/`, with a boot loader entry
   `tilo-vm-<release>-<layer>.conf` that takes the running command line.
3. That entry is the next boot only (`bootctl set-oneshot` on systemd-boot,
   `grub2-reboot` on GRUB; with no default set, the running entry is pinned
   as the default first). tilo-vm reboots the VM and waits for SSH.
4. When the VM answers on the new build, the entry becomes the default and
   the report prints. When it does not answer within 240 s, tilo-vm resets
   the VM: the old default boots, and the error says so.

The previous default entry stays as the fallback, and the image's own
kernel entry is never touched. Modules load from the layer only after the
activation has verified and merged it at boot; a drop-in
(`tilo-extension-activate.service.d/50-tilo-vm-kernel.conf`) then re-triggers udev
and `systemd-modules-load`, so drivers requested earlier (the virtio NIC,
vkms, uinput) load. Drivers the root filesystem needs are built in or in the
initramfs.

## Rolling back

`tilo-vm rollback --slot vm` moves the `vm` slot to its previous generation.
When that changes the kernel, tilo-vmd points the next boot at the previous
kernel's entry (reusing its initramfs) or, back at the base generation,
makes the image's entry the default again and removes every `tilo-vm-*`
entry; tilo-vm reboots and confirms as above. After a direct boot, the same
command boots the VM's disk again instead.

A swapped kernel's entry carries the command line of the deployment it was
installed from. Put the image's kernel back (`tilo-vm rollback --slot vm`
down to the base generation) before `tilo-vm vm upgrade`, or the VM keeps
booting the old deployment through that entry.

A failed boot falls back to the previous default entry. If that is an
earlier dev build of the same release, its modules may not match the
current layer's: `tilo-vm rollback --slot vm` puts them back in step. GRUB
can only clear a one-shot entry where it can write its environment (not a
plain `grubenv` file on btrfs; an `env_block` works); where it cannot, the
swap warns, and a kernel that never comes up keeps being booted until the
entry is removed (a direct boot gets you in: see below).

## Direct boot (`--direct`)

For pure boot testing: libvirt boots the host-built vmlinuz with a generic
initramfs (the builder base's dracut, plus virtio_net) and the VM's current
kernel command line (the domain's `<kernel>`, `<initrd>` and `<cmdline>`).
When the VM is not up to read it from, give it with `--cmdline`: that also
gets you into a VM whose installed kernel no longer boots.
Nothing is installed: modules outside that initramfs are absent in the VM,
and the image's initramfs features (encryption, tilo-rollback) are not
there. Use it to see whether a kernel boots at all; use `swap kernel` for
everything else. The VM keeps booting that kernel until
`tilo-vm rollback --slot vm`.

## What it needs

- a Linux host with rootless podman (the builder image, about 6 GB, plus
  the cache: the tree and its objects take about 5 GB, the staged modules
  about 120 MB);
- a VM that boots with systemd-boot (the Tilo dev image) or GRUB with BLS
  entries in `/boot/loader/entries`, with `tilo-extension-activate.service`
  enabled (the dev image does);
- for `--direct`: the libvirt adapter.

Other boot components (initramfs on its own, UKI sealing, bootloader,
installer, LUKS/TPM) are not swappable: `tilo-vm swap` names their scenario
driver instead (`os/dev/test-*.sh`).
