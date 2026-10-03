# System extensions and local models

Tilo treats optional machine software as signed desired state, not as an
unrestricted package-manager escape hatch. The first consumer is local agent
inference, but the mechanism also covers GPU userspace stacks and
kernel-coupled drivers.

## Extension classes

| Class | Examples | Activation | Rollback |
| --- | --- | --- | --- |
| Runtime pack | vLLM, CUDA/ROCm userspace, model gateway | A supervised, loopback-only service after artifact verification | Stop service and select the previous content-addressed runtime |
| System extension | Additive `/usr` or `/opt` payload with no early-boot dependency | Read-only systemd-sysext image matched to the Tilo release | Unmerge/mask the image |
| Boot pack | NVIDIA kernel modules, firmware, initramfs inputs | A new signed bootc deployment; reboot required | Normal Tilo deployment rollback |

Kernel modules are never hot-installed into the immutable root. A boot pack is
pinned to the exact base-image digest, architecture, and kernel ABI. It is
staged and measured like an OS update, so Secure Boot, boot-health confirmation,
and rollback remain meaningful. systemd-sysext is useful for additive userspace
files, but it is not the kernel-driver transaction mechanism.

## Trust and catalogue rules

The catalogue may describe artifacts, compatibility, display metadata, and
fixed typed launch fields. It may not contain scripts or arbitrary command
lines. Every accepted extension has:

- a signature from a key already trusted by the installed Tilo image (Ed25519
  release keys in `trust.d/` for BootPacks and runtimes; the OS update key
  itself for downloaded system extensions, see below);
- an HTTPS retrieval location, exact byte length, and SHA-256 digest;
- an immutable model revision or OCI manifest digest—never a mutable tag;
- explicit base-image, architecture, kernel, accelerator, memory, and storage
  compatibility where applicable;
- a license identifier, canonical license URL, and whether explicit acceptance
  is required.

Downloaded bytes remain inert in a private partial store. Downloads resume by
byte offset, are flushed and verified, then atomically promoted to a
content-addressed object. A complete download is not an installation, and an
installation is not a running service.

Tilo does not mirror vendor payloads unless their redistribution terms have
been reviewed for that exact artifact. For click-through or restricted vendor
software, the signed catalogue points at the vendor's official source and the
device downloads it only after the user accepts the displayed terms. Acceptance
records identify the license and catalogue revision; they never contain
credentials.

Downloaded system extensions (`/usr/share/tilo/system-extensions/sysext.d/`)
apply this to content Tilo cannot distribute at all, such as patent-encumbered
media codecs: CI resolves the publishers' packages for each image and signs a
manifest pinning them by URL, length and SHA-256; the device fetches the bytes
from the publishers' own hosts only, and rebuilds the read-only sysext from
verified bytes at every activation. The contract is in
`os/usr/share/tilo/system-extensions/README.md`.

## One-click local setup

“Set up on this device” is one user action with visible checkpoints, not one
opaque mutation:

1. The trusted hardware probe reports memory, storage, accelerator, driver, and
   supported runtime versions.
2. The signed catalogue selects the best fitting runtime/model pair and shows
   why, including download size, installed size, source, revision, and license.
3. The user accepts any required license. Tilo starts a resumable background
   transaction and Settings follows its durable status.
4. Runtime and model artifacts are hash-verified. A boot pack is staged when a
   compatible kernel driver is missing; otherwise the userspace runtime is
   activated immediately.
5. The model manager starts vLLM under a supervisor, bound only to loopback,
   waits for readiness, and publishes the endpoint into the device-local model
   profile. Agents still need their normal workspace capabilities.

Leaving Settings, logging out, losing the network, or rebooting does not lose
the transaction. Cancellation stops at a safe boundary; retry reuses verified
objects. Removal is confirmed, stops the model first, and deletes only objects
whose content identity belongs exclusively to that model.

## Hardware policy

- NVIDIA is selected only when a supported driver is loaded. On Podman, the
  runtime uses NVIDIA's CDI device interface. Missing or incompatible kernel
  support becomes a rebooting boot-pack transaction, never an ad-hoc DKMS run
  from Settings.
- AMD uses the kernel's `amdgpu` driver when compatible and installs ROCm as a
  pinned runtime pack. Tilo selects only hardware/version pairs present in the
  signed support matrix.
- CPU is the compatibility fallback when memory and instruction-set checks
  pass. It is labelled honestly and never presented as GPU acceleration.

The recommendation engine prefers a model that leaves headroom for the desktop
and attached apps. Users may choose another compatible installed model, but an
unsupported combination cannot be forced through the UI.

## Privilege boundary

Settings is a client. It cannot download into system storage, invoke Podman,
load modules, edit boot state, or construct commands. `tilo-model-manager`
accepts only typed verbs and catalogue IDs. The privileged extension activator
independently re-reads the signed catalogue, current image identity, and
hardware facts before each state transition. Polkit authorizes the narrow
management operation; it does not authorize a general command runner.

All lifecycle changes are durable and auditable: requested, downloading,
verifying, staging, restart-required, starting, ready, stopped, failed, removed.
Passwords, provider keys, and bearer tokens are excluded from status and logs.
