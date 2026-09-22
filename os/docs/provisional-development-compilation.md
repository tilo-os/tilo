# Retained development compilation

`development-compile.yml` is a manual, feature-branch-only x86-64 job. Its default
`cache-only` mode tests remote compiler-cache access and stops before creating
signing identities or compiling the OS. Select `full-build` explicitly to invoke
the `os/dev/build-image.sh` development path with browser support enabled
and all applications and desktop components compiled from source. Supply the
reviewed immutable x86-64 WPE buildroot candidate, its completed WPE stage JSON
receipt and an explicit positive test generation. The committed base, RPM
snapshot and kernel snapshot remain inputs. The full-build job imports the
completed WPE artifact; a missing or mismatched receipt fails without compiling
WebKit as a fallback.

## Reuse and timing

Run `development-wpe-stage.yml` only when a matching completed artifact is not
available. Select the native architecture and reviewed immutable WPE buildroot;
the tools builder must also support that architecture. This producer uses six
jobs on the native `tilo-package` class, with an explicit 23 GiB container limit.
It restores and checksum-verifies only the three WPE source archives, saving
them before compilation so a later failure does not require another download.
The archives are shared across architectures; compiled stages are architecture
specific. Completed layers also use the existing development registry cache.

The producer records measured phase durations, memory and available space. Its
30 GiB admission floor, 8 GiB stop reserve and 155-minute duration guard are
experiment bounds, not proof that WebKit fits or a predicted build duration.
Only bounded compiler diagnostics and public receipts are retained. Native
transport run 7030 proved cold registry restoration, payload-only projection
and skipping the original build stage on ARM and x86-64 using synthetic ELF
payloads; it did not compile or exercise WebKit.

After a successful producer run, supply the public `wpe-stage.json` contents
as `wpe_stage_receipt` to `development-compile.yml`. Keep its matching buildroot
as `wpe_candidate`. A normal application change can reuse the completed stage;
the consumer validates the current dependency hashes before accepting it.

The full build requires the existing S3 compiler cache to start successfully.
Credentials enter through secret mounts, never build arguments or image layers.
Run 7000 verified a remote hit after restarting the cache server with an empty
local cache, plus invalidation after changing a tracked compiler input. Its
11.85-second duration measures that tiny probe, not OS compilation. Inspect the
nested build's own sccache statistics to assess Rust reuse.

The WPE producer has a separate native Buildah registry cache and stable dependency
epoch. Source commits and development signing identities do not change that
epoch; recipe, asset and dependency changes still invalidate the stage. Both
native architectures passed fresh-store reuse and invalidation fixtures in run
6992. The full build uses the resulting immutable local stage ID. This proves
the cache mechanism; WebKit compilation and its warm duration remain to
be measured. The completed artifact contains only `/wpe`, so consumer pulls do
not transfer the producer's compiler and intermediate build layers. Its receipt
binds architecture, pinned buildroot, dependency epoch, recipe and source archive
hashes; unrelated application changes reuse that artifact. The recipe key
conservatively includes the entire OS Containerfile.

Build logs report individual phase durations and compiler-cache statistics,
including on failure. Missing Rust outputs fail with their names instead of
triggering another complete compile. The full-build path stops at the raw-rootfs
handoff and retains it before signing and sealing. Run 7025 passed the native
registry, signature and historical-layer audit fixtures on both architectures.
Each build must independently pass the scoped layer audit before publishing
to `code.podesta.ai/qos/tilo-development-rootfs`. Publication uses registry layer
deduplication instead of creating a second on-disk image archive. The audit scans
all historical layers for the supplied private material and PEM private keys;
it is not a universal secret detector. `retained-rootfs.json` binds the exact
registry digest to the independently validated build handoff.

The 170-minute full-build timeout is a ceiling inside the existing runner limit,
not a measured build duration. No measured ten-hour build justifies increasing
infrastructure timeouts. Use phase measurements and cache misses to choose the
next performance change.

## Capacity and evidence

**Capacity experiment:** `runner-disk-100g` provides 100 GiB; `runner-large`
provides only 50 GiB despite its larger CPU/RAM allocation. Run 6815 completed
the source build **with browser disabled** on the ordinary 50 GiB class, then
ran out of space while writing the additional OCI archive. This job does not
export an archive, but WebKit and native Podman storage have not been measured.
The 80 GiB free-space admission threshold is a deliberately conservative
experiment floor, not a calculated WebKit requirement or proof that a complete build
fits. After admission, the driver samples free space every five seconds and
terminates its own build at an 8 GiB reserve, retaining `capacity.json` for the
next capacity decision. The selected class has general-runner memory; Cargo
uses two build jobs, WPE is imported, and receipts record the visible cgroup/guest memory
limits and peak. A storage abort is a failed experiment, never a reason to
disable the browser or publish an incomplete image.

`admission-capacity.json` records the free, used and total bytes of both
private backing filesystems before input pulls and compilation. It is retained
even when admission or a later preflight fails; `capacity.json` starts only when
the compilation process starts. Neither receipt proves that a future build fits.

The privileged tools container receives an explicit 9 GiB memory limit with
swap disabled (`--memory=9g --memory-swap=9g`). It is a sibling of the workflow
job container, so the job's own limit does not constrain it. This ceiling leaves
an engineering margin within the selected class's 10 GiB DinD budget; it is not
a measured peak or proof that the build fits. The workflow wrapper and any
other nested containers share that outer budget. Memory receipts record the
tools container's cgroup limit and peak for subsequent sizing decisions.

This is compilation evidence, **not an installation image**. The retained rootfs
is unsealed. The job creates no disk, performs no VM acceptance and allocates no
production release generation.

The disposable Kata guest runs rootful Podman on a private Docker volume and
requires at least 80 GiB free backing storage after tooling setup. A tools image inherits the tested
immutable multi-architecture builder and therefore the workspace Rust toolchain;
Cargo metadata and the app-store contract run before compilation. Native x86-64
execution is mandatory. No host disk, TPM or firmware interface is supplied.

Every run generates independent EFI/PCR, Cosign and Ed25519 registry-owner
identities outside the checkout. The private keys are used only inside that
disposable container. Existing registry authentication permits input pulls and
development quarantine publication;
production signing material is not supplied. Cleanup removes the container and
its private storage. The artifact contains only public trust and input/tool/
workspace/result, layer-audit and retention receipts. A missing result receipt
means the complete workflow did not finish successfully; check the retention
receipt before rebuilding, because publication may already have succeeded.
Retention mounts the immutable raw image and checks embedded trust, origin,
generation and the kernel/initramfs pair. Loader signing and final sealing are
later completion steps. CI logs retain build diagnostics.

Before compilation, the pinned base's `bootc install to-disk --help` and bootc/
bootctl versions are captured in `base-installation-tooling.json`. The completed
raw image is probed by immutable image ID in `raw-installation-tooling.json`.
These read-only containers have no network, extra devices or host mounts and
drop all capabilities. Advertised flags help prepare the next disk test; their
presence does not prove signature-policy enforcement or successful installation.

Persistent development signing, a retained authenticated source image, a final
composefs seal, disk inspection and Secure Boot VM acceptance are separate gates
before an installable candidate exists. Do not reuse this job's ephemeral trust
for a retained installation or update channel.
