# collab-broker

The production fan-out server for `QUICK_COLLAB_SOCK` — the app-level
collaboration socket (Plane A). It relays opaque CRDT + presence frames
between every connected app instance, generalizing the test relay in
`ui/tests/real_socket_two_peer.rs` from two peers to N.

All relay logic lives in `ui::collaboration::broker`; this binary is a
thin wrapper so the logic is unit-tested from `ui` (`real_socket_n_peer`).

## Run the two-instance notes demo (one machine)

```bash
cargo build -p notes -p collab-broker

# 1. Start the broker on a socket path.
./target/debug/collab-broker /tmp/notes.sock &

# 2. Start two notes instances pointed at it, each with its own identity
#    (drives the Automerge actor, presence attribution, and peer color).
QUICK_COLLAB_SOCK=/tmp/notes.sock QUICK_APP_ID=alice ./target/debug/notes &
QUICK_COLLAB_SOCK=/tmp/notes.sock QUICK_APP_ID=bob   ./target/debug/notes
```

The first instance seeds a Welcome workspace; the second adopts it via
the late-join handshake (it broadcasts `Arrived`, the first replies with
full state). Then, between the two windows you should see:

- **Live text** — typing in either window appears in the other.
- **Round pointer** — each peer's mouse as a color-coded dot.
- **Caret** — each peer's text caret, in their color, on the right block.
- **Selection** — each peer's selection highlighted in their color.
- **Authorship** — the text a peer wrote, marker-highlighted in their color.
- **Focus glow** — an outline on the block a peer is editing.
- **Indicative mist** — when a peer is on a *different* page, the sidebar
  row that leads there glows in their color.

`selected` (which page each instance views) is per-peer, so the two
windows can read different pages while sharing one document — which is
exactly what the indicative mist is for.

## How it bridges to LAN (later)

This is Path 1 (local). Path 2 swaps this binary for the **shell** binding
`QUICK_COLLAB_SOCK` and bridging the same frames out over libp2p
(Plane B), so the identical demo works across two laptops. No app or
protocol change is required — the apps already speak this protocol.

The cross-app / window-group-tab variant of indicative mist (a peer in a
*different app* in the same window group) is the same `draw_peer_mist`
primitive fed by the shell's chrome; it's designed-for but not built in
this milestone (it needs the shell running both apps in a group, which two
standalone notes instances can't reproduce).
