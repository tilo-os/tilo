# Certified headless cloud workspace

`qos-cloud-host` runs native `StatePeer`, Writer and Chat documents and their real
application action implementations. It does not construct a compositor, window,
renderer, framebuffer or video encoder. The app libraries still have UI build/link
dependencies; a model-only crate extraction has not been performed.

Configuration comes from trusted provisioning:

- `QOS_CLOUD_MACHINE_ID`: immutable machine identity (required).
- `QOS_CLOUD_STATE`: private persistent directory, default `/var/lib/tilo-cloud`.
- `QOS_CLOUD_AI_CONFIG`: private rotating credential file, default `/var/lib/tilo-cloud/ai.json`.
- `QOS_CLOUD_PACKET_RELAY`: public HTTPS Iroh packet relay URL (required for apply/serve).

Run as the dedicated `tilo-cloud` account. The SSM wrapper must deliver a bounded
public request file readable by that account. Commands:

```
qos-cloud-host enrollment inspect
qos-cloud-host enrollment apply --request-file /run/tilo-cloud/request.json
qos-cloud-host serve
```

Untrusted Automerge bytes are applied in a re-executed worker process. The deployed
binary is this crate's own `qos-cloud-host` (`src/main.rs`), which serves those workers
by calling `tilo_automerge_isolate::dispatch_early()` before starting any threads; no
other binary links this library in production.

Inspect creates only a local host device key and a five-minute public challenge.
The browser owns its root/device keys and signs the native certificates and both
root authorization and device-possession proofs. `qos-user-identity::cloud_enrollment`
defines the shared signed schema. A bound host certificate is returned in subsequent
challenges and must be reused verbatim. Account login cannot replace a bound root.

Every state transaction holds an OS file lock and atomically fsyncs a private journal
before exposing success. The journal contains root public key, certified roster,
revocations, native directory and peer state, idempotence receipts, and pending
replication. No user root secret is stored. Corrupt journals fail closed.

The host device secret is never in the journal. It is sealed in `host-secret/`
(a `tilo-seal` envelope bound to the machine identity, under an owner-only local
key) and opened on every Store load; a tampered or foreign blob, or a Store whose
sealed secret or key is missing, is refused rather than given a new identity. The
local key is readable by the state directory's owner and root, so this separates
the secret from the state and authenticates it, but is not hardware binding.
Moving a state directory means moving `host-secret/` with it. State written by
builds that kept the secret inline is refused; reset such a directory.
Run only one `serve` process per state directory. It owns the Store for its whole
life: HTTPS delivery (`serve --https-config PATH --ready-file PATH`), the relay,
agents and every other component borrow that one in-memory Store. Operator commands
(`enrollment`, `mesh`, `agent`) run as separate processes reach it through the
owner-only `owner.sock` control socket while `serve` runs, and open the Store
directly otherwise.

On disk the aggregate is `state.json` (atomically replaced checkpoint) plus
`state.journal` (self-validating delta records appended with one barrier per commit;
bounded to 32 records or max(1 MiB, checkpoint), then checkpointed). Every reader
reconstructs both under the state lock; never read `state.json` alone. `serve` folds
the journal into the checkpoint on orderly shutdown; do so (or run
once with a commit under `QOS_HOST_PERF_BASELINE=1`) before starting an older binary.
`QOS_HOST_PERF_BASELINE=1` restores whole-file commits and the 20 ms busy tick, for
paired measurement only.

The host also serves the signed `tilo-workspace-join/1` protocol on its relay endpoint.
An authenticated Owner issues `tilo.workspace.collaboration` actions through the
certified private relay lane. Invitations contain a signed, service-pinned Iroh hint;
redemption requires a separately rooted device certificate plus proof of possession.
The admitted certificate, role, one-use receipt, retry response, and governed relay
access are persisted before success. Revoking an invitation blocks redemption, while
role changes and removal immediately narrow live relay access. Guest certificates may
use the action transport, but invitation and membership administration still requires
the structural workspace Owner. Reconnects use the same endpoint's signed membership
challenge and proof-of-possession refresh, so they do not consume or recreate an invite.

`qos-relay/1` remains unchanged. Real admitted CRDT state uses retained checkpoints
and deltas. Startup republishes canonical snapshots so a crash between sending and
relay persistence cannot strand the workspace. Application commands, collaboration
administration and media governance all arrive through the certified action broker
(the private relay lane and `/peer/v3/private`); there is no separate addressed action
endpoint. Invocation arguments and receipts never travel through broadcast state or
presence messages. The committed application document changes are shared normally.

Managed inference privately reloads the five-field `{machineId,endpoint,model,token,expiresAt}`
credential from the trusted refresh helper for each dispatch. It accepts only a regular owned
0600 file of at most 8 KiB, rejects symlinks and expired credentials, and requires HTTPS with
the exact machine inference path, no URL credentials, query, or fragment. Invalid credentials
disable model dispatch without disrupting native documents. Bearers never enter receipts,
workspace state or diagnostics. HTTPS redirects and environment proxies are disabled.

The host pins `gpt-4.1-mini-2025-04-14`, sends one user message, limits the serialized message
to 32 KiB and output to 2048 tokens, and returns `{text}` in a completed receipt. The gateway
has a 24-second total deadline within the existing 30-second action deadline. A pending
record is atomically fsynced before HTTP; no Store lock spans the network await. Journal
version 1 explicitly migrates through version 2 to version 3 while preserving terminal
receipts. Version 3 adds hosted invitation, join retry, canonical action intent, and
generation records. Corrupt or incomplete journals fail closed instead of resetting
pending calls.

Gateway idempotency is an RFC 9562 UUIDv8 using a domain-separated BLAKE3 hash of the canonical
JSON tuple `(machine, certified device hex, invocation ID, call index 0)`, persisted in the
pending record. Retry with the identical invocation and arguments reuses this UUID. Timeouts,
disconnects, credential unavailability, `request_pending`, and `service_unavailable` remain
pending. Status exposes that state. Cancellation cannot undo an admitted model request.
HTTP 402 with `insufficient_credit` or `credit_exhausted` becomes `included_credit_exhausted`;
`subscription_required` remains distinct even with a positive balance. Unknown rejection codes
become `model_request_rejected`. Raw server error text is discarded, and funding never changes.
Success and definitive rejection are durable, and later duplicate results cannot overwrite
them. Current roster and model-action capability are rechecked before dispatch/finalization.
Model output is bounded to 32 KiB and is never broadcast through workspace replication.

Validation: `cargo test -p qos-user-identity cloud_enrollment --lib`,
`cargo test -p qos-peer-state`, `cargo test -p qos-cloud-host`.
