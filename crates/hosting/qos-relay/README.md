# qos-relay

Authenticated iroh transport for qOS workspace CRDT ops. The
long-running daemon is what a headless qOS host or an enterprise-tier
tenant runs so devices reach each other and the host across networks
and, if a peer is offline, its deltas are store-and-forwarded.

## What it does

- Binds an iroh endpoint on the `qos-relay/1` ALPN.
- Verifies every inbound peer's `DeviceCert`. A home host defaults to the
  same user identity; shared/provider hosts can use an explicit trust policy
  plus per-workspace device membership. Certs are Ed25519
  signatures over `blake3("qos-devcert-v1\0" || postcard(view))`; the
  canonical types live in `qos-user-identity`.
- Fans out incoming Automerge deltas to every other subscribed
  peer for the same workspace.
- A default same-owner home host auto-binds and pins rooms on first subscribe;
  shared/provider hosts require an explicit binding.
- Enqueues deltas for offline members under
  `workspaces/<id>/pending/<hex(device_pubkey)>/<seq>.delta` and
  drains them on next `Subscribe`.
- Retains one compact full checkpoint per shell/app document plus its
  post-checkpoint incremental tail. This bootstraps a device that had not yet
  become a member when the state was published. Connected shells refresh
  checkpoints every ten minutes.
- Bounds every in-memory peer queue. A peer that cannot keep up is disconnected;
  durable traffic is then queued on disk and replayed after it reconnects.
- Ships a `qos-relay-admin` CLI for binding/unbinding workspaces
  and listing what a running relay is pinning.

## Standalone deployment boundary

The standalone `qos-relay` image and Compose deployment are **CRDT relay
only**. They retain workspace/app collaboration records, checkpoints, and
offline queues; they do not retain filesystem blobs, media/book bytes, or
full-resolution thumbnails. They also do not run app/compute workers or a
remote-control tunnel. The default configuration therefore advertises only
workspace replication and inbound collaboration relay.

`ReplicationIntent::FullWorkspace` is an explicit request for a composite
host that has also started the authenticated filesystem/blob host plane. The
relay binary clamps content, app, compute, and remote-control capability
advertisements off until those backing services exist. For the current
operator deployment, use `WorkspaceOnly` (the default). See
[`os/containers/qos-relay/README.md`](../../../os/containers/qos-relay/README.md)
for the persistent-volume and identity requirements.

## Binaries

- `qos-relay` — daemon. Systemd `Type=notify`; subcommands `init`,
  `run`, `status`, `bind-workspace`, `unbind-workspace`.
- `qos-relay-admin` — helper: `bind-room`, `bind-workspace`, membership
  add/remove/list, status, and root-signed `revoke-device`. Membership and
  revocation changes are reloaded by a running daemon within one second.

## Shell configuration

The shell keeps direct Iroh gossip enabled even when a durable host is
attached. Configure one host with `QOS_COLLAB_HOST` (an endpoint id or JSON
`EndpointAddr`) or several with `QOS_COLLAB_HOSTS` (a JSON array of
`EndpointAddr`s). Newly minted invite tokens carry these host addresses so the
guest is not dependent on the inviter staying online.

`qos-relay init` writes configuration; it does not mint an identity. Before
`run`, the host must already have an enrolled, host-capable device key and cert
at the configured `identity_root`. Home-server first-boot enrollment belongs to
the provisioning-image flow, while a hosted service supplies its provider
identity through its control plane.

LAN discovery is deliberately opt-in because it advertises the device's Iroh
endpoint id and local addresses on the current network. Set
`QOS_COLLAB_LAN_DISCOVERY=1` to enable the qOS-specific
`_qos-collab-v1._udp.local` service. Invite addresses work without it.

## Wire protocol

1. Iroh QUIC (NAT hole-punched, node id is stable).
2. `qos-relay/1` ALPN.
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
  after the replay reaches the wire. Retained bootstrap tails
  are compacted by periodic full checkpoints; a permanently offline app that
  never supplies a checkpoint can still accumulate a tail.

See `crates/hosting/qos-relay/src/lib.rs` for the module overview
and `crates/hosting/qos-relay/tests/` for end-to-end examples of
the wire protocol.
