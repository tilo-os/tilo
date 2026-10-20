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
2. The `tilo-relay/1` ALPN, which never changes: the peer protocol version
   is the handshake's first field, and there is one framing per version
   (docs/architecture/compatibility.md). Device-signed private actions,
   zero-signature deltas without the signature, presence coalesced, paced,
   batched and slot-coded per connection (`src/presence_mailbox.rs`), and
   live deltas coded against the previous one from the same author;
   presence that may go early rides in the same write as a delta.
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

- Who-is-here lines (`/peer/v1/here`, feature `web`) live in memory only, per
  workspace and device, for five minutes after a person's last post, and
  for at most 4096 people per workspace. Model prose for them is off unless
  `TILO_RELAY_SUMMARY_*` configures a provider, and it is capped per person,
  per call and per hour. The variables are in
  `crates/hosting/tilo-room-service/README.md`.

## Cost model

What a relay costs per active collaborator is measured, not estimated:

- **Egress counters.** The daemon counts every byte it writes to a peer
  session (`bytes_sent`), the live fan-out part of it (`bytes_sent_live`),
  both per room, and each connected session's total
  (`Metrics::session_egress`). `tilo-relay-admin status` prints them.
  They count framed bytes before QUIC; the load bench adds what the iroh
  endpoint actually put on UDP.
- **Presence bytes** (`peer::state_tests::presence_cost`): a controlled
  60-move ingress schedule exercises production presence routing and writing
  on both `/5` and `/6`. The actual accepted bytes must cost at most 70 bytes
  per delivered presence on `/6`, and under a third of `/5`; both streams
  must decode to identical attributed final states and event order. The
  network scenario in `tests/presence_coalescing.rs` separately covers protocol
  negotiation and delivery without using its scheduler-dependent byte mix
  as a budget gate.
- **Bytes per keystroke** (`tests/hosting_cost.rs`, per change): one
  one-character edit on the production path, frame by frame, with byte
  budgets, and the live egress counter checked against the frames the
  subscribers read.
- **Load bench** (`examples/relay_load`, nightly in
  t's `perf-hosting` check): 100 collaborators in rooms of four, each typing 30
  characters, then pointing for four seconds at 30 Hz (which each client's
  session writer paces to the relay's 20 Hz before it is sent), then idle, every
  minute, against one relay process. It records CPU-seconds, resident MiB and
  egress bytes per collaborator-hour; packets per collaborator-hour, from the
  collaborators' own QUIC connection counters (datagrams they sent and of
  those the ones carrying data, datagrams the relay sent and of those the
  ones carrying only acknowledgements), so the same bytes in more packets
  fail a budget too; fan-out latency, and the workspace
  store's fsyncs per committed edit (`store::fsyncs`; the durability
  contract is in `src/persistence.rs`).
  `tools/perf/adapters.py hosting-load` prices each minute with
  `tools/perf/prices.toml` (provider, VM price and date stated there) into $
  per collaborator-hour and per active user-month (88 h);
  `tools/perf/kpi-budgets.toml` holds the ceilings.
- **Timed keystroke writes** (`examples/keystroke_writes`, nightly in the
  same workflow's `hosting` job): the original 21-keystroke network scenario,
  with one warm-up and 30 ms between edits, asserts a median of one write
  for the delta and its presence. Run with
  `cargo run --locked -p tilo-relay --example keystroke_writes`.
  This scheduling-sensitive budget fails the scheduled job directly;
  the normal gate tests exact one- and two-write schedules at the production
  writer and separately checks complete network delivery without a timed budget.

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
