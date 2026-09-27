# System extensions and local models

Tilo treats optional machine software as signed desired state, not as an
unrestricted package-manager escape hatch. The first consumer is local agent
inference, but the mechanism also covers GPU userspace stacks and
kernel-coupled drivers.

## Extension classes

| Class | Examples | Activation | Rollback |
| --- | --- | --- | --- |
| Runtime pack | An inference engine (vLLM, llama.cpp), CUDA/ROCm userspace | A supervised service without network egress after artifact verification | Stop service and select the previous content-addressed runtime |
| System extension | Additive `/usr` or `/opt` payload with no early-boot dependency | Read-only systemd-sysext image matched to the Tilo release | Unmerge/mask the image |
| Boot pack | NVIDIA kernel modules, firmware, initramfs inputs | A new signed bootc deployment; reboot required | Normal Tilo deployment rollback |

Inference engines are runtime packs only: the base image ships neither vLLM
nor llama.cpp. The catalogue stays deliberately small, one pack per engine:
vLLM for CUDA cards, llama.cpp with Vulkan (and its CPU backend) for every
other GPU, integrated AMD and Intel graphics included.

Kernel modules are never hot-installed into the immutable root. A boot pack is
pinned to the exact base-image digest, architecture, and kernel ABI. It is
staged and measured like an OS update, so Secure Boot, boot-health confirmation,
and rollback remain meaningful. systemd-sysext is useful for additive userspace
files, but it is not the kernel-driver transaction mechanism.

## Trust and catalogue rules

The catalogue may describe artifacts, compatibility, display metadata, and
fixed typed launch fields. It may not contain scripts or arbitrary command
lines. Every accepted extension has:

- an Ed25519 signature from a key already trusted by the installed Tilo image;
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

## One-click local setup

“Set up on this device” is one user action with visible checkpoints, not one
opaque mutation:

1. The trusted hardware probe reports memory, storage, accelerator, driver, and
   supported runtime versions.
2. There is no default engine or model. Every (engine, device, model)
   configuration the signed catalogue and the hardware support is ranked by
   expected speed, then by how much room it leaves. Settings asks “Set this
   up?” for the best one, naming model, engine, device, download size,
   license, memory and speed, and lists the others behind “Show more”, each
   with its own fit and speed, e.g. vLLM on an RTX 4070 Mobile recommended,
   llama.cpp on the same card, on the Radeon 780M beside it through Vulkan,
   and on the processor as alternatives. Only after a yes are the runtime
   pack and then the model downloaded.
3. The user accepts any required license. Tilo starts a resumable background
   transaction and Settings follows its durable status.
4. Runtime and model artifacts are hash-verified. A boot pack is staged when a
   compatible kernel driver is missing; otherwise the userspace runtime is
   activated immediately.
5. The model manager starts the chosen engine under a supervisor in a
   container with no network and only the configuration's own device (one
   render node for Vulkan, NVIDIA's CDI device for a card), serving only a
   Unix socket in a private directory, waits until its
   API answers, and publishes that socket into the device-local model profile
   as supervised custody. Only such a profile counts as unable to send what it
   is given off the device. Agents still need their normal workspace
   capabilities.

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
- Any GPU with a Vulkan driver runs llama.cpp: AMD and Intel through the Mesa
  drivers the runtime pack carries, driven through the in-kernel driver's
  render node, so they need no boot pack. The probe reads each GPU's
  dedicated memory and the system memory it can map. An integrated GPU's
  model memory is RAM: it gets its carve-out plus no more of that than the
  desktop can spare (available memory less 4 GiB).
- CPU is the compatibility fallback when memory and instruction-set checks
  pass. It is labelled honestly and never presented as GPU acceleration.
- An accelerator has three states, not two: usable, absent, and present but
  usable only after a restart. The last covers NVML reporting a
  driver/library version mismatch and a driver boot pack staged for the next
  boot. That state holds the runtime decision: nothing falls back to the CPU,
  and presets do not move tasks to a hosted provider as if there were no
  card. Settings says “Restart to finish setting up your graphics driver”.

The ranking prefers the faster configuration and, between equals, the one
that leaves more headroom for the desktop and attached apps. A CPU model is offered only when the memory the kernel
reports available, less 4 GiB for the desktop, holds it, the machine has at
least four threads, and its expected speed is better than impractical. The
expected speed (memory bandwidth over the bytes of weights read per token) is
shown beside the download, so a slow model says it is slow. Users may choose
another compatible installed model, but an unsupported combination cannot be
forced through the UI.

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
