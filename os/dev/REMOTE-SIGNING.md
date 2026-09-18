# File-mediated development signing

This adapter keeps all private keys local. It starts no network listener and
copies no credentials. The remote worker emits public signing inputs; the
operator transfers fixed files through its existing authenticated connection.

## Deployment and trust boundary

Install `remote-development-signer.py` as a root-owned executable outside the
checkout (for example `/var/lib/tilo-signing-bridge/signer`). Install
`signing_spool.py` and `development-signer.py` alongside it, all root-owned and
not group/world-writable. The latter is used for file-validation helpers only;
there are no keys in this remote directory. All parent directories must meet
that ownership rule. `bridge.json` is adjacent to `signer` and contains:

```json
{
  "schema": 1,
  "context": {
    "schema": 1,
    "job_id": "independently-pinned-job",
    "pod_uid": "independently-observed-pod-uid",
    "container_id": "independently-observed-container-id",
    "image_id": "sha256:IMMUTABLE_IMAGE_CONFIG_DIGEST",
    "revision": "FULL_SOURCE_COMMIT",
    "architecture": "aarch64",
    "manifest_sha256": "PUBLIC_TRUST_MANIFEST_SHA256",
    "target": "intended-registry/repository:target"
  },
  "spool": "/var/lib/tilo-signing-bridge/requests",
  "public_certificate": "/var/lib/tilo-signing-bridge/efi-signing.crt",
  "certificate_sha256": "SHA256_OF_EXACT_CERTIFICATE_PEM_BYTES",
  "timeout_seconds": 1800
}
```

Spool directories are owner-only. Existing finalization scripts call the
adapter through their existing signer argument. It emits `<nonce>/input` then
atomically publishes `<nonce>/request.json` and waits. `export-efi-cert` serves
only the configured public certificate after hashing it.

The operator MUST recheck exact Pod UID/container identity before each transfer,
admit actual input+output+measurement-scratch storage, and transfer only those
fixed filenames using argument-based authenticated exec. Do not execute any
remote path or command carried by metadata. No shell command is in this schema.

## Independent local authorization

Prepare a local root-owned policy with the same independently pinned context,
`schema: 1`, `authorized_inputs` list, and `efi_certificate` / `pcr_public_key`
objects `{ "path": "/local/public/file", "sha256": "EXACT_PEM_FILE_SHA256" }`.
The two public hashes must come from the validated persistent public bundle,
not the pending remote request. Policy is outside the untrusted transfer area.

Create each authorization using `authorize-signing-input.py` with an exact
locally authenticated input snapshot and independently expected SHA256:

```
authorize-signing-input.py --identifier live-uki-1 --purpose uki \
  --source /local/verified/unsigned.efi --expected-sha256 VERIFIED_SHA256 \
  --architecture aarch64 --generation GENERATION --composefs-digest SEAL \
  --prepared-input /local/owner-only/approved-uki.efi
```

The returned public JSON row is inserted in the local policy. This checks exact
input hash, PE architecture, generation and composefs seal. `loader` authorizes
the authenticated original loader. `detached-uki` requires the exact already
verified signed selected target. `pcr-policy` also takes `--systemd-version`,
requires an already signed target snapshot, independently recomputes PCR11 with
the trusted matching systemd-measure, and outputs exactly policy32+generation8.
Use the trusted Fedora44 tools locally where systemd259 matches the UKI stub.
Never manufacture an authorization by copying a request's claimed input hash.
Authenticated source/recipe and public initramfs validation are prerequisites;
this utility does not independently reproduce the OS build.

Different legitimate calls for identical bytes require distinct explicit
single-use authorization IDs. A request is never automatically reauthorized
following timeout, transport failure or process restart.

## Local service and return

Receive metadata+input in a fresh owner-only directory; invoke:

```
service-local-signing-request.py \
  --request-directory /local/received/REQUEST \
  --policy-path /local/trusted/policy.json \
  --state-directory /local/trusted/used \
  --response-directory /local/outgoing/REQUEST \
  --signer-path /var/lib/tilo-development/signer/signer
```

The local service checks exact context, operation, length and bytes against the
local authorization, then durably reserves both nonce and authorization before
key use. It snapshots into private local state, invokes the existing signer,
and verifies the result with the independently pinned public identity. EFI
outputs must preserve the original Authenticode envelope; RSA signatures must
verify. Only `output` and `response.json` are returned. Transfer output first,
then atomically publish response.json remotely; transport must not overwrite
existing files. The adapter binds response nonce/context/operation/input hash
and output length/hash before returning to the existing remote script, which
also verifies cryptographic signatures. All paths in metadata are fixed by the
operator; response data never chooses a local destination.

Failed or interrupted reservations remain used. Inspect public status and use
an explicitly new authorization if appropriate, instead of replaying key use.
Never delete a pending request merely because observation timed out.

The existing local signer now allocates its owner-only operation scratch under
trusted `/var/lib`, allowing its key home to remain mounted read-only. No key
material is copied into scratch. Do not mount the key home in any remote worker
or nested image payload container.

After public-only final image publication, use existing local Cosign to sign
the immutable registry digest directly; no rootfs pull or private-key transfer
is needed. Fresh source, installed-disk and boot acceptance are separate gates.

Tests use newly generated fixture keys and exercise actual EFI/RSA signing,
replay rejection, context/input mismatch, symlink/oversize refusal, substituted
responses, and an actual read-only signer-home mount in a private namespace.
They do not claim that the real OS has been finalized or booted.
## Native signer test prerequisite

The signer and its independent response verification invoke only the canonical
`/usr/local/bin/tilo-crypto` Rust helper. The fixture suite tests that actual
checked-in backend and never substitutes legacy external signing tools. It
fails clearly before creating fixture keys if the helper, protected
`/var/lib`, or mount namespace capability is missing.

Use the prepared native tools container with the authenticated native crypto
binary copied root-owned at its canonical path. Fedora's base `/var/lib` mode
may be0775; the tools recipe must set0755 for the existing trusted-parent
contract. This is a container metadata change, not a global host change.
Run with network disabled, SYS_ADMIN only for the isolated read-only-home
mount test, source mounted read-only, and no persistent key mount. The
container writes only newly generated fixture keys in its private writable
layer and is removed afterward. Require all nine tests to pass; a prerequisite
error is not a skipped or successful test.

## Concrete authenticated transport

Install `signing-spool-endpoint.py` alongside the remote `signer`, root-owned
and non-writable. The endpoint loads that directory's public `bridge.json` and
can only list/read request files or publish the matching public response. It
does not sign or receive credentials. Use `transfer-signing-spool.py` locally
with the **existing authenticated kubectl context**; existing exec-auth and
endpoint-proxy setup remain unchanged. No kubeconfig copying or new service is
performed by these helpers.

Prepare an independently observed root-owned binding JSON with fields:
`schema` (1), `namespace`, `pod`, `container`, `pod_uid`, `container_id` (exact
Kubernetes status value including scheme), `image` (exact Pod spec reference),
`image_id` (exact Kubernetes status value), `restart_count`, `pod_spec_sha256`,
`context_sha256`, and `endpoint` (absolute installed endpoint path).
`pod_spec_sha256` hashes canonical JSON of the entire observed Pod spec;
`context_sha256` uses `signing_spool.context()` over the same operator-pinned
context as the bridge/local policy. Do not refresh these pins automatically
when a mismatch appears. Both the Pod and selected running container are
checked before and after **every** transfer; deletion/replacement, restart,
image or spec changes reject the result.

```
transfer-signing-spool.py list --context EXISTING_CONTEXT --binding /local/trusted/binding.json
transfer-signing-spool.py fetch --context EXISTING_CONTEXT --binding /local/trusted/binding.json \
  --request-id NONCE --directory /local/received/NEW_DIRECTORY
# Independently authorize input, then invoke service-local-signing-request.py.
transfer-signing-spool.py deliver --context EXISTING_CONTEXT --binding /local/trusted/binding.json \
  --directory /local/outgoing/REQUEST
```

Fetch requires a new owner-only local directory beneath a trusted owner-only
parent. It makes `request.json` available to the local signer only after exact
input length/hash and post-transfer container identity pass. Delivery compares
against the actual remote request, uploads exact public output first, checks
identity again, then atomically publishes matching response metadata with
no-overwrite semantics. A partial delivery does not authorize another signing
operation. Inspect the existing request/output state to resolve interrupted
transfers; never delete replay reservations or blindly retry key use.

Only fixed filenames and argument-vector commands are used; no tar extraction,
request-controlled shell command or archive path is involved. Streams have
explicit byte bounds and deadlines. Kubernetes exec cannot atomically pin a
Pod UID, so before/after checks detect replacement; the bytes being transferred
are public signing inputs/results, and any mismatch prevents accepting the
transfer receipt. Physical installation and signing authorization remain
separate steps. Tests use a fake API runner plus the actual endpoint filesystem
protocol, and real subprocess byte-limit/deadline tests; no cluster is contacted.

### Nested tools container on an existing CI runner

When the public tools run inside the runner's Docker daemon, pin the Kubernetes
DinD container using the same fields above and add a `docker` object containing
`id` (full 64-character container ID), `image_id` (full `sha256:` config ID),
`started_at` (exact Docker State.StartedAt), and `restart_count`. Obtain these
from the created running worker; never use a mutable container name or short ID.
The endpoint path then refers to the installed helper inside that exact tools
container. Each transfer checks both runtime identities before and afterward,
and uses `docker exec` with the pinned full ID. Docker inspection requests only
identity and state fields, excluding environment and registry authentication.

This supports image finalization on the existing overlay-storage CI runners.
It does not authorize installation-disk creation there or prove a live transfer;
the nested-container tests exercise the fixed command path and replacement,
restart and paused-state rejection using the fixture API runner.
