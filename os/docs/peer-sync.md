# Peer sync (Iroh)

The collaborative session starts the shell's Iroh transport. It supports an
ad-hoc direct invite and an optional always-on workspace host at the same time.

The normal desktop remains conservative: peer sync is enabled by the
`shell-peer-sync.desktop` session or `--enable-peer-sync`. Once enabled, the
shell loads `/var/lib/qos/device/keypair.bin`, binds an Iroh endpoint, and
joins a deterministic gossip topic for the workspace. Iroh attempts a direct
path and falls back to its encrypted transport relay when necessary.

Invites carry a signed Iroh `EndpointAddr`; no multicast discovery or fixed
TCP port is required. The inviter and invitee authenticate the endpoint id
embedded in the token before room state is accepted.

For a persistent opt-in, enable the user marker unit and start a new desktop
session:

```sh
systemctl --user enable --now shell-peer-sync.service
```

To return to the default-off posture, remove the marker and start a new
session:

```sh
systemctl --user disable --now shell-peer-sync.service
```

## Persistent host

Set `QOS_COLLAB_HOST` to the host's Iroh endpoint id (or the JSON representation
of a complete `EndpointAddr`) to attach the session to a home or hosted server. The shell
authenticates with its device certificate and sends durable collaboration
messages to the host in addition to the direct gossip path. The host pins
configured workspaces and store-and-forwards updates to offline devices.

The currently shippable standalone host is `qos-relay`: it retains the
workspace CRDT, shell/app checkpoints, and pending collaboration updates. It
does **not** itself retain file bytes or thumbnails, and it does not run app,
compute, or remote-control host planes. Full file residency requires the
separate authenticated filesystem/blob host to be configured. The Compose
deployment and its identity prerequisites are documented in
`os/containers/qos-relay/README.md`.

The identity files are created together by `shell --provision-identity`:

```text
~/.config/qos/identity/primary.json
/var/lib/qos/user-identity/pubkey.bin
/var/lib/qos/device/keypair.bin
/var/lib/qos/device/cert.bin
```

The personal record is the only copy of the root private key; the shared
system projection contains only its public key. The personal root certifies
the device key. Shell, filesystem sync, and the workspace-host client all
derive the same Iroh endpoint id from that device key.

## Diagnostics

```bash
journalctl --user -u graphical-session.target | grep -Ei 'iroh|gossip|workspace host'
stat -c '%a %U:%G %n' /var/lib/qos/device/keypair.bin
systemctl --user status qos-identity.service
systemctl --user status shell-peer-sync.service
```

An unreadable or mismatched key is not repaired by generating a replacement;
restore or deliberately re-enrol the device instead.

## Home peer replica

On a workspace member, export the current workspace and its enforced access
policy. Repeat this for each mounted workspace; the exporter appends or
refreshes it in the same bundle automatically:

```sh
fs export-home-peer --cache-dir /var/cache/quick-fs \
  --acl /var/lib/qos/fs-capabilities.json \
  --output home-peer.toml
```

Transfer `home-peer.toml` to the host as `/etc/qos/home-peer.toml` and restart
`qos-fs-replica.service`. The bundle's `workspaces` array is the home peer's
complete set: it runs a durable worker for every entry using the enrolled
device identity, keeps each namespace plus all manifests, chunks, and
thumbnails under its configured cache, and serves them through the bundled
capability policy. `qos-relay status` reports `full_content_replication: true`
only after the atomic aggregate status confirms every configured workspace is
complete.
