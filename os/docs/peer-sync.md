# Peer sync (Iroh)

The collaborative session starts the shell's Iroh transport. It supports an
ad-hoc direct invite and an optional always-on workspace host at the same time.

Peer sync starts automatically in every normal desktop session. The shell
loads the certified device identity, binds an Iroh endpoint, and joins the
workspace mesh; Iroh attempts a direct path and falls back to its encrypted
transport relay when necessary. Recovery and deliberately offline sessions
can use `--disable-peer-sync`.

Invites carry a signed Iroh `EndpointAddr`; no multicast discovery or fixed
TCP port is required. The inviter and invitee authenticate the endpoint id
embedded in the token before room state is accepted.

The legacy `--enable-peer-sync`, peer-sync desktop entry, and marker unit are
still accepted so upgraded installations keep working, but they are no longer
required.

## Persistent host

Set `TILO_COLLAB_HOST` to the host's Iroh endpoint id (or the JSON representation
of a complete `EndpointAddr`) to attach the session to a home or hosted server. The shell
authenticates with its device certificate and sends durable collaboration
messages to the host in addition to the direct gossip path. The host pins
configured workspaces and store-and-forwards updates to offline devices.

The home-server deployment combines `tilo-relay` with the authenticated
filesystem replica. Pairing it in Settings transfers its certified identity,
workspace bindings, and full-replication configuration; it then retains CRDT
state, app checkpoints, file bytes, and thumbnails around the clock. The
container deployment is documented in `os/containers/tilo-relay/README.md`.

The identity files are created together by `shell --provision-identity`:

```text
~/.config/tilo/identity/primary.json
/var/lib/tilo/user-identity/pubkey.bin
/var/lib/tilo/device-private/keypair.bin
/var/lib/tilo/device/cert.bin
```

The personal record is the only copy of the root private key; the shared
system projection contains only its public key. The personal root certifies
the device key. Shell, filesystem sync, and the workspace-host client all
derive the same Iroh endpoint id from that device key.

## Diagnostics

```bash
journalctl --user -u graphical-session.target | grep -Ei 'iroh|gossip|workspace host'
stat -c '%a %U:%G %n' /var/lib/tilo/device-private/keypair.bin
systemctl --user status shell-peer-sync.service
```

An unreadable or mismatched key is not repaired by generating a replacement;
restore or deliberately re-enrol the device instead.

## Home peer replica

Settings → Devices & Sync → Home server or NAS is the normal setup path. For
headless recovery or diagnostics, the equivalent workspace bundle can be
exported manually:

```sh
fs export-home-peer --cache-dir /var/cache/quick-fs \
  --acl /var/lib/tilo/fs-capabilities.json \
  --output home-peer.toml
```

Transfer `home-peer.toml` to the host as `/etc/tilo/home-peer.toml` and restart
`tilo-fs-replica.service`. The bundle's `workspaces` array is the home peer's
complete set: it runs a durable worker for every entry using the enrolled
device identity, keeps each namespace plus all manifests, chunks, and
thumbnails under its configured cache, and serves them through the bundled
capability policy. `tilo-relay status` reports `full_content_replication: true`
only after the atomic aggregate status confirms every configured workspace is
complete.
