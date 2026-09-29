# Certified headless cloud workspace

`tilo-cloud-host` runs native `StatePeer`, Writer and Chat documents and their real
application action implementations. It does not construct a compositor, window,
renderer, framebuffer or video encoder. The app libraries still have UI build/link
dependencies; a model-only crate extraction has not been performed.

Configuration comes from trusted provisioning:

- `TILO_CLOUD_MACHINE_ID`: immutable machine identity (required).
- `TILO_CLOUD_STATE`: private persistent directory, default `/var/lib/tilo-cloud`.
- `TILO_CLOUD_AI_CONFIG`: private rotating credential file, default `/var/lib/tilo-cloud/ai.json`.
- `TILO_CLOUD_PACKET_RELAY`: public HTTPS Iroh packet relay URL (required for apply/serve).

Run as the dedicated `tilo-cloud` account. The SSM wrapper must deliver a bounded
public request file readable by that account. Commands:

```
tilo-cloud-host enrollment inspect
tilo-cloud-host enrollment apply --request-file /run/tilo-cloud/request.json
tilo-cloud-host serve
```

Untrusted Automerge bytes are applied in a re-executed worker process. The deployed
binary is this crate's own `tilo-cloud-host` (`src/main.rs`), which serves those workers
by calling `tilo_automerge_isolate::dispatch_early()` before starting any threads; no
other binary links this library in production.

Inspect creates only a local host device key and a five-minute public challenge.
The browser owns its root/device keys and signs the native certificates and both
root authorization and device-possession proofs. `tilo-user-identity::cloud_enrollment`
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
the journal into the checkpoint on orderly shutdown.
`TILO_HOST_PERF_BASELINE=1` restores whole-file commits and the 20 ms busy tick, for
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

`tilo-relay/1` remains unchanged. Real admitted CRDT state uses retained checkpoints
and deltas. Startup republishes canonical snapshots so a crash between sending and
relay persistence cannot strand the workspace. Application commands, collaboration
administration and media governance all arrive through the certified action broker
(the private relay lane and `/peer/v3/private`); there is no separate addressed action
endpoint. Invocation arguments and receipts never travel through broadcast state or
presence messages. The committed application document changes are shared normally.

Validation: `cargo test -p tilo-user-identity cloud_enrollment --lib`,
`cargo test -p tilo-room`, `cargo test -p tilo-cloud-host`.
