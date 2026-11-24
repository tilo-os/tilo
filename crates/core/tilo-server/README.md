# tilo-server

Authenticated iroh transport for Tilo workspace CRDT ops. The
long-running daemon is what a headless Tilo host or an enterprise-tier
tenant runs so devices reach each other and the host across networks
and, if a peer is offline, its deltas are store-and-forwarded.

## What it does

- Binds an iroh endpoint on the `tilo-server/1` ALPN.
- Verifies every inbound peer's `DeviceCert`. A home host defaults to the
  same user identity; shared/provider hosts use an explicit trust policy, and
  each workspace's own governance decides which devices it admits. Certs are Ed25519
  signatures over `blake3("tilo-devcert-v1\0" || postcard(view))`; the
  canonical types live in `tilo-user-identity`.
- Fans out incoming Automerge deltas to every other subscribed
  peer for the same workspace.
- A server holds the workspaces made on it and the ones a governor hands
  over; it never takes one into being on a device's Subscribe. A workspace it
  does not hold refuses the subscription until it is handed over or made there.
- Enqueues deltas for offline members under
  `workspaces/<id>/pending/<hex(device_pubkey)>/<seq>.delta` and
  drains them on next `Subscribe`.
- Retains one compact full checkpoint per shell/app document plus its
  post-checkpoint incremental tail. This bootstraps a device that had not yet
  become a member when the state was published. Connected shells refresh
  checkpoints every ten minutes.
- Bounds every in-memory peer queue. A peer that cannot keep up is disconnected;
  durable traffic is then queued on disk and replayed after it reconnects.
- Carries only the daemon verbs: `init`, `run` and `workspace`.

## Composite home-peer deployment

The headless home-peer deployment runs `tilo-server` with the authenticated
filesystem replica. It retains workspace/app collaboration records,
checkpoints, offline queues, and all filesystem blobs, media/book bytes, and
thumbnails for every workspace in `/etc/tilo/home-peer.toml`. Which workspaces it
holds is up to the workspaces: a workspace's governor adds the home peer as one of the
workspace's hosts.

`ReplicationIntent::FullWorkspace` is backed by the composite host. The server
reads the replica's aggregate freshness proof for every advertisement and
withdraws full-content capacity until every configured workspace is complete.
See
[`os/containers/tilo-server/README.md`](../../../os/containers/tilo-server/README.md)
for the persistent-volume and identity requirements.

## Binaries

- `tilo-server` — the daemon. Systemd `Type=notify`; subcommands `init`,
  `run` and `workspace`. A revocation is a written record in the person's
  workspace, which its readers apply as it reaches them.

## Shell configuration

The shell dials its workspace host directly, whether or not that host is one
of the person's own devices. Configure one host with `TILO_COLLAB_HOST` (an
endpoint id or JSON `EndpointAddr`) or several with `TILO_COLLAB_HOSTS` (a
JSON array of `EndpointAddr`s). Newly minted invite tokens carry these host
addresses so the guest is not dependent on the inviter staying online.

The device's daemon (`tilo-runtime`) reads the configuration; it does not mint
an identity. Before it keeps a server's workspaces, the machine must already
have an enrolled, host-capable device key and cert at the configured
`identity_root`, which a ticket signs it in with. Home-server first-boot
enrollment belongs to the provisioning-image flow, while a hosted service
supplies its provider identity through its control plane.

A desktop session finds the person's own devices on the local network with
sealed probes only those devices can read; nothing on
the network learns the endpoint id. Invite addresses work without it.

## Wire protocol

1. Iroh QUIC (NAT hole-punched, node id is stable).
2. The `tilo-server/1` ALPN, which never changes: the peer protocol version
   is the handshake's first field, and there is one framing per version
   (docs/architecture/compatibility.md). Device-signed private actions,
   zero-signature deltas without the signature, presence batched per
   connection in the order it was posted (`src/presence_mailbox.rs`), and
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
  require the operator/control plane to bind the workspace and add each device key;
  the bearer invite itself does not grant a provider-host account.
- Per-device pending queues are replayed on reconnect and acknowledged only
  after the replay reaches the wire. Retained bootstrap tails
  are compacted by periodic full checkpoints; a permanently offline app that
  never supplies a checkpoint can still accumulate a tail.

## Cost model

What a server costs per active member is measured, not estimated:

- **Egress counters.** The daemon counts every byte it writes to a peer
  session (`bytes_sent`), the presence part of it (`bytes_sent_live`),
  both per workspace, and each connected session's total
  (`Metrics::session_egress`). They count framed bytes before QUIC; the load
  bench adds what the iroh endpoint actually put on UDP.
- **Presence delivery** (`tests/presence_batching.rs`): one member's presence
  reaches another in the order it was posted, a keystroke's caret and typing
  ride in the write its delta takes, and a pointer that comes to rest arrives
  without another message.
- **Bytes per keystroke** (`tests/server_cost.rs`, per change): one
  one-character edit on the production path, frame by frame, with byte
  budgets, and the live egress counter checked against the frames the
  subscribers read.
- **Load bench** (`examples/server_load`, run by
  `tools/ci/perf-scheduled.sh server`): 100 members in workspaces of four, each typing 30
  characters, then pointing for four seconds at 30 Hz (which the server
  forwards one frame per move, in the order it was posted), then idle, every
  minute, against one server process. It records CPU-seconds, resident MiB and
  egress bytes per member-hour; packets per member-hour, from the
  members' own QUIC connection counters (datagrams they sent and of
  those the ones carrying data, datagrams the server sent and of those the
  ones carrying only acknowledgements), so the same bytes in more packets
  fail a budget too; fan-out latency, and the workspace
  store's fsyncs per committed edit (`store::fsyncs`; the durability
  contract is in `src/persistence.rs`).
  `tools/perf/adapters.py server-load` prices each minute with
  `tools/perf/prices.toml` (provider, VM price and date stated there) into $
  per member-hour and per active user-month (88 h);
  `tools/perf/kpi-budgets.toml` holds the ceilings.
- **Timed keystroke writes** (`examples/keystroke_writes`, nightly in the
  same workflow's `server` job): the original 21-keystroke network scenario,
  with one warm-up and 30 ms between edits, asserts a median of one write
  for the delta and its presence. Run with
  `cargo run --locked -p tilo-server --example keystroke_writes`.
  This scheduling-sensitive budget fails the scheduled job directly;
  the normal check tests exact one- and two-write schedules at the production
  writer and separately checks complete network delivery without a timed budget.

```bash
cargo run --release -p tilo-server --example server_load -- \
  --members 100 --minutes 10 --out target/server
python3 tools/perf/record.py env --profile release --renderer none \
  --virtualization none --runner local > target/server/env.json
python3 tools/perf/adapters.py server-load --env-json target/server/env.json \
  target/server/server-load.json
```

See `crates/core/tilo-server/src/lib.rs` for the module overview
and `crates/core/tilo-server/tests/` for end-to-end examples of
the wire protocol.
