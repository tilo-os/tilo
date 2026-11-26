# Wine, patched

The x86_64 CI builder (`os/Containerfile.builder`) runs Inno Setup, which
compiles the imager's Windows installer, and the imager's Windows test
executables under Wine built from the pinned upstream source with the patches
here, instead of Fedora's package. `tools/ci/install-wine.sh` builds it in the
builder's `wine` stage.

- Upstream: <https://gitlab.winehq.org/wine/wine>, LGPL-2.1-or-later.
- Base: 11.20 (`wine-11.20.tar.xz` in `../github-assets.tsv`), the version
  Fedora 44 ships.
- Configured for WoW64 with i386 and x86_64 PE modules and without display,
  sound or device support: the programs run headless on Wine's null graphics
  driver (`tools/ci/windows/wine-prefix.sh`).

## 0001-cryptbase-size-the-random-buffer-for-every-processor.patch

Wine bug: <https://bugs.winehq.org/show_bug.cgi?id=54364>.

`RtlGenRandom` reads its bytes from `SystemInterruptInformation`, which fails
unless the buffer holds one entry per processor, but Wine sized the buffer for
at most 256. On a host with more than 256 logical processors every
`RtlGenRandom` failed, and with it `BCryptGenRandom` and `ProcessPrng`: Inno
Setup's own installer exited 1 before writing a log. The patch sizes the
buffer for the processor count Wine reports. Prepared for upstream, not sent.

## 0002-ntdll-describe-a-file-on-a-local-disk-as-windows-does.patch

On Windows a file on a local disk volume reports `FILE_DEVICE_DISK`, its NTFS
volume `FILE_SUPPORTS_POSIX_UNLINK_RENAME`, and a rename with
`FILE_RENAME_POSIX_SEMANTICS` may replace a target others hold open with
delete sharing. Wine reported every file's device as
`FILE_DEVICE_DISK_FILE_SYSTEM`, no POSIX rename, and refused to replace an
open target, so the identity store, which publishes only to a local disk it
can rename over, refused every Wine drive. The patch answers as Windows does
for ext2/3/4, xfs and btrfs, and for an overlay on one of those that honours
fsync; tmpfs, network and FUSE file systems keep Wine's answer. Prepared for
upstream, not sent.

Once a Wine release contains both fixes, install Fedora's package again and
delete this directory and the install script.
