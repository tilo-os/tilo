# OS image size, 2026-10-10 (TIL-733, TIL-736, TIL-735)

A follow-up of [the TIL-609 receipt](os-image-size-2026-10-09.md), whose
"other redundant payload" table this change works down. The nightly records
`footprint.image.*` in `tilo-perf-results` hold the current values
(DEVELOPMENT.md, "OS image size").

## Method

`python3 tools/perf/image_size.py --image <ref> --arch <arch> --json … --markdown …`
over the production-layout `tilo-ci` images Build / OS dev published (LTO
off). "Before" is main's newest image of each architecture when the change
was measured; "after" is the pull request's image, built on main `e6fe675280`.
The raw output is in `os-image-size-2026-10-10.json`.

| image | revision | layers | compressed | payload | unique content | redundant payload |
|---|---|---:|---:|---:|---:|---:|
| `tilo-ci:main-9ce45bf-x86_64` | `9ce45bfda3` | 204 | 5,163,976,980 | 13,019,880,134 | 10,906,388,340 | 900,763,895 |
| `tilo-ci:pr-4055-7ea8ee3-x86_64` (TIL-733) | `7ea8ee3fe8` | 187 | 4,797,369,652 | 12,041,041,694 | 10,803,088,881 | 521,672,956 |
| `tilo-ci:main-e3915e1-aarch64` | `e3915e1a6f` | 204 | 4,280,887,035 | 11,203,584,810 | 9,053,757,209 | 942,141,456 |
| `tilo-ci:pr-4055-7ea8ee3-aarch64` (TIL-733) | `7ea8ee3fe8` | 187 | 3,917,464,742 | 10,212,326,641 | 8,970,543,576 | 570,623,814 |

All values are bytes.

| measure | x86_64 | aarch64 |
|---|---:|---:|
| compressed | −366,607,328 (−7.1 %) | −363,422,293 (−8.5 %) |
| payload | −978,838,440 | −991,258,169 |
| unique content | −103,299,459 | −83,213,633 |
| redundant payload | −379,090,939 | −371,517,642 |

## What changed

**One package layer.** The 18 package transactions of `rootfs-base` ran as a
RUN each, and each ended in `FINALIZE_RPMDB`, which rewrites `rpmdb.sqlite`
at a fresh inode: 20 layers carried a full 34–58 MB database, 956,600,320 B
of payload in all. They now run in one RUN, so three layers carry it (the
package RUN, the kernel swap, the final cleanup). The package layer is
1,319,324,706 B compressed, where the layers it replaces (72–93 of the x86_64
"before" image) were 1,586,096,443 B. The Plymouth PNGs render in their own
stage.

**No second initramfs.** The sealed image's `split` stage moved the kernel
and initramfs into `/kernel`, a whiteout plus a fresh 114 MB layer beside the
UKI that embeds both. A throwaway `kernel` stage now feeds the UKI, and the
shipped `/kernel` keeps only `vmlinuz`: the split layer went from 113,891,084 B
to 14,176,892 B compressed.

The "before" images are main's newest when measured, a few commits behind
the change's base; none of those commits touched `os/`.

## Still redundant (x86_64, after)

| what | bytes |
|---|---:|
| Proton's copies of `/usr/share/wine` files and its own duplicates (TIL-736) | 179,785,563 |
| env-runtime files in `/usr/share/tilo-apps/pkgs` (TIL-735) | 170,552,368 |
| the base's ostree objects | 39,814,868 |
| the runtime bundle's duplicates (TIL-736) | 22,130,558 |
| `/etc/selinux/targeted/active` | 14,528,753 |
| `/usr/lib/udev/hwdb.bin` (`/etc/udev/hwdb.bin` in the base) | 14,372,567 |
| `/kernel/7.2.8/vmlinuz` (whited-out original in the kernel-swap layer) | 14,299,648 |

## TIL-736: one inode per identical file

`os/scripts/hardlink-identical.py` hard-links the identical files of three
trees, each in the RUN that writes it, before the COPY that carries it into
the rootfs. The build logged 73,094,460 B under Proton, 53,114,139 B under the
app store's packages (the env-runtime's glibc locales) and 11,746,503 B under
the runtime bundles. Measured on the pull request's x86_64 image
(`tilo-ci:pr-4064-84e57e4-x86_64`, revision `84e57e4ca5`, built before TIL-733
landed) against the same "before" image, in bytes:

| measure | before | after | change |
|---|---:|---:|---:|
| compressed | 5,163,976,980 | 5,123,013,161 | −40,963,819 |
| payload | 13,019,880,134 | 12,881,851,920 | −138,028,214 |
| content | 11,337,321,341 | 11,199,293,127 | −138,028,214 |
| unique content | 10,906,388,340 | 10,906,315,228 | −73,112 |
| redundant payload | 900,763,895 | 762,808,793 | −137,955,102 |

Unique content counts identical files once already, so it barely moves; the
pull and the deployed content shrink. The pull request built x86_64 only, so
the aarch64 ceiling is left to the nightly records.

## TIL-735: the env-runtime from the host's files

The app store's env-runtime ships without the files the host already has,
byte for byte: the `app-store` stage leaves them out and names each in the
image's `host-sources`, and package-store imports them from the host, every
byte checked against the image's signed hash. The build logged
`2230 files, 167741166 bytes taken from the host`; the rootfs's final check
put back 0 bytes. Measured on `tilo-ci:pr-4072-e90b144-x86_64` (revision
`e90b144fa5`) against main with TIL-733 and TIL-736
(`tilo-ci:main-e88a45c-x86_64`, three commits later, none touching `os/`), in
bytes:

| measure | before | after | change |
|---|---:|---:|---:|
| compressed | 4,755,833,467 | 4,711,047,974 | −44,785,493 |
| payload | 11,903,191,234 | 11,789,027,884 | −114,163,350 |
| content | 11,096,171,422 | 10,982,008,072 | −114,163,350 |
| unique content | 10,803,193,523 | 10,803,689,053 | +495,530 |
| redundant payload | 383,717,854 | 269,058,974 | −114,658,880 |

Unique content grows by the `host-sources` listing. The pull request built
x86_64 only.

## Together (x86_64)

From the TIL-609 follow-up's "before" (`main-9ce45bf`) to the image with all
three changes: compressed 5,163,976,980 → 4,711,047,974 B (−452,929,006,
−8.8 %), redundant payload 900,763,895 → 269,058,974 B.
