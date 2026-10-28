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

## Application runtime bundles

An application's `optional-runtimes` declaration describes a read-only mount
used when that exact signed runtime is installed. It is not permission to
fetch every listed capability during application installation or update, nor
is it a statement that the runtime is absent from the OS image. The standard
image includes Browser and its signed Chromium engine so web rendering does
not require a setup download. Browser-free images are explicit development
profiles, not the release default.

The Store receives an exact consuming-app/runtime request through
`tilo-store:runtime/<app-id>/<runtime-name>/<version>`. Its existing authenticated
app-host capability checks the installed application's signed manifest,
current package root and exact optional declaration before resolving an offer.
The review shows the configured source and signed download size; package and
publisher identities remain available under Technical details. Inspection
does not acquire component bytes, and a verified installed bundle can be
reused offline.

Explicit acquisition rechecks the consuming root and immutable offer identity
before passing the admitted offer to the existing signed, bounded installer.
The confined Store never executes a privileged installer directly or supplies
its own trust keys or download URLs. Required application runtimes still
install before the app can be replaced; optional ones do not download silently.
Connection-owned notifications report real download bytes and a separate
verification/install phase, without inventing an installation percentage.
Failure offers a fresh review and retry, not an automatic second download.

Acquisition does not change a running application's sandbox: close and reopen
that application through the normal launcher to obtain the runtime mount.
Back leaves active work running in the background; it is not cancellation or
rollback. Durable download resume is not promised. Local model and
system-extension transactions below retain their own catalogue and lifecycle
contracts.

## Trust and catalogue rules

The catalogue may describe artifacts, compatibility, display metadata, and
fixed typed launch fields. It may not contain scripts or arbitrary command
lines. Every accepted extension has:

- a signature from a key already trusted by the installed Tilo image (Ed25519
  release keys in `trust.d/` for BootPacks and runtimes; the dedicated
  extension-manifest key `keys/extensions.pub` for downloaded system
  extensions, see below);
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

## A person's own models

The signed catalogue's recommendation is a starting point, not the only
source of models. In Agent Models a person can search Hugging Face from the
window bar's search (the pane draws no field: what is typed there queries the
Hub, for GGUF or safetensors as the pane's format choice says), open any
model by its id or link (`owner/name`, a model, tree, blob or resolve URL),
including their own fine-tunes and private repositories, or add a GGUF file
from the device. The model manager answers all of it; Settings only shows
the answers and asks through typed effects (`search-hub-models`,
`set-hub-format`, `show-hub-model`, `install-hub-model`, `add-model-file`), which agents reach
through the same action system. A file from the device is chosen in the file
chooser, which hands Settings the open file; Settings passes that handle on and
the model service copies from it. Neither ever opens a path it was given.

What a person sees before anything downloads, for every runnable variant of
the model at the commit it resolved to:

- the variant: one GGUF quantization (a single file, or its numbered shards)
  for llama.cpp, or the safetensors snapshot (weights, config, tokenizer) for
  vLLM;
- its download size, and a fit rating from the trusted hardware probe:
  whether it fits accelerator memory, shares accelerator and system memory,
  runs from system memory on the processor, or does not fit (a variant that
  does not fit cannot be downloaded);
- its licence, read from the model card at that commit, with the licence
  document to accept;
- whether a signed runtime on this device serves its format yet. Without
  one the model can still be downloaded; it starts once one ships.

Gated models need their terms accepted on Hugging Face with the person's
account, then download with that account's credential: a "Sign in with
Hugging Face" (authorization code with PKCE through the system browser and a
one-shot loopback redirect, or a device code entered on Hugging Face's page
for a device without a browser, also `tilo-model-manager hub-sign-in`), or a
pasted access token. Either is kept in the same vault entry
(`ServiceCredentials`, bound to the model manager's purpose and the
`https://huggingface.co` origin): Settings stores it, the model manager reads
it, and it is never put on a command line, in state or in logs. The sign-in
asks only for `openid profile gated-repos`; Settings shows the account and
Sign out. The model manager renews a due sign-in with its refresh token and
reports one it cannot renew as ended, which Settings shows as "sign in
again".

### Why weights may come from anywhere while runtimes may not

A runtime is code: it parses weights, owns accelerator memory and opens a
port. It stays a digest-pinned OCI image from the signed catalogue, started
with an argument vector built only from typed, signed fields. Weights are data
that this signed code reads, so a person can take them from wherever they
choose without widening what runs on the machine:

- only data formats are downloaded: GGUF files, and safetensors with their
  JSON config and tokenizer files. Pickled checkpoints (`*.bin`, `*.pt`),
  Python files and everything else in a repository are never fetched, and vLLM
  is never told to trust remote code, so a repository cannot ship code that
  runs;
- every file is verified against the hash the Hub publishes for the pinned
  commit before it enters the content-addressed store: the LFS SHA-256 of a
  large file, the git blob id of a small one. What runs is exactly what the
  person reviewed, even if the repository changes later;
- the runtime sees the model read-only through one bind mount, rootless, and
  answers only on a loopback port the manager allocates. Agents reach it
  through the gateway like any other provider.

The person takes on the model's behaviour and licence, which is why both are
shown before the download, and why the acceptance is recorded against the
exact commit and file list.

### Storage and lifecycle

A pick downloads in a durable background worker with the same resumable,
byte-offset transfers and published progress as a catalogue release; a
dropped connection resumes, and bytes that fail verification are discarded so
the next try starts clean. Each verified file becomes the object
`$XDG_DATA_HOME/tilo/models/sha256/<sha256>` on this device; the model is a
directory under `snapshots/` assembled from hard links to those objects,
named as in the repository so shards find each other, and put in place
atomically. A GGUF file added from the device is copied and hashed into the
same store, so changing or deleting the original changes nothing. `state.json`
records each pick (repository, commit, files and their published hashes,
licence) beside its lifecycle; another revision of an added model is added
after removing the first. Removal stops the model, deletes its directory and
every object no other model uses, and its unfinished partial files.

Once installed, a pick is started like a catalogue model when a signed
runtime for its format fits the hardware, and while it runs it is a model of
the local provider profile that agents and Chat choose from.

## In-process models

Some signed models are small enough to run on the CPU inside the per-user
service that uses them, such as the U²-Net subject selection model the model
gateway loads with `rten` for "Select subject". These are catalogued with
`"artifact_format": "onnx_file"`, an in-process capability (`segmentation`),
no `allowed_runtimes` and no context window, and can never claim chat, vision
or face capabilities. Settings shows each one as its own card with the same
download, storage, source, revision and licence confirmation, and
`settings://models/<model-id>` opens Agent Models at that card.

Setup is the same verified, resumable download and licence record, then
nothing else: no runtime image, container, service or hardware gate. The
installed model is the content-addressed object
`$XDG_DATA_HOME/tilo/models/sha256/<sha256>`, and `state.json` beside it
records its revision, licence and capabilities. A consumer finds it with
`local_model_service::installed_in_process_model` over the store
`user_model_root()` names (none without a data home, never a shared
directory), a filesystem-only lookup that needs neither the catalogue nor
the model manager. Removal deletes the object; the
lookup then reports no model.

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
accepts only typed verbs with catalogue IDs, Hugging Face model references and
variant names it resolves and verifies itself, or a GGUF file path it copies
into its store. The privileged extension activator
independently re-reads the signed catalogue, current image identity, and
hardware facts before each state transition. Polkit authorizes the narrow
management operation; it does not authorize a general command runner.

All lifecycle changes are durable and auditable: requested, downloading,
verifying, staging, restart-required, starting, ready, stopped, failed, removed.
Passwords, provider keys, and bearer tokens are excluded from status and logs.
