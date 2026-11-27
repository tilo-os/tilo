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
embedded in the token before workspace state is accepted.

## Persistent host

Set `TILO_COLLAB_HOST` to the host's Iroh endpoint id (or the JSON representation
of a complete `EndpointAddr`) to attach the session to a home or hosted server. The shell
authenticates with its device certificate and sends durable collaboration
messages to the host in addition to the direct gossip path. The host pins
configured workspaces and store-and-forwards updates to offline devices.

The home-server deployment runs the device's daemon (`tilo-runtime`) with a server configuration. Pairing it in Settings transfers
its certified identity; it then retains CRDT state and app checkpoints around
the clock. The container deployment is documented in
`os/containers/tilo-server/README.md`.

The identity files are created together by `tilo-session --provision-identity`:

```text
~/.config/tilo/identity/primary.json
/var/lib/tilo/user-identity/pubkey.bin
/var/lib/tilo/device-private/keypair.bin
/var/lib/tilo/device/certs/<person>.bin
```

The personal record is the only copy of the root private key; the shared
system projection contains only its public key. The personal root certifies
the device key. The shell and the workspace-host client both
derive the same Iroh endpoint id from that device key.

## Diagnostics

```bash
journalctl --user -u graphical-session.target | grep -Ei 'iroh|gossip|workspace host'
stat -c '%a %U:%G %n' /var/lib/tilo/device-private/keypair.bin
```

An unreadable or mismatched key is not repaired by generating a replacement;
restore or deliberately re-enrol the device instead.

## Files

A workspace's Files tree is one of its resources. The `fs` daemon gets the tree
from the session's workspace service over its files-tree socket
(`$XDG_RUNTIME_DIR/tilo/services/files-tree.sock`) and keeps a saved copy
under its cache; file bytes come through the same socket from the workspace's
one content store, and so do the keys that open them. Another device asking
this one for a file's bytes gets them only while the workspace's signed policy
lets its member read Files; a member that policy refuses or has revoked gets
none.

The Fedora host payload includes the `fs` daemon and the user service
`tilo-files-workspace.service`. It belongs to
`tilo-graphical-session.target`; installation alone does not enroll a device
or grant Files access. Before mounting, the service checks that this computer
is signed in to a workspace with a current certificate its root has not
revoked:

```sh
systemctl --user start tilo-files-workspace.service
```

With no saved tree and no workspace service to sync one from, `fs` stops and
says so rather than mounting an empty folder.

Cache eviction stays with the existing cache policy.
Device identities, mountpoints, sockets and rendering remain device-local. The
host service's storage socket sits alone in `$XDG_RUNTIME_DIR/tilo/files-storage`,
which the launcher binds into Settings at the image's storage directory.
