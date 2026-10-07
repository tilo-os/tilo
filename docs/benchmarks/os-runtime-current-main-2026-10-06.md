# Current-main OS runtime and installed footprint investigation

The candidate reduces the physical Btrfs root of a complete bootc installation
from 7,525,588,992 to 7,408,070,656 bytes: **117,518,336 bytes (1.56%)**.
This is one matched pair using the same immutable image and installer settings,
with assembly compression changing from zstd level 1 to 3. It is a footprint
improvement, not evidence of faster interaction or hardware frame presentation.
[Raw observations and provenance](os-runtime-current-main-2026-10-06.json)
retain the distinct image, filesystem and artifact measurements.

## Source and scope

The latest completed, non-skipped main x86 image found at acquisition was
source `60f484b76cbde5304535beaac369db7ad5084434`, built by
[main x86 run 72342](https://code.tilo.so/tilo/tilo/actions/runs/72342).
Its production-layout integration image is pinned to
`registry.tilo.so/tilo/tilo-ci@sha256:f3dd831dd2e39860abab11f6f9b0444c7d94cc3449ae059bcf8eb9d5df6579fe`.
The OCI config revision matches that source. The image declares LTO off;
it is **not a signed shipping thin-LTO release**. Later successful workflows
with skipped image-building steps did not publish their apparent tags.

The matching developer image and disk were acquired through the existing
[disk workflow, run 72556](https://code.tilo.so/tilo/tilo/actions/runs/72556).
The candidate manifest explicitly declares itself unvalidated. Its checksum,
source, base digest and prebuilt developer-image digest were checked before
booting a private writable child. That developer guest includes compilers and
test applications, so its installed size is separate from the production layout.

## Current-main refresh after artifact publication

After the matched experiment, [OS build 72677](https://code.tilo.so/tilo/tilo/actions/runs/72677)
published source `12f4dfb2d535f0ea9c64278b66a9bef435d2e4b3`. The exact image is
`registry.tilo.so/tilo/tilo-ci@sha256:7a733a3f1a9fc79ca96f61a667f1b813c91251e822d93a999a5a9c240984cbce`.
Its raw manifest/config digests and source revision were independently verified.
It declares LTO off. [Signed release 72696](https://code.tilo.so/tilo/tilo/actions/runs/72696)
is `release-apps.yml`, not a signed optimized OS image release.

A fresh complete installation at main's level-1 assembly compression used
**7,536,689,152 root bytes**, with fs-verity verified on all 76,338 objects.
Its losslessly checked zstd qcow2 is **5,976,621,056 bytes**; the uncompressed
qcow2 is separately recorded at 8,053,063,680 bytes. These are different disk
encodings, not different installed roots. Compressed OCI layers total
5,045,428,358 bytes.

A fresh private child reached an input-ready greeter in **81.671 s** with the
same four-vCPU, 4096-MiB TCG fixture. The greeter reported the exact expected
`e1a2f7a…711a7745d` composefs digest. A previous final probe used the unsupported
`state-dump` subcommand; its boot/wait results and failed probe are retained,
and a fresh child rerun completed with the real client's `state` command.
Both guests are stopped, mounts detached and the redundant raw disk removed.
No account was provisioned. This source refresh is an absolute baseline,
not a matched comparison against the earlier source or a hardware-speed claim.

## Complete installed comparison

The production-layout image's placeholder UKI digest was refused by bootc.
The existing `reseal-image.sh` recovery added a deterministic **UKI-only** layer
with the exact digest install requested. Both successful installations used
that same private image ID, `sha256:1853812a965dc7832c2af311d6e7a2f801633447c8a6fff03195bbdaefacc6de`.
Neither artifact is promoted to a trusted release by that recovery.

Both disks used `disk-image.sh --composefs --size 24G --rootfs btrfs --raw`.
The baseline ran the unmodified assembly mount at level 1; the candidate ran
level 3. Both completed installation with **76,338 of 76,338 objects carrying
fs-verity**. Physical use was read from `btrfs filesystem usage -b` through
read-only, no-log-replay loop mounts, before any guest boot. It includes root
data, duplicated metadata and fs-verity overhead; the EFI partition is separate.
Reserved block groups and virtual disk capacity are not counted as used data.

The losslessly converted, opt-in zstd-compressed qcow2 artifacts are 5,948,375,040 and 5,841,944,576
bytes, a **106,430,464-byte (1.79%)** reduction. `qemu-img check` passed on
both; `qemu-img compare -f raw -F qcow2` verified that each conversion preserves
its entire raw disk. This is a complete disk-artifact measurement, distinct
from the root partition use above.

The first whole-object fingerprint comparison failed. Investigation found
76,336 identical objects and two differing generated OCI manifest SplitStream
wrappers. Each wrapper's decoded data stream, content-object reference, full
set of stream references and all 204 resolved name-to-verity references match
its counterpart. Their reference-array order and corresponding numeric indices
differ. The raw report preserves the failed bytewise result alongside those
successful semantic checks, rather than discarding the differing objects.
[The upstream SplitStream format](https://github.com/composefs/composefs-rs/blob/main/doc/splitstream.md)
defines the indexed reference maps used in that verification.

The assembly mount setting ends at unmount. The inherited compression algorithm
and the image's boot-time mount policy remain intact. Btrfs documents the
[dataset-dependent cost of compression levels](https://btrfs.readthedocs.io/en/latest/Compression.html);
this change does not select higher compression for runtime writes. Single-run
assembly wall times are retained but are not an install-speed comparison:
cache order and other work on the shared runner were uncontrolled.

## Boot and idle observations

The runner lacks `/dev/kvm` and a DRM render node. Every guest used explicit
multithreaded TCG, four vCPUs, 4096 MiB and virtio 2D at 1280×720. No physical
device was rebooted or deployed to. These are software-emulation observations.

Both complete production-layout installations reached an input-ready greeter
through the console: 83.021 s at assembly level 1 and 78.312 s at level 3. Each
reported the same expected composefs digest. These are one run each on a shared
host, so they validate bootability and readiness, not a boot-speed improvement.
Neither guest had an account provisioned, and both were stopped after testing.

The current developer guest reached systemd's graphical target after 36.995 s
of kernel, initrd and userspace time. Its greeter accepted input after 81.430 s
from the host's immediate pre-boot timer, including firmware and UI startup.
The image's linker-cache condition correctly skipped `ldconfig.service`.
The older October 2 pilot is not a matched before/after comparison.

After ordinary passcode sign-in, the real shell and session services ran.
Six uninterrupted five-second desktop-idle intervals measured **0.938% mean
aggregate guest CPU capacity** and **884.7–889.2 MiB unavailable RAM**.
The SSH/Python observer is included; unavailable RAM is not process PSS.
The greeter window sampled earlier still contained background activity and is
explicitly excluded from steady-idle conclusions.

The ordinary Terminal action returned success in 62.8 ms but created an
`AppPlaceholder` while first-login setup remained pending. It is not a successful
app launch, startup timing or presentation result. No identity enrollment or
new access grant was used to bypass that state.

`vm.sh wait` previously required a greetd serial message or development SSH
before asking the console whether the app was ready. It now observes the
authoritative console directly, with bounded probes, so quiet production guests
can qualify without either prerequisite. Tests exercise the real script and
console client over private sockets: ready greeter, answering installer and an
unready greeter behind an active greetd. The two quiet-console cases fail at
the base commit and pass with the fix.

## Footprint costs and rejected changes

A complete read-only scan of `/usr`, `/etc`, `/boot` and `/opt` found
11,006,083,124 bytes of unique regular-file content with no unreadable paths.
Compressed OCI layers total 5,039,589,382 bytes. Neither is physical installed
filesystem use. The development guest used 9,769,893,888 physical Btrfs bytes.

The largest single file is the 877 MB VirtIO driver ISO, required by the
Windows guest setup path. Lossless compressed qcow2 shrank its logical length
to 282 MB, but outer zstd compression gave 246 MB for the raw ISO versus
282 MB for the qcow2. Repackaging therefore worsens transfer size and is not
shipped. Driver signatures, tools and licences remain intact.

The large native binaries retain function-name symbol tables by explicit
release policy. Removing them would sacrifice field crash diagnosis. Wine's
large runtime dependencies are real package requirements; they were not
removed as presumed build-only clutter.

A follow-up scan found 419,257,061 additional unique content bytes under the
image's `/sysroot/ostree/repo`. The repository has no refs. A disposable derived
image deleting only that embedded directory passed bootc lint and complete
installation, calculating the same composefs root digest. However, its installed
object store still contained 76,339 objects and 11,972,689,826 logical bytes:
only about 13 MB below the previous level-3 install, despite removing 419 MB
from the final image view. Physical root use was 7,386,718,208 bytes, 21.4 MB
below that install, with a changed image/layer graph and no repeatability proof.
An upper-layer deletion does not establish recovery of historical OCI payload.
This cleanup is not shipped or counted as savings. The physical system root
repository used for deployment and upgrades remains untouched.

Native app startup, hardware input-to-present latency and active hardware
frame cadence remain unmeasured. A shipping-release comparison needs its
verified optimized artifact and an approved, provisioned app session. The
existing Yoga runtime work owns physical-device measurements. The earlier
[SHM conversion improvement](../shm-conversion-benchmark.md) is a separate
matched CPU benchmark, not proof that the full current OS beats Clear Linux.

## Validation follow-up

Exact-head OS image run [72823](https://code.tilo.so/tilo/tilo/actions/runs/72823)
failed on Music packaging: 11,396,096 bytes exceeded the existing 8,388,608-byte
limit. Required quality/affected checks passing did not establish image success.
This branch predates [the landed Music fix](https://code.tilo.so/tilo/tilo/pulls/2948),
which shares native Spotify playback through the runtime. The branch was rebased
onto that fix, source `12f4dfb2`, without changing the package-size gate.

A fresh COW of the verified source-`12f4dfb2` developer image also exercised a
private headless compositor and a private, non-activating D-Bus broker. Its
marked developer-only throwaway identity was created, and the public Terminal
action returned a successful receipt in 415.8 ms. No foreign surface appeared
within the 90-second observation window: the final tile remained
`AppPlaceholder`. This is a failed journey, not a startup measurement. The
isolated session logs show an unavailable secret store and filesystem authority;
the minimal fixture has not established all normal session prerequisites.
No security check was bypassed to force a frame. The private compositor and
guest were stopped and the writable COW discarded. Raw response, state and log
tail are retained in the JSON.

## Reproduction

Use the recorded immutable OCI digest, rather than a moving main tag. The
source image needs the repository's existing UKI reseal path before installation;
record the resealed image ID and composefs digest and reuse that exact image
for both runs. On a disposable Linux runner with native Btrfs and existing
loop/mount access, install sequentially with the old and candidate scripts:

```sh
os/dev/disk-image.sh "$PINNED_RESEALED_IMAGE" "$PRIVATE_OUTPUT" \
  --composefs --size 24G --rootfs btrfs --raw
```

Read root use before either boot: attach each raw disk with `losetup --partscan`,
mount partition 2 with `ro,rescue=nologreplay`, and run
`btrfs filesystem usage -b`. Record the `Used` value; detach every loop after
unmounting. Measure each composefs object's fs-verity identity with
`FS_IOC_MEASURE_VERITY`, compare the complete object lists, and decode any
differing SplitStream wrappers using the referenced upstream format. Do not
infer equality from a sampled binary or silently exclude differing objects.

Convert each full disk using the existing opt-in zstd qcow2 path
(`TILO_QCOW2_COMPRESS=1`, equivalent to `qemu-img convert -f raw -O qcow2
-c -o compression_type=zstd`), run `qemu-img check`, then compare it to its
raw input with `qemu-img compare -f raw -F qcow2`.
For readiness, run the existing `vm.sh boot` and `vm.sh wait` with the fixture
settings in the JSON, start a monotonic host timer immediately before boot,
and retain the console `state-dump`. Stop each guest before starting the next.
Production input readiness is `/accepts_input`, not a serial log milestone.
