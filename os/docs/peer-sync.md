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

## Locate existing Files before initializing a host installation

A Fedora Desktop may have an enrolled home peer but no local Files daemon or
namespace. Do not infer a new empty workspace from this, or from an offline
peer. The saved `home-peers.json` under the host's existing identity root
(`$XDG_DATA_HOME/tilo`, normally `~/.local/share/tilo`) identifies the enrolled
device, relay service and relay room IDs to probe:

```sh
tilo-relay-admin home-peer-files-status \
  --identity-root "$HOME/.local/share/tilo" \
  --device ENROLLED_DEVICE_PUBLIC_KEY --service RELAY_SERVICE_PUBLIC_KEY \
  --workspace-id REGISTERED_RELAY_ROOM_ID
```

This read-only command uses the existing device key, verified certificate and
signed revocation cache. It restricts the query to a room registered to that
exact home peer and person, then uses the ordinary authenticated Iroh content
finder. It prints the public Files namespace UUID and endpoint identifiers;
it does not print or save the returned ticket, refresh the peer registry,
create an identity, grant access, or start a Files mount. A timeout or denied
query leaves the existence of remote data unknown. Probe the saved candidate
rooms before deciding whether explicit new initialization is appropriate.

The relay room ID is a routing key, not a Files namespace UUID. A successful
probe identifies a recovery source; actual namespace recovery must still use
the existing authenticated Files join path and corresponding authority.

Current tooling keeps this registry at `<identity-root>/peers/home-peers.json`.
An older host may still have `<identity-root>/home-peers.json`. To inspect that
existing record without creating or migrating trust, pass
`--registry /path/to/existing/home-peers.json` explicitly. The selected record
must still match this person's verified certificate, exact enrolled device
and service, and saved room. The command neither copies the legacy record nor
changes which registry other services use.

## Fedora primary Files service and explicit first run

The Fedora host payload includes the existing `fs` daemon and
`tilo-relay-admin`, plus the user service `tilo-files-workspace.service`.
It belongs to `tilo-graphical-session.target`; installation alone does not
initialize storage, enroll a device, or grant Files access. The image's system
`fs.service` remains the image path.

The standard host cache is `$XDG_STATE_HOME/tilo/files`, normally
`~/.local/state/tilo/files`. Its saved `namespace.bin` and signed
`workspace-authority.bin` must already exist. Before mounting, the entry point
`fs serve-primary` checks the existing certified person/device, signed
revocations, matching namespace authority, and an explicit read/write Files
path capability. A room
administration grant is insufficient. Failed readiness exits without making
an empty mount. The service suppresses the daemon's ticket-bearing stdout.

For an already prepared primary:

```sh
tilo-relay-admin files-workspace-check \
  --identity-root "$HOME/.local/share/tilo" \
  --cache-dir "$HOME/.local/state/tilo/files"
systemctl --user start tilo-files-workspace.service
```

The host shell points its existing content finder, Files blob socket and
local authority publisher at the same host paths. Its native app launcher
continues to require an actual path grant before binding the Files mount into
an app; discovering a same-person peer is never that grant. This service uses
the default Fedora user-manager XDG paths. For a custom XDG profile, its unit
conditions, cleanup and the user manager's environment must match that profile
through an explicit service override; changing only the shell environment is
insufficient.

The normal Permissions choice remains scoped to `/var/workspace` for Read
and Write; Edit files (Write) already includes Read. It does not require a
host-root `/` grant or Admin. Files evaluates that mount scope against its
namespace-relative root `/`, using the shared ACL
projection for initialization, signed authority, FUSE and peer requests. Deeper
scopes stay subtrees, and restrictions (including mount ancestors), subject
matching and expiry remain enforced. The signed capability list keeps its
original paths. Its accepted authority envelope binds it to the exact Files
workspace UUID; this mapping grants no access to host paths outside the Files
mount. An approval must still explicitly name a NEW empty namespace and its
private cache/bindings when no verified recovery copy is available.

When recovery cannot supply a verified copy and the user has explicitly chosen
a new empty primary namespace, the source controller supports:

```sh
tilo-relay-admin files-workspace-initialize \
  --identity-root "$HOME/.local/share/tilo" \
  --identity-dir "$HOME/.config/tilo/identity" \
  --cache-dir "$HOME/.local/state/tilo/files" \
  --acl "$HOME/.config/tilo/fs-capabilities.json" \
  --confirm-new-workspace --acknowledge-unavailable-recovery
```

This is an explicit operation, not part of package installation or service
startup. It refuses any existing destination, preserves remembered recovery
candidates, requires the person's existing Files capability and opens the
existing protected single-device signing session. It signs that existing
policy without exporting the person's root key or creating credentials. It
writes a real empty Automerge namespace last so partial preparation cannot
pass service readiness. The Files workspace identifier remains the existing
certified device's topic-derived identifier; this does not recover historical
resources or allocate a distinct workspace identity. A threshold identity
still requires its existing signing ceremony.

Saved signed authority can now resume in the daemon without a raw root key
file. Subsequent ordinary permission publication uses the host's existing
protected signing session through `tilo-relay-admin publish-authority --acl`;
concurrent revision conflicts fail and retry rather than overwriting newer
signed policy.

The read-only candidate probe still discards returned tickets. A separate
`tilo-relay-admin files-workspace-join` source path restores an explicitly
selected existing namespace into an absent absolute cache directory. It requires
the existing certified person/device, the exact enrolled host/service/room,
`--files-workspace-id` and `--confirm-existing-files-access`, and both the existing
local Files read/write policy and the peer's current governor-signed Files
authority. Identity enrollment alone does not authorize Files access. This is
a separate product operation; development of this path does not authorize
live discovery, ticket persistence or joining.

The join uses the native authenticated authority and namespace protocols,
retains the actual Automerge history, and checks authority again before saving.
Changed or revoked authority, another workspace, missing credentials and invalid
namespace bytes fail without initializing replacement state. The enrolled
replica endpoint may differ from the originating ticket endpoint. A successful
join saves the native binding and signed authority privately (directory0700,
files0600), with namespace readiness last. It does not fetch file payloads or
establish recovery of historical populated data. An existing destination,
including an empty saved namespace, must be preserved.

This does not provide new sync or cache eviction mechanisms: native CRDT namespaces/resources,
Iroh content fetch and existing cache policy continue to own those behaviors.
Device identities, mountpoints, sockets and rendering remain device-local. The
Settings Storage pane still selects the image storage socket in this slice;
its host accounting/eviction endpoint wiring is pending and is not implied by
creating the local service socket.
