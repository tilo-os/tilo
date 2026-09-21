# Reusable finalization operator

`finalization-operator.py` replaces per-run bind/stage, pending-request polling,
exact EFI-response reuse, and handoff scripts. It manages public inputs and signed
outputs. Its checkpoint bridge validates the immutable image once, publishes a
small metadata receipt, and joins that receipt to the same EFI input already
fetched through the signing spool. Local signing tools keep their existing roles.
It never dispatches/restarts a build, grants signing authorization, or installs an OS.

Use a trusted Linux root shell, the authenticated `fgj` (or WSL-visible `fgj.exe`)
CLI, and the existing AWS/EKS broker connection used by `broker_signing_runner.py`.
The broker adapter currently uses local user `benn1x`, AWS profile
`tilo-eks-operator`, and the existing on-prem broker context. It authenticates the
explicit on-prem, Frankfurt and Oregon targets, reads the broker's enabled CI target
inventory, and requires the task or pinned Pod to match exactly one enabled target.
An unknown target, unavailable target, duplicate match, changed target path or
incorrect broker/workload/cluster label stops the operation. Tokens remain in the
broker request stream and are not recorded in observations. Those infrastructure
settings do not select hardware. The operation's `--hardware-profile` must match
the profile authenticated by the retained-rootfs receipt (`null` is the generic
profile); initialization records it as `generic`. The request's architecture and
boot profile describe the remaining intended-image properties.

## Prepare once per immutable set of inputs

Keep public inputs in a root-owned, non-writable ancestor chain, and create a new
operation directory under `/var/lib/tilo-development/operations`. The parent must
already exist. Use the exact helper archive SHA and tools image ID admitted for the
finalizer. The archive's Git revision must match the prepared request.

```sh
operator=/trusted/checkout/os/dev/finalization-operator.py
operation=/var/lib/tilo-development/operations/my-image
python3 "$operator" --operation "$operation" init \
  --request /trusted/prepared/request.json \
  --helper-archive /trusted/prepared/helpers.tar \
  --archive-sha256 "$helper_archive_sha256" \
  --receipt /trusted/admitted/retained-rootfs.json \
  --original-public /trusted/admitted/public-trust \
  --new-public /trusted/prepared/public-trust \
  --tools-image-id "$tools_image_id" \
  --hardware-profile "$hardware_profile" \
  --initial-free-bytes "$initial_free_bytes" \
  --forgejo-host code.podesta.ai --repository qos/qos \
  --dispatch-ref "$controller_branch"
```

Initialization snapshots public files and helper contents once, then publishes
`operation.json` last. Every later command verifies all snapshot hashes and the
local `os/dev/*.py` implementation fingerprint. Keep this checkout for the
operation's lifetime. A changed input or implementation requires a new operation;
existing partial directories are never overwritten. No credentials enter this config.

When a reviewed operator correction must continue an already initialized operation,
pass `--compatible-operation-sha256` with the SHA-256 of that operation's exact
immutable `operation.json` and `--compatible-operator-sha256` with the identity of
the exact reviewed replacement operator. Compatibility remains explicit and bound
to both sides: changed code still fails closed, and a digest for another config or
operator is rejected. The config and its pinned snapshots are never rewritten.

## Bind an actual attempt and stage

Dispatch through the existing finalization workflow, then use its **API run ID**
and actual attempt, not the run number displayed in its URL. The CLI authenticates
the event's exact request bytes, helper revision, ref and unique `finalize` job.
Task ID and worker/container identities are discovered from that authenticated job.

```sh
python3 "$operator" --operation "$operation" wait \
  --run-id "$run_id" --attempt "$attempt" --until running --timeout 300
python3 "$operator" --operation "$operation" bind \
  --run-id "$run_id" --attempt "$attempt"
python3 "$operator" --operation "$operation" stage \
  --run-id "$run_id" --attempt "$attempt"
```

The running job can still be pulling its tools image. `bind` requires the inner
worker to exist and verifies its exact image, request, source, helper bytes,
permissions and capacity before reserving state. If it has not started yet, inspect
the same job and repeat binding while the local run directory does not exist.
Stage promptly within the workflow's operator gate. Once binding or staging has
reserved a directory, inspect failures; do not delete partial state to force retry.
An observation timeout does not mean the build failed and does not authorize restart.
If the worker releases successfully but the response is lost, the local staging
receipt is absent and `status` will not list its pending inputs. This CLI does not
repair that ambiguity: reconcile the exact remote receive/release state before
manual completion. Preserve the reservation throughout that investigation.

## Pending inputs and signing handoff

```sh
python3 "$operator" --operation "$operation" status \
  --run-id "$run_id" --attempt "$attempt"
python3 "$operator" --operation "$operation" wait \
  --run-id "$run_id" --attempt "$attempt" --until request --timeout 300 \
  --ignore-request "$already_serviced_request_id"
python3 "$operator" --operation "$operation" fetch \
  --run-id "$run_id" --attempt "$attempt" --request-id "$request_id"
python3 "$operator" --operation "$operation" handoff \
  --run-id "$run_id" --attempt "$attempt"
```

`handoff` emits existing stable `received`, `responses`, and `checkpoints` paths,
hardware/architecture, and the exact context hash for checkpoint validation and
signing. Generic checkpoint commands derive the same run directory from the admitted
run ID and attempt. A generic operation created by this version intentionally rejects
checkpoint export with `no authenticated checkpoint exporter`: its immutable config
does not yet pin one. A follow-up format must make the exporter a first-class pinned
operation input; copying the legacy validator into the generic operator would weaken
that boundary.

For the currently active legacy operation, select compatibility explicitly and
publish the small checkpoint receipt without streaming the EFI payload through a
second transport:

```sh
python3 "$operator" --operation "$operation" export-checkpoint \
  --legacy-layout --run-directory "$run_directory" --purpose live-uki
# Fetch with the operation-specific legacy spool procedure.
python3 "$operator" --operation "$operation" materialize-checkpoint \
  --legacy-layout --run-directory "$run_directory" --purpose live-uki \
  --request-directory "$run_directory/received/$request_id"
```

Repeat this for `loader` and `target-uki` as each request becomes available.
Materialization requires the fetched signing request,
context, size, and hash to match the independently validated receipt exactly. It
writes the EFI input under `checkpoints/<purpose>` only after that comparison, and
it never grants signing authorization. The read-only `diagnose-checkpoint` command
reports sanitized validation phases when a legacy exporter cannot produce metadata.

Use `authorize-signing-input.py` with the materialized checkpoint bytes as the
independently expected input, then
`service-local-signing-request.py` with the existing explicit local policy.
Write its response under the handoff's `responses/<request-id>` directory.
These stages must not derive expected hashes or authorization from a worker request.

Select the source according to the signing purpose:

- Loader: extract the original architecture-specific systemd loader from the
  authenticated retained image/checkpoint through the pinned operator channel.
- UKI: use the selected unsigned UKI from the immutable resealed checkpoint;
  verify source recipe, public initramfs trust, composefs digest and generation.
- Detached UKI/PCR: use the exact verified local signed UKI response already
  produced and independently recompute policy using the matching systemd version.

Keep nonce/authorization reservations across interruptions. The detailed local
policy and service procedure remains in [REMOTE-SIGNING.md](REMOTE-SIGNING.md).

## Reuse a previously verified EFI response

For an unchanged loader, live UKI, or target UKI, first export its authenticated
checkpoint receipt. Then wait for its request and replace another private-key
operation with one bounded command:

```sh
python3 "$operator" --operation "$operation" service-reused-efi \
  --run-id "$run_id" --attempt "$attempt" \
  --old-run-directory "$old_run_directory" --old-request-id "$old_request_id" \
  --purpose target-uki \
  --timeout 2400 --interval 10 \
  --ignore-request "$already_serviced_request_id"
```

The command requires exactly one unignored pending request. It fetches that request,
materializes it against the current authenticated checkpoint receipt, runs the same
byte, context, certificate, Authenticode signature, and PE-envelope checks as
`reuse-efi`, then delivers the rebound response. Any failure preserves the
completed local step
and stops before the next one. Existing request or response state is never resumed
automatically because a lost delivery response is ambiguous; inspect it and use the
individual commands below to reconcile the exact state.

The equivalent individual commands remain available for recovery and diagnosis:

```sh
python3 "$operator" --operation "$operation" materialize-checkpoint \
  --run-id "$run_id" --attempt "$attempt" --purpose target-uki \
  --request-directory "$run_directory/received/$request_id"
python3 "$operator" --operation "$operation" reuse-efi \
  --run-id "$run_id" --attempt "$attempt" --request-id "$request_id" \
  --old-run-directory "$old_run_directory" --old-request-id "$old_request_id" \
  --purpose target-uki
python3 "$operator" --operation "$operation" deliver \
  --run-id "$run_id" --attempt "$attempt" --request-id "$request_id"
```

Reuse requires materialized checkpoint receipts for the old and current requests,
byte-identical original/current/checkpoint-selected inputs,
matching source revision, architecture, tools image, manifest and target, and exact
old/new request/context bindings. The native verifier checks the retained EFI
signature against the new operation's pinned public certificate and checks the PE
envelope. Its measured hashes must match the output being published. Only the
response nonce and runtime context change. PCR policies remain excluded. Delivery
rechecks the actual worker identity around each transfer. Reuse invokes verification
tools only; it has no private-key input or signer call.

## Focused verification

```sh
python3 -B -m unittest discover -s os/tests -p test_finalization_operator.py -v
```

Run on Linux as root: filesystem tests use disposable owner-only directories under
`/root`. Tests exercise immutable input changes, archive traversal, exact event
admission, transport-byte decoding, timeout/partial-state behavior, and retained
loader context/bytes/publication order. Crypto itself is exercised by the existing
loader-verifier tests; the operator test substitutes that verifier to prove that
rejection prevents publication. No image build or active worker is used.
