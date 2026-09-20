# Retained ARM image finalization worker

`development-finalize.yml` is manual and must not be dispatched until the raw
artifact exists and the operator is ready to service signing requests. It uses
the existing `runner-disk-100g-arm64` disposable CI guest and native Podman overlay.
It does not run the compile driver, generate signing keys, or create installation
disks. The CI guest has its existing block-backed scratch disk; it is not an
eligible supervised installation-disk worker.

The workflow pins the public tools image/config and original source4583. Supply
the reviewed full helper commit and exact SHA256 of the request JSON. The helper
and source checkouts are checked for revision and tracked modifications, archived
without `.git`, and stamped for the finalizer. Controller and helpers copy only
`os`; source is copied once. No image or dependency build occurs.

## Operator handshake

1. Prepare the validated retained receipt and original/new public trust bundles
   before dispatch. Keep signing keys and used-request state on the operator.
2. Observe `worker-identity.json` in the live job output: full inner Docker ID,
   image config ID, StartedAt and request SHA256. Independently bind the enclosing
   Kubernetes pod UID, runtime, DinD container identity and these inner fields.
   A container name alone is insufficient. Use the nested Docker signing-spool
   transport with before/after identity checks.
3. Within five minutes, copy public files to
   `/work/inputs/retained-rootfs.json`, `/work/inputs/original-public` and
   `/work/inputs/new-public`. Install the validated external signer bridge under
   `/var/lib/tilo-signing-bridge`, with its actual runtime context and private
   directory modes. This directory contains public configuration and requests,
   never private signing keys. The launcher supplies scoped registry auth
   privately from the existing CI login.
4. After pinning and validating the worker and bridge, atomically publish the
   root-owned regular `/work/operator-ready` containing the request SHA256.
   The launcher checks native overlay and trusted writable directories before
   waiting, then invokes `finalize-development-rootfs.py --helpers /work/helpers
   --request /work/finalization-request.json --evidence /evidence` exactly once.
5. Service external signing through the bound transport. The driver must publish
   its verified image before success; a local image ID is insufficient. The
   public `08-published.json` identifies the immutable result. Apply local Cosign
   signatures and fresh verification to that immutable result separately.

`/var/lib` is a writable root-owned0755 operation-scratch volume, distinct from
the nested image-store volume. `/evidence` is root-owned0700, matching the driver.
No host directory or Docker socket enters the tools container. Its privilege is
confined to the existing disposable CI guest, as in the compile workflow.

## Completion and failure

The finalizer gets a bounded lifetime, not automatic retries. Only fixed public
JSON receipts (including bounded pending signing metadata) are exported. Tar
transport itself is byte/time limited; symlinks, multiple members and oversized
files are rejected before export. No signing inputs, full workspace, raw logs,
registry credentials or keys are uploaded.

Cleanup always attempts the exact created Docker ID and only volumes created by
this invocation; `cleanup.json` lists failures. Existing resource names cause
rejection and are never adopted or deleted. CI teardown can also destroy the
guest: no image-store persistence is promised. Stages 2 and 5 therefore publish
their images under request-owned staging tags and record the registry
manifest/config digests in `02-loader.json` and `05-reseal.json`. After an
interruption, inspect public receipts and resolve the local signer's pending/used
state. An explicit new invocation may pass the prior public receipt directory
with `--restore-checkpoint` and `--restore-stage`; the launcher admits exactly
receipts 01-02 or 01-05 and the driver
revalidates their chain, stable request/source/helper identities, the fresh
worker context, and the immutable registry bytes before pulling the digest.
Producer and consumer pod/job IDs may differ; each context must independently
match the stable request fields. The option is never inferred, and a started
later signing stage still blocks rather than retrying implicitly.

The workflow exposes restore only as three all-or-none dispatch inputs: the
prior internal run ID, its attempt, and an independently recorded canonical
SHA256 of its selected stage-2 or stage-5 receipt, plus the selected stage. A
task-scoped repository token reads that terminal failed
or cancelled run and its uniquely named `finalization-RUN-ATTEMPT` artifact.
Retrieval refuses the current/active run, redirects, expired or ambiguous
artifacts, oversized responses, unexpected ZIP members, request/receipt-chain
mismatches and receipts without immutable registry identities. It materializes
only public receipts 01-02 or 01-05 into a new owner-only directory.
When the three inputs are empty the workflow neither calls the retrieval helper
nor passes a restore checkpoint.

Contract tests: `python3 os/tests/test_finalization_worker.py`. These test input,
argv, directory-mode, bounded-receipt and cleanup behavior; they do not establish
that a full remote finalization or OS boot has succeeded.
