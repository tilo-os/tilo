# Full provisional development compilation

`development-compile.yml` is a manual, feature-branch-only x86-64 job. It invokes
the actual `os/dev/build-image.sh` development path with browser support enabled
and all applications and desktop components compiled from source. Supply the
reviewed immutable x86-64 WPE buildroot candidate and an explicit positive test
generation. The committed base, RPM snapshot and kernel snapshot remain inputs.

**Capacity experiment:** `runner-disk-100g` provides 100 GiB; `runner-large`
provides only 50 GiB despite its larger CPU/RAM allocation. Run 6815 completed
the source build **with browser disabled** on the ordinary 50 GiB class, then
ran out of space while writing the additional OCI archive. This job does not
export an archive, but WebKit and native Podman storage have not been measured.
The 80 GiB free-space admission threshold is a deliberately conservative
experiment floor, not a calculated WebKit requirement or proof that a complete build
fits. After admission, the driver samples free space every five seconds and
terminates its own build at an 8 GiB reserve, retaining `capacity.json` for the
next capacity decision. The selected class has general-runner memory; WebKit and
Cargo both use two build jobs, and receipts record the visible cgroup/guest memory
limits and peak. A storage abort is a failed experiment, never a reason to
disable the browser or publish an incomplete image.

This is compilation evidence, **not an installation image**. The local container
still has a provisional composefs seal. The job creates no disk, performs no VM
acceptance, publishes no image and allocates no production release generation.

The disposable Kata guest runs rootful Podman on a private Docker volume and
requires at least 80 GiB free backing storage after tooling setup. A tools image inherits the tested
immutable multi-architecture builder and therefore the workspace Rust toolchain;
Cargo metadata and the app-store contract run before compilation. Native x86-64
execution is mandatory. No host disk, TPM or firmware interface is supplied.

Every run generates independent EFI/PCR, Cosign and Ed25519 registry-owner
identities outside the checkout. The private keys are used only inside that
disposable container. Existing registry authentication permits input pulls;
production signing material is not supplied. Cleanup removes the container and
its private storage. The artifact contains only public trust and input/tool/
workspace/result receipts. A missing result receipt means compilation did not
finish successfully. Final inspection mounts the immutable local image, checks
embedded trust/origin/generation, verifies the original and signed EFI loader
against the independent public certificate, and records installed RPM versions.
CI logs retain build diagnostics.

Before compilation, the pinned base's `bootc install to-disk --help` and bootc/
bootctl versions are captured in `base-installation-tooling.json`. The completed
image is probed by immutable image ID in `final-installation-tooling.json`.
These read-only containers have no network, extra devices or host mounts and
drop all capabilities. Advertised flags help prepare the next disk test; their
presence does not prove signature-policy enforcement or successful installation.

Persistent development signing, a retained authenticated source image, a final
composefs seal, disk inspection and Secure Boot VM acceptance are separate gates
before an installable candidate exists. Do not reuse this job's ephemeral trust
for a retained installation or update channel.
