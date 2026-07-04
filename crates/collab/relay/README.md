# qos-relay

Standalone libp2p gossipsub relay for the qOS collaboration mesh.

Phase 9 of `docs/superpowers/specs/2026-06-29-spaces-channels-and-cryptographic-roles.md`.

## What it is

A long-lived libp2p node that forwards gossipsub messages on the qOS
collaboration topics so two shells behind NAT can reach each other
without a direct connection. It also runs a Kademlia DHT in server
mode so distant peers can rendezvous given a single bootstrap relay.

The relay is **not** a member of any channel. Today it sees plaintext
JSON-encoded `NeighbourMsg` payloads (matching `shell/src/network/
protocol.rs`). Once MLS encryption ships (Phase 7 of the collab
spec) the relay will see only ciphertext plus metadata — topic name,
message size, timestamp, signing peer-id. The MLS switch is a layer
below `qos-relay` and requires no relay-side changes.

## Architecture

```
       +---------+         +-----------+         +---------+
       | shell A | <-----> | qos-relay | <-----> | shell B |
       +---------+ libp2p  +-----------+ libp2p  +---------+
                  gossipsub mesh + kademlia DHT
```

- **Gossipsub** (libp2p 0.56, same version as the shell, same heartbeat
  + mesh sizes + strict signed authenticity) is the message-forwarding
  protocol. The relay subscribes to every allowlisted topic it sees a
  peer announce, so by mesh participation it forwards traffic to
  everyone else in the mesh.
- **Kademlia** runs in `Server` mode so NATted clients can use the
  relay as a rendezvous point. Bootstrap multiaddrs in the config seed
  the routing table at startup.
- **Identify** announces `qos-relay/<version>` as the agent string;
  operators can fingerprint the network's relay topology with that.
- **mDNS** auto-discovers LAN peers (useful for self-hosted home
  setups). Silent no-op on hosts that block multicast.
- **Ping** keeps connections through stateful NATs alive.

## Install + run

### From source

```bash
cargo install --path qos-relay
qos-relay --help
```

### Try it out locally

```bash
cargo run -p qos-relay --release -- \
    --listen /ip4/127.0.0.1/tcp/4001 \
    --identity /tmp/qos-relay-dev.key \
    --log "qos_relay=debug,libp2p=info"
```

The first run generates `/tmp/qos-relay-dev.key` (mode 0600). Note the
`local_peer_id = ...` line in the log — that's the relay's stable
peer-id. Hand it to a peer as `/ip4/127.0.0.1/tcp/4001/p2p/<peer-id>`.

## Systemd unit

```bash
sudo install -m 0755 target/release/qos-relay /usr/local/bin/qos-relay
sudo install -d -m 0750 /etc/qos-relay
sudo install -m 0640 qos-relay/example-config.toml /etc/qos-relay/config.toml
sudo install -m 0644 qos-relay/deploy/qos-relay.service \
    /etc/systemd/system/qos-relay.service
sudo useradd --system --create-home --home-dir /var/lib/qos-relay qos-relay
sudo systemctl daemon-reload
sudo systemctl enable --now qos-relay
sudo journalctl -fu qos-relay
```

The unit is hardened (`ProtectSystem=strict`, `MemoryDenyWriteExecute`,
`SystemCallFilter=@system-service`, etc.) so a compromised relay
process can't pivot to the rest of the host.

## Container

```bash
# Build (rootless podman; run from the workspace root)
podman build -t qos-relay:latest -f qos-relay/deploy/Containerfile .

# Run with the default config
podman run -d --name qos-relay \
    -p 4001:4001/tcp -p 4001:4001/udp \
    -v qos-relay-state:/var/lib/qos-relay \
    qos-relay:latest --identity /var/lib/qos-relay/identity.key

# Run with a custom config
podman run -d --name qos-relay \
    -p 4001:4001/tcp -p 4001:4001/udp \
    -v qos-relay-state:/var/lib/qos-relay \
    -v $PWD/my-config.toml:/etc/qos-relay/config.toml:ro \
    qos-relay:latest --config /etc/qos-relay/config.toml
```

## Configuration

`qos-relay --config /etc/qos-relay/config.toml` loads TOML. Failing
that, the binary reads `$QOS_RELAY_CONFIG`, then falls through to the
in-binary defaults. CLI flags (`--listen`, `--bootstrap`, `--metrics`,
`--log`, `--identity`) always override individual fields.

```toml
# Multiaddrs the relay listens on. Dual-stack TCP + QUIC by default.
listen = ["/ip4/0.0.0.0/tcp/4001", "/ip4/0.0.0.0/udp/4001/quic-v1"]

# Persistent ed25519 keypair. Generated on first run with mode 0600.
identity_path = "/var/lib/qos-relay/identity.key"

# Bootstrap multiaddrs — other relays to seed Kademlia from. Each MUST
# include /p2p/<peerid>. Empty for the first relay in a network.
bootstrap = [
    "/dns4/relay-a.example.com/tcp/4001/p2p/12D3KooW...",
]

# Topic glob allowlist. `qos.*` matches every shell-published topic.
topic_allowlist = ["qos.*"]

# Rate limit per (topic, peer) per minute. 0 disables.
rate_limit_per_topic_per_minute = 6000

# Optional Prometheus exposition endpoint.
metrics_listen = "127.0.0.1:9090"

# Log level filter (`tracing_subscriber::EnvFilter` syntax).
log_level = "info"
```

A fully-annotated example lives at `qos-relay/example-config.toml`.

## Bootstrap pattern

To run multiple relays for redundancy:

1. Start the first relay with `bootstrap = []`. Note its peer-id from
   the startup log.
2. Start every subsequent relay with the previous relays' multiaddrs in
   its `bootstrap` list. Multiaddrs must end in `/p2p/<peerid>` so each
   dial can be authenticated.
3. Shells connect to *any one* of the relays. Kademlia handles the rest
   — once a shell is in the DHT it can find every other peer that has
   joined any of the relays.

A community-run setup with three relays gives you:
- N+2 redundancy: any two relays can fail and the mesh stays connected.
- DHT bootstrap diversity: clients with stale bootstrap lists still
  rendezvous as long as one entry is alive.

## Trust model

- **The relay forwards; it does not author.** Gossipsub messages carry
  their original signer's `PeerId` in the `propagation_source` field;
  the relay's only signature is on its own outgoing IHAVE/IWANT
  control messages.
- **Today's wire format is plaintext.** A compromised relay can read
  CRDT contents. This matches the current shell, which also publishes
  plaintext. Treat the relay's threat model as identical to a passive
  network observer until Phase 7 lands.
- **Phase 7 (MLS) makes the relay blind.** When channel members
  encrypt under their MLS group key, the relay sees only
  `(topic, ciphertext, signer)`. No relay-side changes are required —
  gossipsub doesn't inspect the payload.
- **Persistent identity.** The relay's ed25519 keypair lives in
  `identity_path` with mode 0600. Loss or theft of that key lets an
  attacker impersonate the relay, so back it up with the same care as
  any production secret. The peer-id derived from it appears in
  community-published bootstrap lists; rotating it costs an
  announcement.

## Monitoring

If `metrics_listen` is set, `GET /metrics` serves OpenMetrics text:

- `qos_relay_connected_peers` (gauge) — currently-connected libp2p
  peers.
- `qos_relay_messages_forwarded_total{topic}` (counter) — gossipsub
  messages observed per topic.
- `qos_relay_bytes_in_total` / `qos_relay_bytes_out_total` (counters)
  — approximate gossipsub payload bytes.
- `qos_relay_kademlia_queries_total` (counter) — outbound DHT query
  completions.
- `libp2p_*` — libp2p-internal metrics (mesh state, connection
  churn, identify cadence) via the `libp2p::metrics` feature.

`GET /healthz` returns `200 ok` once the listener is up — point your
load balancer at it.

## Tests

```bash
cargo test -p qos-relay
```

Notable:

- `tests/relay_forwards_gossipsub.rs` spins up an in-process relay plus
  two libp2p peers that dial *only* the relay (never each other) and
  asserts the message they exchange traversed the relay.
- `tests/identity_persistence.rs` verifies the keypair survives
  restarts, is mode 0600, and surfaces a clear error (not a panic) on
  a corrupted file.
- `tests/config_roundtrip.rs` covers default values, partial-config
  field inheritance, and clear failure messages on bad TOML.

## Versioning

Pre-0.1.0. The public crate API (`Relay`, `RelayConfig`, etc.) exists
so integration tests can spawn a relay in-process; downstream code
should depend on the binary, not the library.
