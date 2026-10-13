# OS image size and update size, 2026-10-10: content-based layers

A follow-up of [the #4055, #4072 and #4064 receipt](os-image-size-2026-10-10.md).
Release / OS now re-lays every generic image it publishes out by content
with chunkah ("Rechunk the image", docs/measurements/os-image-size.md, "Content-based layers").
The raw image of the same build stays in the registry as
`tilo-ci:<series>-<arch>-layered`, so both layouts of one tree can be measured
side by side.

## Method

`python3 tools/perf/image_size.py --image <ref> --arch <arch> --json … --markdown …`
over the two images of Build / OS dev run 92862 (pull request #4111, revision
`27042bba49`, LTO off), and the step's own comparison report
(`rechunk-report-<arch>` artifact). The raw output is in
`os-image-size-2026-10-10-rechunk.json`. Updates are measured with
`image_size.py --update-from OLD --update-only`: the compressed layers of the
new manifest whose digests the old one does not have, which is what bootc
downloads.

## Update size

Two Build / OS dev builds (x86_64, runs 93859 and 94090, dispatched on a
throwaway branch): main `8e6975576c`, then that commit with one app changed
(`apps/calculator`, `HISTORY_CAP` 64 → 65). Each build pushed both layouts.

| update `8e69755` → `824e3c2` (one app changed) | new layers | download | share of the image |
|---|---:|---:|---:|
| layered (one layer per Containerfile instruction) | 118 of 187 | 3,689,501,680 B | 78.3 % |
| rechunked | 7 of 145 | **215,178,773 B** | **5.1 %** |

17 times less. Other pairs measured the same way:

| update | layered | rechunked |
|---|---:|---:|
| pull request `27042bb` → `cd9ed1c` (receipt and ceilings only; nothing the image reads) | 3,689,436,177 B (78.3 %) | 215,088,007 B (5.1 %) |
| main `5f091b5` → `927bede` (seven merges, 74 files: SDK, packaging, browser, notes) | 3,689,873,407 B (78.3 %) | 689,770,510 B (16.2 %) |

What the one-app update downloads: five layers of app binaries and app
packages (28 to 42 MB each), the 41 MB layer of Tilo's small files, and the
directory layer (0.25 MB). The calculator's own change is inside one of the
five; the other four and most of the small-file layer change in every build,
because the app packages are not reproducible: each `.tapp` under
`/usr/share/tilo-apps/cohort` has new bytes at the same size, and each
env-runtime package under `/usr/share/tilo-apps/pkgs/<hash>/` gets a new hash
directory around an identical binary. That is why an update that changes
nothing the image reads costs the same 215 MB. With reproducible app packages
a one-app update would be about one app layer.

The layered image re-sends 78 % in every case: every layer from the package
transaction on is a new blob in each build, whatever changed.

## Install and boot

Release / OS refused a dispatch on main (93311) before building, by its own
rule: the pinned app collection must match the OS commit. Dev / VM acceptance
installed and booted main's dev disk built FROM the first rechunked main image
(`5f091b55c1`, run 93675, `swap=none`): 52 of 67 checks pass. The same suite on
the last main dev image before the change (`8a7f570a0d`, run 93886): 48 of 67.
Every check that fails on the rechunked image fails on the earlier one too
(A07 A09 A13 A14 B03 B05 B06 B07 R02 R05 U02 U03 W16 W19 W20: Xwayland, the
on-screen keyboard, display and Bluetooth hardware, GPU rendering and
animation pacing on runners without a GPU); R01 R04 R06 W17 failed only on the
earlier one.

## Size (same tree, both layouts)

| image | layers | compressed | payload | unique content | undeployed | redundant payload |
|---|---:|---:|---:|---:|---:|---:|
| `tilo-ci:pr-4111-27042bb-x86_64-layered` | 187 | 4,712,267,878 | 11,791,003,092 | 10,805,664,261 | 545,556,816 | 269,058,974 |
| `tilo-ci:pr-4111-27042bb-x86_64` | 145 | 4,255,399,337 | 10,986,691,580 | 10,805,664,261 | 0 | 181,027,319 |
| `tilo-ci:pr-4111-27042bb-aarch64-layered` | 187 | 3,834,082,478 | 9,950,392,675 | 8,972,231,146 | 624,086,294 | 307,002,278 |
| `tilo-ci:pr-4111-27042bb-aarch64` | 145 | 3,405,132,197 | 9,062,932,496 | 8,972,231,146 | 0 | 90,701,350 |

All values are bytes.

| measure | x86_64 | aarch64 |
|---|---:|---:|
| compressed | −456,868,541 (−9.7 %) | −428,950,281 (−11.2 %) |
| payload | −804,311,512 | −887,460,179 |
| unique content | 0 | 0 |
| undeployed (`/sysroot`'s ostree objects) | −545,556,816 | −624,086,294 |
| redundant payload | −88,031,655 | −216,300,928 |

Unique content does not move: it is the same tree. What a full pull no longer
downloads is everything no installed system reads: the base's ostree
repository under `/sysroot`, the originals later layers whited out or
replaced (the base's kernel 7.2.7, the kernel swap's vmlinuz, rpm databases,
botocore), and the raw layers' zstd level 1, replaced by gzip level 6.
`content` grows by 2.7 MB (x86_64) because 44 hard-link groups now span two
layers and are two files each. The ceilings in `tools/perf/kpi-budgets.toml`
are lowered by the compressed savings.

## What the comparison proved

`os/scripts/oci-rechunk.py` replayed both images on each lane and refused
anything but these differences:

| | x86_64 | aarch64 |
|---|---:|---:|
| paths equal (type, mode, owner, xattrs, size, bytes, target) | 109,800 | 101,811 |
| file times capped lower by chunkah | 5,723 | 5,690 |
| directory times restored by the directory layer | 12,174 | 10,860 |
| `/sysroot` paths dropped | 76,322 | 80,472 |
| hard-link groups split across layers | 44 | 44 |
| file capabilities restored from the raw layers | 8 | 8 |

The first CI run (92464) refused the rechunk: BuildKit's snapshot of the
image, which chunkah reads, lacks the `security.capability` of the base
image's `arping`, `clockdiff`, `newuidmap`, `newgidmap` and four sssd helpers.
Those entries now take their xattrs from the raw image's layers.

The step costs 6 to 10 minutes per lane: chunkah 2 to 5 (including its
compile when the builder has no cache of it), the proof 2.5 to 3.5 (both
images replayed and hashed), the push 1.5 (only blobs the registry lacks).

## Layout

chunkah's "unclaimed" component (files under 1 MB that no package owns) was
one 289 MB layer, 841 of its 961 MB Proton's and the Steam runtime's files.
Those now have a chunkah run and 16 layers of their own
(`os/scripts/rechunk-chunkah.py`), and the unclaimed layer that Tilo's own
small files change with almost every build is 41 MB.

The largest layers (x86_64): Wine (463 MB), linux-firmware (297 MB), the
virtio-win ISO (253 MB), Proton's and the Steam runtime's small files
(224 MB), the kernel (146 MB), Proton DLLs (140 MB), the UKI (116 MB),
wine-mono, libcef.so, shell (108 MB), wine-gecko, libruntime_dylib.so
(102 MB). Package layers kept their digests across the two pull request
builds, 54 main commits apart (Wine, linux-firmware, the kernel, the ISO).
