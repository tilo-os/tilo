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
Paths refer to the guest; source must be an immutable digest reference. A new
exclusive regular file is the only installer target. The controller streams
the local Python helpers instead of trusting guest-supplied helper code.
The required guest image pin must match both the admitted pod image and its
running image digest. The controller binds the actual virtiofs server's shared
root, socket owner and complete export inventory, rejects missing observations,
and checks pod/container identity again after execution before reporting success.

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

The operation directory retains mode-0600 worker bindings, creation, source-layer and
verification receipts, raw identity, conversion/cycle evidence and the final
acceptance receipt. The temporary local registry-auth snapshot is removed on
success and interpreter exit. A successful run deliberately leaves the KVM
worker alive so the exact installed target can be reused for browser acceptance;
do not launch another install or manufacture a replacement target disk.
