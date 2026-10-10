# OS image size, 2026-10-09

This is a historical investigation. The nightly records
`footprint.image.*` in `tilo-perf-results` hold the current values
(DEVELOPMENT.md, "OS image size").

## Method

Each image was measured with
`python3 tools/perf/image_size.py --image <ref> --json … --markdown … --why python3-botocore`.
The tool streams every layer from registry.tilo.so, checks each blob against
its digest and replays the layers with their whiteouts. It counts the content
bytes of the merged root, deployed by install, with each hard-linked file
counted once. The images are the production-layout `tilo-ci` builds (LTO
off, as Build / OS dev builds them). The raw output is in
`os-image-size-2026-10-09.json`.

| image | revision | compressed | content | unique content | undeployed `/sysroot` | redundant payload |
|---|---|---:|---:|---:|---:|---:|
| `tilo-ci@sha256:a14338ea…` x86_64 (issue start) | `db91126b44` | 5,184,164,673 | 11,455,711,780 | 10,915,976,405 | 419,296,484 | 1,012,013,356 |
| `tilo-ci@sha256:d7d38d3d…` aarch64 (issue start) | `db91126b44` | 4,304,306,967 | 9,528,195,578 | 9,067,894,573 | 497,786,155 | 1,053,367,734 |
| `tilo-ci:main-f238447-x86_64` (this change's base) | `f238447596` | 5,238,105,634 | 11,576,609,411 | 11,036,874,036 | 419,296,484 | 1,012,034,765 |
| `tilo-ci:pr-4003-b15688e-x86_64` (this change) | `b15688ee1c` | 5,168,289,107 | 11,340,619,892 | 10,909,686,891 | 545,556,816 | 900,765,406 |

All values are bytes.

- **unique content** counts identical files once. It is what composefs stores
  after install.
- **undeployed** is the base image's own ostree repository under
  `/sysroot/ostree/repo`, which bootc does not deploy. It is the 419 MB
  that `os-runtime-current-main-2026-10-06.md` found.

The issue quoted 11,006,083,124 B of unique content. That figure is the
receipt of 2026-10-06 for an older image, and its scan counted
`/usr`, `/etc`, `/boot` and `/opt`. This one counts the whole deployed root.

### Against its own base

The change is measured against the main commit it is built on (`f238447596`):

| measure | change | why |
|---|---:|---|
| compressed | −69,816,527 B (−1.3 %) | the font catalog's second and third copies leave the OCI layers |
| content | −235,989,519 B | −108.8 MB of font copies, −126.3 MB of the AWS SDK |
| unique content | −127,187,145 B | the AWS SDK; composefs already stored the font copies once |
| redundant payload | −111,269,359 B | the font copies |
| undeployed | +126,260,332 B | the SDK's base-image bytes stay in the base's `/sysroot` objects |

## Findings

**Windows compatibility is most of the size.** These are the largest
deployed directories on x86_64:

| directory | bytes |
|---|---:|
| `/usr/lib/tilo` (mostly `windows-runners`: Proton and the Steam runtime) | 2,212,151,903 |
| `/usr/lib64/wine-wow64` | 1,499,511,780 |
| `/usr/share/virtio-win` | 909,015,923 |
| `/usr/share/wine` | 442,194,824 |
| `/usr/x86_64-w64-mingw32/sys-root` | 194,127,564 |

The largest packages are `wine-core` (1,420,588,763 B) and `wine-mono`
(318,703,141 B). aarch64 carries `/usr/lib/tilo/windows-runners` and
`/usr/share/virtio-win` as well. A later change moves this stack to a downloaded
system extension.

**The font catalog was in three layers.** The same 54.4 MB was in the runtime
bundle's `lib/fonts`, in `/usr/share/tilo/font-catalogs/<sha>` and in
`/usr/share/fonts/tilo`. The `runtime-image-payload` stage now hard-links the
two later names to the runtime's files in one layer. In this change's image,
no font file appears in the redundant payload.

**python3-botocore (122,751,302 B) is not a Tilo requirement.** `--why` traces
the chain:

```
python3-botocore <- python3-s3transfer requires (python3.14dist(botocore) < 2~a0 with python3.14dist(botocore) >= 1.37.4)
  python3-s3transfer <- python3-boto3 requires (python3.14dist(s3transfer) < 0.20~~ with python3.14dist(s3transfer) >= 0.19)
    python3-boto3 <- sos recommends python3-boto3
      sos: nothing requires it (installed by name)
```

`sos` and the SDK both come from the fedora-bootc base. The SDK is in the
base's rpm-ostree chunk layers 1 and 39; no Tilo package transaction names
either.
Removing the SDK with `rpm -e` shrinks the installed root. It cannot shrink
the pull, because the base layers and their `/sysroot` objects still carry
the bytes. Only a smaller base would.

**Other redundant payload the report shows, not changed here (x86_64):**

| what | bytes |
|---|---:|
| the rpmdb rewritten by every package layer | 247,832,576 |
| Proton's own wine-mono and gecko beside Fedora's | 179,785,563 |
| system libraries copied into `/usr/share/tilo-apps/pkgs` | 170,552,368 |
| initramfs and vmlinuz in both `/kernel/<v>` and `/usr/lib/modules/<v>` | 116,184,284 |
