# Supervised disposable Kata disk creation

The standalone `create-development-disk.py` command still requires QEMU/KVM
identification through systemd. ARM Kata guests with no DMI/device-tree markers
can instead use the external `create-supervised-kata-disk.py` controller.

Run the controller on the operator machine with an authenticated Kubernetes
context. No Kubernetes credentials enter the guest. Provision a dedicated
single-container sleeping ARM Kata pod first: no host namespaces, token,
annotations, device resources, device volumes, PVCs or sidecars; only explicit
emptyDir and ConfigMap volumes. The container must already have native Podman,
bootc verification tools, the public signing bundle and sufficient scratch
space. Registry authentication, if needed, must be separately provisioned over
an authorized private channel and must never become disk payload.

The host pins JSON maps absolute paths to operator-reviewed SHA256 hashes for
the runtime configuration and drop-ins, containerd configuration, QEMU, shim,
kernel, immutable Kata guest image and firmware code. Review these from the
deployment source and actual host before use. Do not automatically accept
whatever hashes a future guest or host reports. The controller reads the live
host through the node's existing `kata-deploy` agent, compares the pins, binds
the actual container/sandbox/QEMU identities, rejects host block fds (including
hotplugged devices), and inspects actual virtiofs exports as well as OCI intent.
It then rechecks the same live pod/container and invokes the helper through
authenticated Kubernetes exec. Its JSON observation is an audit record, never
a bearer authorization token or claim of independent attestation.

Example (all names, UIDs, pins and paths must come from the current deployment):

```sh
python3 os/dev/create-supervised-kata-disk.py \
  --context OPERATOR_CONTEXT --namespace kata-system --pod DISPOSABLE_POD \
  --pod-uid CURRENT_POD_UID --node NODE --node-uid CURRENT_NODE_UID \
  --host-agent CURRENT_KATA_DEPLOY_POD --pins reviewed-host-pins.json \
  --guest-image REGISTRY/TOOLS@sha256:INDEPENDENTLY_VERIFIED_DIGEST \
  --disk-args verified-disk-inputs.json --evidence live-disk-environment.json \
  --helper-revision FULL_REVIEWED_COMMIT \
  --inspect-only
```

Remove `--inspect-only` only when the real finalized source image is available
and all disk inputs are verified. The execution form also requires
`--creation-receipt PRIVATE/creation.json`; pass `--registry-auth PRIVATE/config.json`
when the source registry is authenticated. The controller snapshots that bounded
file into the authenticated exec only, removes the guest snapshot with its private
temporary directory, records the exact helper commit and bundle digest, and
atomically persists the accepted receipt both beside `disk.raw` and locally.
`verified-disk-inputs.json` contains the normal
creation arguments with Python field names: source, target, architecture,
revision, manifest_sha256, composefs_digest, systemd_version, public_bundle,
output, generation, size_bytes, offline_key_signature and disposable_guest.
It also contains `installer_bootc_directory` (fixed to
`/work/installer-bootc-helper`) and the independently reviewed
`installer_bootc_sha256`.
Paths refer to the guest; source must be an immutable digest reference. A new
exclusive regular file is the only installer target. The controller streams
the local Python helpers instead of trusting guest-supplied helper code.
The required guest image pin must match both the admitted pod image and its
running image digest. The controller binds the actual virtiofs server's shared
root, socket owner and complete export inventory, rejects missing observations,
and checks pod/container identity again after execution before reporting success.

The inner installer container receives a fresh read-only tmpfs at `/run/udev`.
Its entrypoint requires that `/run/udev/data` is absent, the mount belongs to
the inner PID 1 namespace and the filesystem is read-only before bootc starts.
This keeps a later helper from creating an empty database directory that would
disable bootc's direct block-device probing for guest-local loop partitions.

The creator owns partitioning rather than asking `bootc install to-disk` to do
it.  It attaches only the held new regular file, creates a 1 GiB ESP and one
remaining Btrfs root with Tilo type
`7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab`, and materializes only those two loop
partition nodes from their exact sysfs identities.  Both nodes are removed
before the loop device is detached.  The root and ESP are mounted in the
creator's private mount namespace and passed to `bootc install to-filesystem`.
An explicitly empty `--root-mount-spec=` preserves the signed
`root=tilo-auto` command line; adding a filesystem UUID would change the UKI
and bypass Tilo's boot-ESP-bound root discovery.  The independent verifier
recreates and removes the same bounded device nodes before mounting the copied
disk read-only.

The supervised creator also requires a native installer-only bootc artifact at
`/work/installer-bootc-helper/{bootc,receipt.json}` and an independently
reviewed `installer_bootc_sha256` in `verified-disk-inputs.json`. The receipt is
fail-closed: it binds architecture, bootc version, upstream tag and commit, the
tracked patch SHA-256, binary SHA-256 and GNU build ID, Rust/Cargo versions,
target triple and exact build command. The reviewed source pin and patch live in
`os/external-assets/bootc/`; they are provenance inputs, not OS payload.

The creator verifies that contract before allocating a disk. It mounts the
helper read-only at `/run/tilo-installer-bootc` and executes that explicit path;
it never shadows the source image's `/usr/bin/bootc`. The in-container wrapper
rechecks the binary hash, ELF architecture, build ID, version and actual
read-only mountinfo before execution. Its runtime receipt and the original
source `/usr/bin/bootc` identity are included in the creation receipt. The
installed-disk verifier then requires installed `/usr/bin/bootc` to equal the
freshly verified original source binary, to differ from the helper, and scans
installed mutable state and the ESP for any file equal to the helper digest.
This keeps the workaround available across bootc re-exec while proving it was
not installed.

Verify the helper artifact with `installer_bootc_helper.verify_artifact`, and
use the receipt's `binary_sha256` as `installer_bootc_sha256`.
The installer container itself is started by the already verified canonical
digest reference. Bootc resolves that running container's exact local manifest
digest and enters its private mount namespace; the explicit canonical target
reference remains the installed system's upgrade origin.

Nested Podman receives a temporary `fuse-overlayfs` copy-on-write view of the
creator's verified rootful image store. The parent store is mounted read-only
at an alternate path. After the source container is fully running, its wrapper
uses that path only as the lower layer; all lock and metadata writes go to a
root-owned private upper layer. This ordering excludes the parent's in-progress
container layer from the private store snapshot.
Before bootc starts, the wrapper requires the container metadata, Podman image
identity, raw manifest digest and config architecture to agree with the exact
immutable source. The view is unmounted and deleted before the source hold can
be released or the created disk retained; no shared read-write store fallback
is allowed.

Root/native architecture/user-namespace/remote-container/EFI/TPM checks and all
source, signature, boot-prediction and cleanup checks remain enforced. Success
means disk creation only. Installed-byte verification and VM boot remain
separate required acceptance steps. Delete the disposable pod and its owned
resources after collecting the disk and evidence; never reuse its observation
for another pod.

## Complete disposable ARM lane

`run-supervised-install-lane.py` is the durable controller for the steps after a
finalized digest exists. It selects fresh ARM creation, verification and KVM
nodes (or accepts explicit current node names), creates three pinned-image workers without host
paths or service-account tokens, stages the exact Git archive and public trust
bundle, and invokes the supervised creator first in inspection mode and then in
creation mode. Registry credentials are read from a named Docker config Secret
or a private local file and streamed to the disk worker; they never enter a Pod
manifest, command argument, receipt, disk, or retained operation evidence.

After creation, it freezes and hashes the 64 GiB raw file and sparsely transfers
it into a fresh Kata sandbox. That independent worker receives fresh committed
helpers only after the creator has been deleted; it repeats source signatures,
the read-only installed-disk checks and the full source-layer material audit.
Registry auth is introduced only for that bounded verifier command and a guest
shell trap deletes it on every normal, error, signal and timeout path. The raw
file is then sparsely transferred into the KVM worker after both identities are
rechecked. The KVM worker
checks the received length and digest, converts raw to qcow2 exactly once,
requires `qemu-img compare`, verifies the pinned firmware/profile encoder, and
runs the existing prepared-alongside graphical cycle. That cycle performs the
install, removes the medium, cold-boots the encrypted target, completes OOBE,
and reaches the desktop. Workers are retained until the browser follow-up and
evidence collection finish; their active deadlines bound cleanup.

The creator-to-verifier boundary is resumable. Once `creation.json` and the raw
identity are durable, the controller deletes its local registry-auth snapshot
and writes a typed `creator-checkpoint.json` containing the complete selected
creator node document and exact worker identity. A controller, network, or WSL
failure anywhere from verifier provisioning through transfer and creator
release keeps `paused-creator.json` authoritative and leaves that exact creator
emptyDir available for `--resume`. A verifier has its own typed handoff receipt.
If its create response is lost, resume adopts only the exact pinned manifest,
owner labels, Pod UID and node identity. A partially prepared verifier is
deleted and recreated; a ready verifier is reused idempotently. The pause
receipt becomes `creator-released.json` only after creator deletion succeeds.
Creator/install failures and checkpoint-write failures remain terminal and
delete their worker. Creator Pods carry an operation owner and absolute expiry
label and have a 12-hour active deadline. The pause receipt records a
credential-free cleanup command; it validates the checkpoint, Pod UID,
ownership labels, and expiry before deleting anything.

Run it from a clean Linux checkout at the exact helper commit:

```sh
python3 os/dev/run-supervised-install-lane.py \
  --context OPERATOR_CONTEXT \
  --operation-dir PRIVATE/FRESH_ACCEPTANCE \
  --disk-args PRIVATE/media-disk-args.json \
  --public-bundle PRIVATE/public-trust \
  --host-pins PRIVATE/reviewed-host-pins.json \
  --helper-revision "$(git rev-parse HEAD)" \
  --acceptance-id FRESH_DNS_SAFE_ID \
  --registry-secret forgejo-pull-secret
```

To resume, repeat the same pinned arguments and add `--resume`. If the retained
media is no longer wanted, run the `cleanup_command` array recorded in
`paused-creator.json` after its expiry. `--cleanup-paused` deliberately rejects
early cleanup and Pods whose live UID or ownership labels differ.

The operation directory retains mode-0600 worker bindings, creation, source-layer and
verification receipts, raw identity, conversion/cycle evidence and the final
acceptance receipt. The temporary local registry-auth snapshot is removed on
success and interpreter exit. A successful run deliberately leaves the KVM
worker alive so the exact installed target can be reused for browser acceptance;
do not launch another install or manufacture a replacement target disk.
