# OS image size and update size, 2026-10-10: content-based layers (TIL-747)

A follow-up of [the TIL-733/735/736 receipt](os-image-size-2026-10-10.md).
Release / OS now re-lays every generic image it publishes out by content
with chunkah ("Rechunk the image", DEVELOPMENT.md "Content-based layers").
The raw image of the same build stays in the registry as
`tilo-ci:<series>-<arch>-layered`, so both layouts of one tree can be measured
side by side.

## Method

`python3 tools/perf/image_size.py --image <ref> --arch <arch> --json … --markdown …`
over the two images of Build / OS dev run 92646 (pull request #4111, revision
`a326521876`, LTO off), and the step's own comparison report
(`rechunk-report-<arch>` artifact). The raw output is in
`os-image-size-2026-10-10-rechunk.json`.

## Size (same tree, both layouts)

| image | layers | compressed | payload | unique content | undeployed | redundant payload |
|---|---:|---:|---:|---:|---:|---:|
| `tilo-ci:pr-4111-a326521-x86_64-layered` | 187 | 4,711,584,427 | 11,788,979,892 | 10,803,641,061 | 545,556,816 | 269,058,974 |
| `tilo-ci:pr-4111-a326521-x86_64` | 129 | 4,254,404,676 | 10,984,583,204 | 10,803,641,061 | 0 | 180,942,143 |
| `tilo-ci:pr-4111-a326521-aarch64-layered` | 187 | 3,833,482,775 | 9,948,056,243 | 8,969,894,714 | 624,086,294 | 307,002,278 |
| `tilo-ci:pr-4111-a326521-aarch64` | 129 | 3,404,475,838 | 9,060,239,081 | 8,969,894,714 | 0 | 90,344,367 |

All values are bytes.

| measure | x86_64 | aarch64 |
|---|---:|---:|
| compressed | −457,179,751 (−9.7 %) | −429,006,937 (−11.2 %) |
| payload | −804,396,688 | −887,817,162 |
| unique content | 0 | 0 |
| undeployed (`/sysroot`'s ostree objects) | −545,556,816 | −624,086,294 |
| redundant payload | −88,116,831 | −216,657,911 |

Unique content does not move: it is the same tree. What a full pull no longer
downloads is everything no installed system reads: the base's ostree
repository under `/sysroot`, the originals later layers whited out or
replaced (the base's kernel 7.2.7, the kernel swap's vmlinuz, rpm databases,
botocore), and the raw layers' zstd level 1, replaced by gzip level 6.
`content` grows by 2.6 MB (x86_64) because 42 hard-link groups (43 on
aarch64) now span two layers and are two files each.

## What the comparison proved

`os/scripts/oci-rechunk.py` replayed both images on each lane and refused
anything but these differences:

| | x86_64 | aarch64 |
|---|---:|---:|
| paths equal (type, mode, owner, xattrs, size, bytes, target) | 109,800 | 101,811 |
| file times capped lower by chunkah | 5,723 | 5,690 |
| directory times restored by the directory layer | 12,157 | 10,843 |
| `/sysroot` paths dropped | 76,322 | 80,472 |
| hard-link groups split across layers | 42 | 43 |
| file capabilities restored from the raw layers | 8 | 8 |

The first CI run (92464) refused the rechunk: BuildKit's snapshot of the
image, which chunkah reads, lacks the `security.capability` of the base
image's `arping`, `clockdiff`, `newuidmap`, `newgidmap` and four sssd helpers.
Those entries now take their xattrs from the raw image's layers.

## Layout

The largest chunked layers (x86_64): Wine (463 MB), linux-firmware (297 MB),
files no package owns (289 MB: Proton's and the Steam runtime's small files,
`/usr/share/tilo-apps`, Tilo's own small files), the virtio-win ISO (253 MB),
the kernel (146 MB), the UKI (116 MB), wine-mono, libcef.so, shell,
wine-gecko, libruntime_dylib.so, QEMU. Files over 1 MB that no package owns
(Tilo's binaries, Proton's DLLs) are packed together into the remaining
layers.
