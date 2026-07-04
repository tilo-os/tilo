# qos-relay

Authenticated iroh transport for qOS workspace CRDT ops. The
long-running daemon a headless qOS host or an enterprise-tier
tenant runs so devices reach each other + the host across networks
and, if a peer is offline, its deltas are store-and-forwarded.

## What it does

- Binds an iroh endpoint on the `qos-relay/1` ALPN.
- Verifies every inbound peer's `DeviceCert` (SP1 identity) chains
  to the same user identity as this relay (default `TrustPolicy`).
- Fans out incoming Automerge deltas to every other subscribed
  peer for the same workspace.
- Pins configured workspaces on disk (persistent CRDT snapshot).
- Enqueues deltas for offline members under
  `workspaces/<id>/pending/<hex(device_pubkey)>/<seq>.delta` and
  drains them on next `Subscribe`.
- Ships a `qos-relay-admin` CLI for binding/unbinding workspaces
  and listing what a running relay is pinning.

## Binaries

- `qos-relay` — daemon. Systemd `Type=notify`; subcommands `init`,
  `run`, `status`, `bind-workspace`, `unbind-workspace`.
- `qos-relay-admin` — helper: `bind-workspace`, `unbind-workspace`,
  `list-workspaces`, `status`, `revoke-device` (stub — needs the
  user root private key on the same host).

## Wire protocol

1. TCP → iroh QUIC (NAT hole-punched, node id is stable).
2. `qos-relay/1` ALPN.
3. Three-message auth handshake — mutual proof-of-possession of a
   device signing key that chains to the shared user identity.
4. Bidirectional stream of `postcard`-framed `Envelope`s:
   `Subscribe`, `Unsubscribe`, `Delta`, `Presence`, `Ready`,
   `NotAuthorised`, `Ping`, `Pong`.

## Not (yet) done

- Inter-relay CRDT federation: two relays bound to the same
  workspace merge as any other Automerge peers do, but the wire
  binding — one relay dialling another and exchanging deltas — is
  future work. Today they operate as independent pinning replicas.
- Directory-driven bindings: workspace bindings come from the
  admin CLI (which edits `/etc/qos/relay.toml`). Reading them
  from a `WorkspaceDirectory` CRDT the user's primary device
  published lands with SP3.
- Store-and-forward compaction: pending queues grow unbounded
  today. A compaction pass that merges consecutive deltas from
  the same author before delivery is the follow-up.

See `crates/hosting/qos-relay/src/lib.rs` for the module overview
and `crates/hosting/qos-relay/tests/` for end-to-end examples of
the wire protocol.
