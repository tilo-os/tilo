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
- Enqueues deltas for offline members under
  `workspaces/<id>/pending/<hex(device_pubkey)>/<seq>.delta` and
  drains them on next `Subscribe`.
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
2. `tilo-relay/1`, `tilo-relay/3` (private actions and agent reads),
   `tilo-relay/4` (device-signed actions), `tilo-relay/5` (`/4` whose
   zero-signature deltas go without the signature), `tilo-relay/6` (`/5`
   whose presence arrives batched and slot-coded) or `tilo-relay/7` (`/6`
   whose live deltas arrive coded against the previous one from the same
   author) ALPN. Every protocol
   gets its presence coalesced and paced per connection
   (`src/presence_mailbox.rs`).
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

## Cost model

What a relay costs per active collaborator is measured, not estimated:

- **Egress counters.** The daemon counts every byte it writes to a peer
  session (`bytes_sent`), the live fan-out part of it (`bytes_sent_live`),
  both per room, and each connected session's total
  (`Metrics::session_egress`). `tilo-relay-admin status` prints them.
  They count framed bytes before QUIC; the load bench adds what the iroh
  endpoint actually put on UDP.
- **Bytes per keystroke** (`tests/hosting_cost.rs`, per change): one
  one-character edit on the production path, frame by frame, with byte
  budgets, and the live egress counter checked against the frames the
  subscribers read.
- **Load bench** (`examples/relay_load`, nightly in
  `perf-scheduled.yml`): 100 collaborators in rooms of four, each typing 30
  characters, then pointing for four seconds at 30 Hz, then idle, every
  minute, against one relay process. It records CPU-seconds, resident MiB and
  egress bytes per collaborator-hour, fan-out latency, and the workspace
  store's fsyncs per committed edit (`store::fsyncs`; the durability
  contract is in `src/persistence.rs`).
  `tools/perf/adapters.py hosting-load` prices each minute with
  `tools/perf/prices.toml` (provider, VM price and date stated there) into $
  per collaborator-hour and per active user-month (88 h);
  `tools/perf/kpi-budgets.toml` holds the ceilings.

```bash
cargo run --release -p tilo-relay --example relay_load -- \
  --collaborators 100 --minutes 10 --out target/hosting
python3 tools/perf/record.py env --profile release --renderer none \
  --virtualization none --runner local > target/hosting/env.json
python3 tools/perf/adapters.py hosting-load --env-json target/hosting/env.json \
  target/hosting/hosting-load.json
```

See `crates/hosting/tilo-relay/src/lib.rs` for the module overview
and `crates/hosting/tilo-relay/tests/` for end-to-end examples of
the wire protocol.
