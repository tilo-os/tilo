# Finalization operator handshake

Prepare the real retained receipt, exact request JSON and public bundles before
dispatch. No placeholder receipt or request is acceptable. The source artifact
must already exist; this helper does not dispatch CI or authorize signatures.
The helper revision in the workflow must include `stage-finalization-worker.py`.

## Local inputs, before the five-minute gate

The existing operator signing installation is `/var/lib/tilo-development`.
Its public bundle is `public`; the local signer executable is `signer/signer`.
Use only the five public files, never the signer directory, as remote inputs.
The persistent bundle manifest SHA256 observed on 2026-09-18 is
`63dc37bc8bf026be8806e480a2f8cb67b0f7e7c4b1804d528b98845c3602f4f7`.
Revalidate it against the request; do not silently substitute it for a different
original build bundle. The raw artifact may have a different original identity.

Keep all local input files under trusted root-owned directories. Prepare a
private local working directory for binding, signing context, used-request
reservations and incoming/outgoing requests. Retain it across worker failure.
Do not create a policy authorizing hashes copied from incoming signing requests.

After the worker emits its identity, independently inspect the enclosing Pod and
DinD container plus the full inner Docker ID/config ID/StartedAt/restart count.
Construct the existing transport binding described in `REMOTE-SIGNING.md`.
The signing context uses the exact Pod UID, **inner Docker ID** as container_id,
inner tools config ID as image_id, source revision, new public manifest digest,
new target and the actual job identifier. `context_sha256` is
`signing_spool.context(context)`. Replacements require a new explicit operation;
never refresh pins automatically.

## Stage and release

With a directly working existing kubectl context, the executable entry point is:

```sh
python3 os/dev/stage-finalization-worker.py stage \
  --context "$EXISTING_CONTEXT" --binding "$PINNED_BINDING" \
  --request "$EXACT_REQUEST_JSON" --signing-context "$PINNED_SIGNING_CONTEXT" \
  --receipt "$AUTHENTICATED_RETAINED_RECEIPT" \
  --original-public "$ORIGINAL_PUBLIC_BUNDLE" \
  --new-public /var/lib/tilo-development/public
```

The current EKS operator route uses the existing authenticated broker Kubernetes
API runner. For that route, replace the context argument above with
`--context broker-authenticated-api --broker`. The same two options apply to
`transfer-signing-spool.py list|fetch|deliver`. Run locally as root: the adapter
uses the existing `xemmerling` AWS profile `tilo-eks-operator`, then the existing
on-prem kubectl context `admin@tupo-production` to `deployment/sandbox-broker`. It loads the broker's
existing EKS endpoint/CA config and supplies the fresh authorized operator token
through stdin. No kubeconfig is copied or new service started.

`broker_signing_runner.py` implements the existing `Transport.runner` contract;
`stage(client, packet, request_raw)` also remains available for orchestration.
The adapter admits only exact bound get/exec commands and fixed endpoint
operations. Public payloads stream with byte/deadline bounds; tokens remain in
memory/pipes and diagnostics are suppressed. The API EOF wrapper runs inside the
tools container, so the enclosing DinD container needs no Python installation.
`remote_call` retains all before/after identity checks. The earlier live fixture
proved the existing API route; this production adapter and nested Docker route
still require their own public-only live acceptance before actual signing.

The packet contains only the retained receipt and exact five-file public bundles
as bounded base64 values, the pinned context, and exact request SHA256. The helper
validates the retained receipt schema and both public identities. It checks the
worker's request and helper/source revision markers, then reserves fresh private
directories and installs bridge code **from the already pinned helper checkout**.
No caller-supplied code, destination paths or private keys are transferred.

The first authenticated operation returns the exact staged packet/context/request
receipt. Only after its post-transfer identity check succeeds does the second
operation atomically publish `/work/operator-ready` without overwrite. Partial
staging fails closed; do not retry into its directories or release manually.
This gate permits artifact authentication and finalization to start. It is not a
signing authorization, image acceptance or installation approval.

## Service each signing request

Use `transfer-signing-spool.py` through the same bound Transport to fetch into a
fresh private local directory. Establish the source independently of request
metadata:

* Loader: authenticate the retained image/checkpoint and extract its original
  architecture-specific systemd loader through the pinned operator channel.
* UKI: obtain the selected unsigned UKI from the immutable resealed checkpoint;
  verify source recipe, public initramfs trust, composefs digest and generation.
* Detached UKI/PCR: use the exact verified **local signed UKI response** already
  produced, and independently recompute PCR policy using matching systemd259.

Run `authorize-signing-input.py` with those independently verified input hashes,
not a hash copied from the pending request. Its public authorization row belongs
in the local root-owned policy with the validated public certificate/PCR-key
paths. Then run `service-local-signing-request.py` as documented in
`REMOTE-SIGNING.md`, and deliver its verified public response through the same
bound Transport. Keep durable nonce/authorization reservations; interrupted key
use is not automatically retried. No OS input is signed before authentication.

The staging helper deliberately does not infer these artifact authorizations.
Checkpoint extraction/authentication and real nested-worker transport remain
separate acceptance steps. Tests cover the filesystem/identity/gate protocol;
they do not prove a real finalization, signing operation or boot.

## Reviewed assembly source selection

The request selects one explicitly admitted source/epoch pair:
4583a44c4056c67cfd1ebc4f1e02c7fa4aff3ffc /1789690573 or
8fff1d6001b64a80bc730e87d3ffc529d2d9e70a /1789733507, or its reviewed
DNS rootfs correction36c58be61ef1b25f24ae130c5ab4dde5d2350d45 /1789748444,
or reviewed runtime-package policy successor
b4f01a7d5437d015950c9b79be37e673b09b829c /1789749860.
The workflow checks out the source emitted by request validation; the launcher
checks that Git identity and emits matching source markers. Staging validates
the retained receipt against this exact request, and the finalizer authenticates
the retained image before signing. A different commit or mixed epoch is rejected.
The native payload receipt retains its original4583 source/epoch independently.

Use a helper archive containing this change and its exact reviewed SHA in local
request preparation. The old2d135 launcher archive rejects8fff and must not be
patched in memory. Tools image/crypto CLI/signing policy remain unchanged.
