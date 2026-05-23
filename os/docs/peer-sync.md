# Peer-sync (libp2p) — opt-in

qOS's collaboration features rely on the `shell`
compositor speaking libp2p (gossipsub + mDNS) to discover and sync with
nearby peers. This is implemented behind the `--enable-peer-sync` flag
on the compositor binary, and **disabled by default** in the OS image
for privacy reasons.

## Why it's off by default

- **mDNS** broadcasts the device's presence on the local network, which
  on a laptop in a public space leaks the hostname / user to anyone
  listening.
- **gossipsub** accepts inbound connections from any peer that finds us
  via mDNS, which expands the attack surface.
- Per-cluster sandboxing means a malicious peer can only see what's
  been explicitly shared, but **discovery itself** still leaks
  metadata. The default-off posture matches the rest of nirios's
  "explicit consent" model.

## How to opt in

There are two supported paths:

### Path 1 — one-shot, this session only

At the greetd login prompt, select the **"nirios (peer-sync)"** session
entry instead of plain "nirios". This invokes
`shell --enable-peer-sync` for that one session; logging out
returns to the default no-peer-sync compositor.

### Path 2 — persistent, until you turn it off

```sh
systemctl --user enable --now shell-peer-sync.service
```

This sets a marker file at
`$XDG_CONFIG_HOME/shell/peer-sync.enabled` (or
`~/.config/qos/peer-sync.enabled` when `XDG_CONFIG_HOME` is
unset) and persists across reboots. The compositor checks for this
marker on launch and enables peer-sync without needing the
`--enable-peer-sync` CLI flag. To disable:

```sh
systemctl --user disable --now shell-peer-sync.service
```

## What it enables

When `--enable-peer-sync` is on, the compositor:

1. Starts a libp2p `Swarm` with mDNS + gossipsub transports
   (`shell/src/shell/network.rs`).
2. Joins a `nirios.cluster.<id>` gossipsub topic per cluster you share.
3. Accepts incoming Wayland buffer streams from invited peers
   (Peer-sync Phase 4 — H.264-encoded).
4. Forwards local cursor positions over the gossipsub topic so other
   peers can render your presence (`docs/architecture/peer-sync.md` in
   the main repo).

Sandboxing remains the security boundary: each cluster is still a
Bubblewrap-isolated namespace, and peer-sync only widens the *control
plane*, not the data plane.

## Auditing what's running

```sh
# Is the marker present?
ls -la "${XDG_RUNTIME_DIR}/nirios/"

# Is mDNS broadcasting? (will list our service if the flag is on)
avahi-browse -a -t 2>/dev/null | grep -i nirios

# Is the libp2p swarm logging? (look for gossipsub topic subscriptions)
journalctl --user -u graphical-session.target | grep -E 'libp2p|gossipsub|mdns'
```

## Future work

The proper config story is a TOML file under
`/etc/qos/peer-sync.toml` (or `~/.config/qos/`) that
selects between *off / loopback-only / lan / public* discovery scopes.
The current binary flag is a v1 placeholder; track the follow-up in
`docs/superpowers/plans/2026-05-08-peer-sync.md`.
