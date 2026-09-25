# Durable local native session

This is an ordinary permissioned native peer for local development. It starts the
existing Iroh packet relay and certified qOS relay; documents use the existing
postcard NeighbourMsg/Automerge/action protocols. It does not stream video or
introduce an HTTP document protocol.

```sh
cargo run -p qos-session-peer -- init /absolute/private/new-session
cargo run -p qos-session-peer -- run /absolute/private/new-session /absolute/private/ready.json
```

For a named workspace with no open apps or app records:

```sh
cargo run --locked -p qos-session-peer -- init-workspace /absolute/private/new-workspace "Research"
```

`init-workspace STATE NAME` trims the name and requires 1–80 Unicode characters,
with no control characters. It persists the name in the room and workspace
directory, initializes native topology custody, and creates app records only
when an app is first opened. Both initialization commands return status JSON.
An existing directory is always rejected; use `status STATE` to verify a prior
initialization before `run`.

`init` requires a new directory and explicitly creates a NEW room and root,
native and relay identities. `run` only restores an existing aggregate. Corrupt,
incomplete or missing identity files never cause reinitialization. The aggregate
stores references to native identity paths; private keys remain in the existing
qos-user-identity native store. Directory custody is exclusive across processes;
a separate identity-custody lock prevents two imported state directories from
running the same native identity concurrently.

While running, trusted local stdin accepts `status`, `flush`, `reconnect`,
`export /absolute/private/export.bin`, and `shutdown`. The loopback `/status`
endpoint contains public metadata only. It says subscribed only after the actual
relay Ready event. Pending retained events are admitted and committed in order
before Ready; reconnect preserves the owner and does not buffer an extra replay
copy. Health requests cannot cancel a pending dial. Retry delay is capped at four
seconds. POST `/enrollment/challenge` and `/enrollment/complete` are serialized through
this same owner. Each viewer supplies its own public key and proves possession;
no browser private key is generated or returned by the server.

Every accepted mutation and its authoritative outgoing checkpoints share one
signed, atomically replaced aggregate with an explicit checked directory fsync.
A returned local CommitReceipt follows that barrier. Failed writes suspend
publication until restore. Relay publication is not a durable governor receipt.
The existing governed coverage messages are only published after the native
aggregate barrier. This is not a new remote receipt protocol. Pending operation
records are retained for later reconciliation and are never executed here.

The aggregate currently has a documented 64MiB local storage budget, enforced
before both write and read. This is a byte resource limit, not a viewer limit.
Snapshots retain full canonical history. Real resource failures must be surfaced;
no implicit history truncation or reseeding is allowed.

## Recovery and migration preflight

`export` writes a signed public-state aggregate, without keys. `SessionOwner::import`
requires the original root/native/host identity stores, validates the export and
canonical state, and takes exclusive identity custody before creating a new state
directory. It never provisions replacement keys. Importing the same identity into
another directory is a custody transfer, not cloning a peer. Keep the original
source directory until the destination is verified. Existing identity references
must remain available; exporting state alone is intentionally not a key backup.

Before handing over any old fixture, inventory these separately:

1. Root signing authority, native signing identity, and certified relay identity.
2. Signed canonical room, documents, roster, revocations, and actor replay floors.
3. Directory and pending outgoing/effect journal.

If any identity is unavailable, the old room cannot be truthfully resumed with
new keys. Retain its archive and produce a reviewed content-only import plan that
names the old room and record IDs, verifies retained checkpoint signatures and
causal closure, renders recoverable content, and explicitly names a new authority
and room. Do not import trust, replay acknowledgements, or unknown effects, or
claim same-room continuity. This crate does not automatically migrate or replace
an existing fixture.

This local host is not a production multi-tenant account service. Remote broker
addressed commands, production account enrollment, browser lease UI and revocation
controls are separate integration work. The existing fixture
example is unchanged.

## Authorized dynamic enrollment

The first `run` creates `STATE/enrollment-capability` with a persistent random
64-hex bearer capability in an owner-only0600 file. It is separate from public
ready/build/status metadata. Give its filename to the local proxy through
`QOS_PEER_CAPABILITY_FILE`; never put its contents in URLs or browser JavaScript.
The proxy sends `Authorization: Bearer <token>`, exact native Host and frontend
`Origin: http://127.0.0.1:9998`. Public GET `/status` does not require the capability.
Enrollment POST does. Requests are limited to16KiB with bounded headers/timeouts;
unknown, replayed, expired, revoked and wrong-scope proof requests fail explicitly.

Ready JSON has version2, enrollment_version2, fixture_origin, status_path,
challenge_path and complete_path. It contains no capability/token. The proxy
should wait for actual `/status` native_peer `subscribed` before enrolling.

Certificates, immutable key/request idempotency entries, governed roster, full
directory history and relay membership intent commit atomically in aggregatev2.
Relay membership reconciles before a bootstrap response. On a lost response or
membership failure the client obtains a fresh challenge for the SAME key/request
and retrieves the SAME certificate with current transport addresses. A native
restart preserves identity and issuance. Valid signed aggregatev1 state migrates
losslessly to v2; failures never create replacement identities.

A complete signed directory snapshot includes dependency closure for an empty
peer after any previous publication cursor. Bootstrapv2 is public-only Rust JSON;
timestamp-bearing fields must reach browser Rust without JavaScript number
re-serialization. Challenge proof bytes are defined once by qos-peer-state.

For an explicitly reviewed content migration, run before starting/enrolling:

```sh
qos-session-peer init STATE
qos-session-peer import-reviewed STATE ARCHIVE_DIRECTORY REVIEWED_MANIFEST_BLAKE3
qos-session-peer run STATE READY_JSON
```

The import is a separate reviewed content operation into a new authority/room.
It requires a fresh generation1 destination and an independently approved manifest
digest. It does not import old signing identity, acknowledgements or pending effects.
