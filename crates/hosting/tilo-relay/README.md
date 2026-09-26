# tilo-relay

Authenticated iroh transport for Tilo workspace CRDT ops. The
long-running daemon is what a headless Tilo host or an enterprise-tier
tenant runs so devices reach each other and the host across networks
and, if a peer is offline, its deltas are store-and-forwarded.

## What it does

- Binds an iroh endpoint on the `tilo-relay/1` ALPN.
- Verifies every inbound peer's `DeviceCert`. A home host defaults to the
  same user identity; shared/provider hosts can use an explicit trust policy
  plus per-workspace device membership. Certs are Ed25519
  signatures over `blake3("tilo-devcert-v1\0" || postcard(view))`; the
  canonical types live in `tilo-user-identity`.
- Fans out incoming Automerge deltas to every other subscribed
  peer for the same workspace.
- A default same-owner home host auto-binds and pins rooms on first subscribe;
  shared/provider hosts require an explicit binding.
- Enqueues non-retained deltas for offline members under
  `workspaces/<id>/pending/<hex(device_pubkey)>/<seq>.delta` and
  drains them on next `Subscribe`. Retained document traffic (below) is
  stored once per document and never copied into per-member queues.
- Retains one compact full checkpoint per shell/app document plus its
  post-checkpoint incremental tail. This bootstraps a device that had not yet
  become a member when the state was published. Connected shells refresh
  checkpoints every ten minutes.
- Bounds every in-memory peer queue. A peer that cannot keep up is disconnected;
  durable traffic is then queued on disk and replayed after it reconnects.
- Ships a `tilo-relay-admin` CLI for binding/unbinding workspaces
  and listing what a running relay is pinning.

## Composite home-peer deployment

The headless home-peer deployment runs `tilo-relay` with the authenticated
filesystem replica. It retains workspace/app collaboration records,
checkpoints, offline queues, and all filesystem blobs, media/book bytes, and
thumbnails for every workspace in `/etc/tilo/home-peer.toml`.

`ReplicationIntent::FullWorkspace` is backed by the composite host. The relay
reads the replica's aggregate freshness proof for every advertisement and
withdraws full-content capacity until every configured workspace is complete.
See
[`os/containers/tilo-relay/README.md`](../../../os/containers/tilo-relay/README.md)
for the persistent-volume and identity requirements.

## Binaries

- `tilo-relay` — daemon. Systemd `Type=notify`; subcommands `init`,
  `run`, `status`, `bind-workspace`, `unbind-workspace`.
- `tilo-relay-admin` — helper: `bind-room`, `bind-workspace`, membership
  add/remove/list, status, and root-signed `revoke-device`. Membership and
  revocation changes are reloaded by a running daemon within one second.

## Shell configuration

The shell keeps direct Iroh gossip enabled even when a durable host is
attached. Configure one host with `TILO_COLLAB_HOST` (an endpoint id or JSON
`EndpointAddr`) or several with `TILO_COLLAB_HOSTS` (a JSON array of
`EndpointAddr`s). Newly minted invite tokens carry these host addresses so the
guest is not dependent on the inviter staying online.

`tilo-relay init` writes configuration; it does not mint an identity. Before
`run`, the host must already have an enrolled, host-capable device key and cert
at the configured `identity_root`. Home-server first-boot enrollment belongs to
the provisioning-image flow, while a hosted service supplies its provider
identity through its control plane.

LAN discovery is deliberately opt-in because it advertises the device's Iroh
endpoint id and local addresses on the current network. Set
`TILO_COLLAB_LAN_DISCOVERY=1` to enable the Tilo-specific
`_tilo-collab-v1._udp.local` service. Invite addresses work without it.

## Wire protocol

1. Iroh QUIC (NAT hole-punched, node id is stable).
2. `tilo-relay/1` ALPN.
3. Three-message auth handshake — mutual proof-of-possession of certified
   device signing keys. Home hosts require the same user root; provider hosts
   authenticate their pinned host identity and authorize tenant devices with
   workspace membership.
4. Bidirectional stream of `postcard`-framed `Envelope`s:
   `Subscribe`, `Unsubscribe`, `Delta`, `Presence`, `Ready`,
   `NotAuthorised`, `Ping`, `Pong`.

## Operational boundaries

- Same-owner home hosts auto-enrol authenticated devices. Shared/rented hosts
  require the operator/control plane to bind the room and add each device key;
  the bearer invite itself does not grant a provider-host account.
- Per-device pending queues are replayed on reconnect and acknowledged only
  after the replay reaches the wire.
- A retained tail is trimmed only from its front, and only up to what a newer
  checkpoint covers, so a joiner never receives a change whose dependencies
  were dropped. A checkpoint covers what its author had been handed before
  the relay last sent it `Ready` (after its subscribe replay, or as a
  checkpoint request); only then may it replace the base checkpoint. Any
  other checkpoint — forwarded by a peer relay, or from a device not
  subscribed here — is kept as a tail entry instead, adding its state
  without removing any.

## Limits

One relay is sized to host a public room of a few thousand members, most of
them offline at any moment. Fan-out encodes each delta once and hands the same
bytes to every live subscriber; a retained message costs one disk write for
the whole room, not one per offline member.

| Limit | Default | Where | When exceeded |
|---|---|---|---|
| Concurrent connections | 8192 | `[limits] max_connections` | The new connection is closed during accept (`relay connection limit reached`). |
| Live subscribers per workspace | 4096 | `[limits] max_subscribers_per_workspace` | `Subscribe` is answered `NotAuthorised` (`workspace subscriber limit reached`); an already-subscribed device may always resubscribe. |
| Live backlog per connection | 1024 frames | built in | The peer is disconnected; on reconnect it replays the retained store and its pending queue. Frames are shared, so the backlog holds pointers, not copies. |
| Pending queue per offline device | 4096 entries / 64 MiB | built in | In a pinned workspace every queued Automerge change is replaced by one delta carrying the whole materialized document, so the device receives no change without its dependencies. An unpinned workspace holds no document to replace history with and drops the oldest quarter. |
| Retained tail per document (soft) | 16384 entries / 256 MiB | built in | Nothing is dropped. The relay re-sends `Ready` to the most recent connected sender of that document (at most every 30 s); the full checkpoint a shell answers with lets the covered front be trimmed. |
| Retained tail per document (hard) | 65536 entries / 1 GiB | built in | Still accepted; the connected member that retained the most bytes of that tail is disconnected. Its reconnect replays, receives `Ready`, and publishes the covering checkpoint. |
| Audit log | 16 MiB | built in | `log.jsonl` rotates to `log.jsonl.1`, replacing the previous rotation. |
| Inbound frame | 64 MiB | wire protocol | The connection ends with a frame error. |

```toml
[limits]
max_connections = 8192
max_subscribers_per_workspace = 4096
```

A byte-identical republish of a document's current checkpoint (members
republish on every attach) is recognised and neither fanned out nor stored
again. Membership files are re-read only after they change, so the
once-a-second access audit of every subscriber costs a `stat`, not a parse.

## Hosting a public room

A public room, such as the Tilo community room, is owned and governed by its
owner's shell. This relay only admits people to it. See
`src/public_room.rs` and `docs/architecture/feedback.md`, and
`tilo-relay-admin community --help` for the commands:

- `community host --room ROOM --owner-identity-root DIR` binds the room here
  and signs, with the owner's root, a delegation for this relay's device to
  admit at most Commenters.
- `community add-invite --room ROOM TOKEN` registers the room's standing
  invitation. `revoke-invite` stops new joins through it, and `status` shows
  what is hosted.
- Run with `trust_policy = { kind = "allow_any" }`, so certified strangers may
  connect. Each room stays gated by its own membership. A device joins by
  sending `Redeem` before `Subscribe`.
- Direct rooms between anyone and a hosted room's owner are bound on first
  subscribe and admit exactly those two people.

See `crates/hosting/tilo-relay/src/lib.rs` for the module overview
and `crates/hosting/tilo-relay/tests/` for end-to-end examples of
the wire protocol.
