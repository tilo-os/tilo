# Security Architecture

> **Scope note.** This is the *workspace-scoped* threat model: the remote-input
> firewall, the per-workspace sandbox and the portal capability model, in more
> detail than anywhere else. It predates the machine-scoped work, so on identity,
> the boot chain and disk encryption it is superseded by
> [docs/security/](../docs/security/README.md).

The security boundary is the **workspace**. Every workspace runs apps inside its own Bubblewrap sandbox with its own user namespace, mount namespace, seccomp-bpf filter, and a per-workspace Wayland bridge socket. Capabilities (filesystem, hardware, network) require explicit grants surfaced as inline prompts; grants persist in the workspace's CRDT and replicate to peers.

Implementation tracked in `docs/superpowers/archive/2026-05-02-per-workspace-sandbox-permissions.md` (sandbox phases 1-6 + portal 4.3 landed). Where this design diverges from earlier "global host" framings, the current code wins.

## Threat model

The scenarios we defend against:

1. **Malicious guest** — a peer joins a workspace and attempts to read files the workspace was never granted, snoop on tabs not in the shared workspace, or inject input into the host's private windows.
2. **Compromised app** — a valid sandboxed app has a vulnerability that a remote attacker exploits to attempt sandbox escape, network exfil, or privilege escalation.
3. **Misbehaving peer** — a connected peer issues capability requests the owner didn't authorise. The portal backend gates everything.
4. **Man-in-the-middle** — an attacker intercepts P2P traffic to inject fake input events or steal file data.

## Identity

- One ed25519 device key per machine at `/var/lib/qos/device/keypair.bin`, auto-created on first boot. Every network and document identity derives from it — no subsystem mints its own. The file is 0640 in a setgid `qos-identity` directory because both the root-run filesystem daemon and the session-run compositor must present that one identity; a key that exists but cannot be read is an error, never a reason to re-key.
- libp2p `PeerId` is the multihash of the protobuf-encoded public key, and the filesystem's iroh `NodeId` is the same public key. Used everywhere a stable peer-identity is needed.
- Identity decisions carry the whole peer id (`PeerOrigin`), never a fold of it: an equality check a digest can be ground against is not an identity check.
- Per-user identity (display name, avatar) is self-asserted for v1; federated identity is a follow-up.

Invite tokens are signed ed25519 blobs scoped to a workspace; see `plan/collaboration.md §7` for the wire format and validation order.

## Sandbox: Bubblewrap profile

Every app launched into a workspace runs through `bwrap`, whose argv is built by `qos-launcher` (`crates/packaging/launcher`) from the package manifest and the capability grants the shell hands it. `SandboxProfile` (`shell/src/sandbox/profile.rs`) is the compositor-side *model* of that sandbox — the bind/env/uid shape and the seccomp + Landlock plumbing points. It is a value type, and nothing spawns an app through it:

```rust
pub struct SandboxProfile {
    pub workspace_id:    WorkspaceId,
    pub binds_ro:        Vec<(PathBuf, PathBuf)>,
    pub binds_rw:        Vec<(PathBuf, PathBuf)>,
    pub env:             HashMap<String, String>,
    pub uid: u32,  pub gid: u32,
    pub seccomp:         Option<PathBuf>,
    pub wayland_socket:  PathBuf,   // per-workspace bridge
    pub dbus_socket:     Option<PathBuf>,
    pub net_isolated:    bool,
}
```

What a sandboxed app is actually presented with. It names none of this; the launcher decides all of it, and the read side is identical for every package:

- Read-only: the system read profile (`launcher::profile`) — loader and C library, fonts, GPU loader manifests, `/sys`, CA bundle, timezone data — plus the package payload at `/var/qos-apps/<pkg-id>` and its runtime bundle. `/etc/resolv.conf` and `/etc/hosts` only when the launch shares the host network.
- Read-write: exactly one directory, `<data home>/qos-rooms/<room>/<pkg-id>` → `/run/qos/app`, with `HOME` and every `XDG_*` pointed inside it. Two rooms are two directories, so an app opened in one room cannot read what it wrote in another. Plus the bridge socket → `/run/user/1000/wayland-0`.
- `env`: `WAYLAND_DISPLAY=wayland-0`, `XDG_RUNTIME_DIR=/run/user/1000`, and `QOS_APP_DIR`, `HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`, `XDG_CONFIG_HOME`, `XDG_CACHE_HOME` all under `/run/qos/app`.
- Private DBus session bus per workspace.
- `unshare(CLONE_NEWNET)` if `net_isolated`. Default: invited peers run isolated; the owner's session runs without isolation unless overridden.

Anything outside that set is a capability the user grants at runtime, which becomes a bind on the app's next launch. `SandboxProfile::apply_grants` mirrors the launcher's rules so the compositor can reason about the same shape; a disagreement between the two is a bug in the mirror.

## Seccomp-bpf blocklist

A per-workspace BPF blob is compiled via the `seccompiler` crate and cached at `/var/cache/qos-fs/<ws_id>/seccomp.bpf`. v1 blocks:

```
ptrace, mount, umount2, pivot_root, chroot,
clone3 (with namespace flags), unshare, setns,
socket(AF_PACKET, …), bpf, keyctl
```

The blocklist is conservative and expands as we learn. It is applied via `bwrap --seccomp <fd>`.

## Per-workspace Wayland bridge socket

The compositor's main wayland socket lives at `$XDG_RUNTIME_DIR/wayland-1` (or whatever the listener registered). For each workspace, the bridge (`shell/src/compositor/bridge.rs`) binds an additional listening socket `$XDG_RUNTIME_DIR/nirios-ws-<ws_id>` and proxies its connections to the main socket.

The bridge:

- **Tags** every accepted client with a `WorkspaceTag` `UserData` on the smithay `Client`. Adoption logic in `tab_binding` reads this tag to route surfaces to the correct workspace.
- **Filters** protocol messages against the workspace's grants. Pass-through globals: `wl_compositor`, `wl_subcompositor`, `wl_shm`, `xdg_shell`. Capability-gated globals: `wl_data_device` (cross-workspace clipboard, blocked v1), `zwlr_screencopy_v1` (always prompts).
- **Routes** the per-workspace seat capabilities so input from the compositor's seat only reaches surfaces tagged for the corresponding workspace.

Surfaces from sandbox A literally cannot adopt into workspace B — the bridge tag mismatches.

## Capability grants

Capabilities are CRDT-persisted (`shell/src/shell/state.rs`):

```rust
pub enum Capability {
    Filesystem   { path, mode: FsMode,          scope: GrantScope },
    Camera       {                              scope: GrantScope },
    Microphone   {                              scope: GrantScope },
    Screencast   {                              scope: GrantScope },
    RemoteDesktop{                              scope: GrantScope },
    Network      { hosts: Option<Vec<String>>,  scope: GrantScope },
}
pub enum FsMode    { Read, ReadWrite }
pub enum GrantScope { Once, Session, Always, Workspace }
```

`Workspace.grants: HashMap<AppId, Vec<Capability>>` persists Session/Always/Workspace-scoped grants. Once-grants live only in the in-flight prompt's resolution and never hit the CRDT.

Replication semantics:

- Grants replicate to peers as part of the room document.
- **Only the owner can grant.** Non-owners see the grant rows in the Permissions tab but the Revoke button is disabled.
- A grant revocation writes the removal to the CRDT; the next workspace start uses the updated profile.
- **Only the owner can grant over the wire either.** A room document's owner and capability list are accepted only from the peer entitled to write them, so a guest cannot project its own cache into the room and let automerge's last-writer-wins decide who administers it. See `plan/collaboration.md §4` — "Write authority on a room's governance" — for the record that decides it, why the invite's signature is what makes it checkable by the receiver, and what it does not yet cover (a relayed or delegated governance change, which needs the op signed rather than the hop authenticated).

### Inline prompt

When an unscoped capability request arrives, a `PermissionPrompt` widget pushes onto `Chrome::pending_grants: VecDeque<(AppId, Capability)>` and renders inline at the top of the active strip (looks like the notification bar). Buttons: **Allow** / **Allow this session** / **Always allow** / **Deny**. "Always allow" is disabled for non-owners.

### Portal D-Bus backend

The compositor implements the `org.freedesktop.impl.portal.*` backend (`shell/src/portal/`) via `zbus`. We expose these impls under the service:

| Interface                               | Methods                                | Capability flow                                   |
|-----------------------------------------|----------------------------------------|---------------------------------------------------|
| `…impl.portal.FileChooser`              | `OpenFile`, `SaveFile`                 | Returns a one-shot file descriptor, not a path    |
| `…impl.portal.ScreenCast`               | `CreateSession`, `SelectSources`, `Start`, `OpenPipeWireRemote` (+ `AvailableSourceTypes`/`AvailableCursorModes`/`version` props) | Session-tracked selection: source types (monitor/window/virtual), cursor modes (hidden/embedded/metadata), `multiple`, and `persist_mode`+`restore_token` so a re-share skips the prompt. `Start` provisions a real PipeWire node per source; `OpenPipeWireRemote` hands back a scoped remote fd |
| `…impl.portal.RemoteDesktop`            | `CreateSession`, `SelectDevices`, `Start`, `NotifyPointer*`/`NotifyKeyboard*` (+ `AvailableDeviceTypes`/`version` props) | `SelectDevices` prompts (`Capability::RemoteDesktop`); `Notify*` is gated on the started session's granted device bitmask, then decoded into a `RemoteAction` and injected into the focused surface via the seat |
| `…impl.portal.Access` (incl. Camera)    | `AccessCamera` flows here              | `/dev/video*` only bound on grant                 |

Per request:

1. Identify the caller's workspace via the bridge tag.
2. Look up the caller's app-id in `Workspace::grants`. If `Always`/`Workspace`-scoped, grant immediately.
3. Else push to `pending_grants`; await user response; cache per `GrantScope` rules.

`xdg-desktop-portal`'s config (`os/usr/share/xdg-desktop-portal/portals/qos.portal`) registers `org.freedesktop.impl.portal.desktop.qos` as the backend for these interfaces. ScreenCast output streams use PipeWire (`shell/src/portal/screencast_pipewire.rs`); the provisioner is a seam (`MockScreenCastProvisioner` for headless/tests, a real libpipewire connection in the production OS build). RemoteDesktop seat injection reuses the same `RemoteInputSink`/seat path as peer-sync remote input.

`tests/portal_test.rs` exercises one round-trip per interface plus prompt-resolution branches. A `PromptResolver::AutoAllow` mode is used in tests so the round-trip doesn't need a human.

## Default-deny enforcement

### Filesystem

- An app names no path. `crates/packaging/launcher` binds a fixed system read profile (`launcher::profile`: loader and C library, fonts, GPU loader manifests, `/sys`, CA bundle, timezone data) plus one read-write directory per room per package at `/run/qos/app` (`launcher::appdir`), and nothing else. The `[sandbox.fs]` manifest block is retired; a package that still declares one is refused at install and at launch.
- `FileChooser` returns an open file descriptor via the portal, not a path. `cap-std` wraps the FD.
- A path outside the profile is reachable only through a capability the user grants, which becomes a bind at the next launch of that app (`launcher::capabilities`). `SandboxProfile::apply_grants` mirrors the same rules compositor-side; the launcher is the enforcement point and a disagreement between the two is a bug in the mirror.

**The mount is the second enforcement point** (`fs/src/acl.rs`). `fs --acl FILE` binds a capability list to the mount, and every `open`/`read`/`write` is answered by `collab_types::decide_capability` — the same function the compositor calls, so a path readable through one and not the other is a bug rather than a policy. Without `--acl` the mount keeps the Unix-veneer-only behaviour, which is the documented opt-out.

Two properties this rests on:

- **The list is re-read while the mount is up** (`fs/src/caps_file.rs`). `/var/workspace` is mounted at boot and lives as long as the session, so a list read only at mount time could not express a grant — or a revocation — made afterwards. A deleted list denies everything (its absence is the producer saying nothing is granted); an unreadable or malformed one keeps the last good list, because the file is rewritten in place and a reader can catch it truncated.
- **The shipped image enforces.** `os/usr/lib/systemd/system/fs.service` passes `--acl /var/lib/qos/fs-capabilities.json`, seeded by `tmpfiles.d` from `/usr/share/qos/fs-capabilities.json`. Be plain about what that default is: it grants the local session read+write on the whole workspace — exactly what an unenforced mount already gave. It narrows nothing on a single-user desktop. What it buys is that the enforcement point is *live*, so a restriction added to that list bites without a remount, and a mount joined from a peer's ticket has somewhere to put a narrower policy. The subject is a principal (`person:local`) rather than a key because the mount's identity is a per-start iroh endpoint key that no file written in advance could name.

### Network

- `unshare(CLONE_NEWNET)` isolates per-workspace.
- Owner workspace defaults to "no isolation"; invited-peer workspaces default to "isolated".

**Per-host egress** (`crates/packaging/launcher/src/netgate.rs`). A grant naming specific hosts does *not* share the network namespace. The namespace stays empty and the app's only route out is an `AF_UNIX` socket bind-mounted from the launcher, which speaks SOCKS5 and enforces the allowlist. A grant for `*` still shares the host network — there is no allowlist to enforce, so there is nothing to broker.

Three properties make that a boundary rather than a convention:

- **An app that ignores the gate is offline, not unfiltered.** An unshared namespace has no route, so there is no fallback path to fall back to. `crates/packaging/launcher/tests/netgate_egress.rs` asserts this against a real sandbox.
- **The allowlist is matched against the host *name*,** taken from the SOCKS request, before that name is resolved. So a rotating CDN works, a DNS record that changes mid-session is just two resolutions, and no address is ever pinned.
- **An address literal never satisfies a hostname entry.** Otherwise an app could resolve an allowed name itself and then ask for the address — and anyone controlling a DNS answer would turn a name-scoped grant into an any-host grant.

DNS is treated as egress. An app with no grant reaches no resolver; an app with an allowlist needs none, because the gate resolves on its behalf after the name has passed the policy. So an app cannot make the gate resolve a name it was not allowed to reach, and cannot signal through query contents because it emits no query.

This needs no privilege and no kernel feature `bwrap` does not already require. Earlier drafts of this document described per-workspace `nftables` rules; that was never implemented, and it would not have worked as described — a rule set is written against addresses and cannot express a hostname, and installing one needs `CAP_NET_ADMIN` the launcher does not have.

Not constrained: **ports** (a `NetHost` scope names a host and carries no port) and **content** (the gate relays TCP and deliberately does not terminate TLS).

**A filtered grant is TCP-only.** The namespace is empty, so the sole route out is the gate socket, and both protocols it serves — SOCKS5 `CONNECT` and HTTP `CONNECT` — carry TCP. SOCKS5 `UDP ASSOCIATE` is refused outright, because a connectionless path carries no name for the policy to match. So an app under a host-scoped grant has no UDP egress at all: QUIC and HTTP/3 do not work, and a client that does not fall back to TCP is offline rather than slow. A grant of `*` shares the host network and is unaffected. This is a real behavioural constraint of choosing a name-filtering broker, not an oversight — there is no way to name a host in a UDP datagram.

**What the permissions panel can express is narrower than what the gate enforces.** The composer's scope picker (`shell/src/shell/workspace_settings.rs`, `GRANT_CHOICES`) is a fixed list with one network entry, "Network access", which mints `NetHost("*")`. So the mechanism accepts a host-scoped grant and enforces it exactly, but the only grant a user can *make from the UI today* is the unrestricted one. Two consequences worth stating plainly:

- An app whose manifest enumerates endpoints is still filtered to them, because a `*` grant narrows to a declared allowlist rather than widening past it. This is the common case and it works.
- An app whose manifest says `net.mode = "host"` is unrestricted once the user clicks the one network choice, and the UI offers no way to say "only `mail.example.com`". Closing this needs the composer to offer the package's declared endpoints as individual choices (the manifest is already parsed into `InstalledApp`), or a hostname text field. Until then, a host-scoped grant is reachable through the capability file and the manifest, not through the panel.

If the gate cannot start, the launch is **refused** rather than downgraded — sharing the network would apply "this host" as "every host". `QOS_LAUNCHER_NET_GATE=off` takes the unfiltered path deliberately and logs it.

### Hardware

- Camera / microphone: `/dev/video*` and the PipeWire socket are unbound by default. A grant binds them.
- Screencast: the captured stream is the requesting app's tab content only — never the full output, never another tab.

## Input firewall (remote input)

When a peer publishes `RemoteInput { peer, surface, event }`:

1. The owner's compositor checks the per-peer `read_only` flag. If set, drop.
2. Verify the target surface is one the peer has been granted view-access to (the surface belongs to a workspace the peer joined).
3. **Region check** — is the event coordinate inside the shared surface's content rect?
4. **Z-order check** — is the surface visible and topmost at that point? (Prevents clickjacking via transparent overlays.)
5. **Keyboard scrubber** — system shortcuts are denied for remote users: `Ctrl+Alt+F*` (TTY switch), the Super key (DE launcher), `Alt+Tab`, `Ctrl+Alt+Delete`. Focus guard: keystrokes only inject if a shared surface has keyboard focus; if the host focuses a private window, the remote keyboard stream mutes instantly.
6. **Rate limit** — max 120 input events/s per remote peer; >10 clicks in 100 ms triggers a temporary block.
7. Inject via `smithay::input::Seat::input`.

## Host overrides

- **Physical priority.** Physical input from the host's seat causes a 500 ms block on all remote input — the user can always grab control.
- **Kill switch (`Super+Esc`).** Drops all P2P connections, revokes all capability grants for the current session, unmounts any peer-owned FUSE bind-mounts, and locks the screen.

## Encryption

- libp2p uses the `noise` transport with ChaCha20-Poly1305. All P2P traffic is encrypted; identities are bound to peer-IDs.
- iroh's FUSE traffic (`fs`) is QUIC + TLS 1.3.
- At-rest encryption for cloud-backed `/var/workspace` is a follow-up.

## Security matrix

| Attack vector              | Defense                                                 | Component                          |
|----------------------------|---------------------------------------------------------|------------------------------------|
| Unauthorised peer connect  | ed25519 invite token + libp2p noise                     | `network/invite.rs`, libp2p        |
| File exfil from sandbox    | bwrap mount namespace + fixed system read profile + one per-room app dir | `packaging/launcher/{profile,appdir}.rs` |
| Hardware exfil (cam/mic)   | Default-deny binds + portal-mediated grant              | `sandbox/profile.rs`, `portal/`    |
| Cross-workspace input leak | Bridge tag + per-workspace seat routing                 | `compositor/bridge.rs`             |
| Clipboard data leak        | `wl_data_device` blocked at bridge v1                   | `compositor/bridge.rs`             |
| Input injection (remote)   | Region + Z-order + scrubber + rate limit                | `compositor/state.rs`              |
| Sandbox escape via syscall | seccomp-bpf blocklist                                   | `sandbox/seccomp.rs`               |
| Network exfil to an ungranted host | Empty netns + name-matched SOCKS5 egress gate   | `packaging/launcher/netgate.rs`    |
| Clickjacking               | Z-order verification before remote input inject         | compositor scene graph             |
| Privilege escalation       | Owner-only grant authority + CRDT-persisted grants       | `shell/state.rs::Workspace.grants` |
| Room takeover via replication | Governance written only by the room's authority, pinned from the invite signature | `shell/state.rs::apply_room_changes` |
| Traffic snoop              | noise / TLS 1.3 end-to-end                              | libp2p, iroh                       |
| Token forgery              | ed25519 signature + canonical payload + expiry          | `network/invite.rs`                |
| Host takeover by peer      | Physical-input priority + kill switch                   | compositor input loop              |

## Where to read first

- `shell/src/sandbox/{mod.rs, profile.rs, spawn.rs, seccomp.rs}` — the bwrap profile + seccomp blob.
- `shell/src/compositor/bridge.rs` — the per-workspace Wayland bridge socket.
- `shell/src/portal/{mod.rs, file_chooser.rs, screencast.rs, screencast_pipewire.rs, remote_desktop.rs, camera.rs}` — D-Bus impls.
- `shell/src/shell/state.rs` — `Capability`, `GrantScope`, `Workspace.grants`.
- `shell/src/network/invite.rs` — invite tokens.
- `docs/superpowers/archive/2026-05-02-per-workspace-sandbox-permissions.md` — phase log.
