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
  --inspect-only
```

Remove `--inspect-only` only when the real finalized source image is available
and all disk inputs are verified. `verified-disk-inputs.json` contains the normal
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
