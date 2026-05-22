# Security Architecture

The security boundary is the **workspace**. Every workspace runs apps inside its own Bubblewrap sandbox with its own user namespace, mount namespace, seccomp-bpf filter, and a per-workspace Wayland bridge socket. Capabilities (filesystem, hardware, network) require explicit grants surfaced as inline prompts; grants persist in the workspace's CRDT and replicate to peers.

Implementation tracked in `docs/superpowers/plans/2026-05-02-per-workspace-sandbox-permissions.md` (sandbox phases 1-6 + portal 4.3 landed). Where this design diverges from earlier "global host" framings, the current code wins.

## Threat model

The scenarios we defend against:

1. **Malicious guest** — a peer joins a workspace and attempts to read files outside `/work`, snoop on tabs not in the shared workspace, or inject input into the host's private windows.
2. **Compromised app** — a valid sandboxed app has a vulnerability that a remote attacker exploits to attempt sandbox escape, network exfil, or privilege escalation.
3. **Misbehaving peer** — a connected peer issues capability requests the owner didn't authorise. The portal backend gates everything.
4. **Man-in-the-middle** — an attacker intercepts P2P traffic to inject fake input events or steal file data.

## Identity

- Per-host ed25519 keypair at `/var/lib/quick-fs/peer-key`, auto-created on first boot.
- libp2p `PeerId` is the multihash of the protobuf-encoded public key. Used everywhere a stable peer-identity is needed.
- Per-user identity (display name, avatar) is self-asserted for v1; federated identity is a follow-up.

Invite tokens are signed ed25519 blobs scoped to a workspace; see `plan/collaboration.md §7` for the wire format and validation order.

## Sandbox: Bubblewrap profile

Every app launched into a workspace runs through `bwrap`, configured by a `SandboxProfile` (`quick-shell/src/sandbox/profile.rs`):

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

Default workspace profile presents the sandboxed app:

- `binds_ro`: `/usr` → `/usr`. `/etc/resolv.conf` only if the workspace has network.
- `binds_rw`: `/var/workspace/<ws_id>` → `/work`. `/var/cache/quick-fs/<ws_id>` → `/cache`. The bridge socket → `/run/user/1000/wayland-0`.
- `env`: `WAYLAND_DISPLAY=wayland-0`, `XDG_RUNTIME_DIR=/run/user/1000`, `HOME=/work`.
- Empty `/home` (tmpfs).
- Private DBus session bus per workspace.
- `unshare(CLONE_NEWNET)` if `net_isolated`. Default: invited peers run isolated; the owner's session runs without isolation unless overridden.

The profile renders to `bwrap` argv via `into_argv(command)`; `spawn_in_workspace` wraps `std::process::Command::new("bwrap")`.

## Seccomp-bpf blocklist

A per-workspace BPF blob is compiled via the `seccompiler` crate and cached at `/var/cache/quick-fs/<ws_id>/seccomp.bpf`. v1 blocks:

```
ptrace, mount, umount2, pivot_root, chroot,
clone3 (with namespace flags), unshare, setns,
socket(AF_PACKET, …), bpf, keyctl
```

The blocklist is conservative and expands as we learn. It is applied via `bwrap --seccomp <fd>`.

## Per-workspace Wayland bridge socket

The compositor's main wayland socket lives at `$XDG_RUNTIME_DIR/wayland-1` (or whatever the listener registered). For each workspace, the bridge (`quick-shell/src/compositor/bridge.rs`) binds an additional listening socket `$XDG_RUNTIME_DIR/nirios-ws-<ws_id>` and proxies its connections to the main socket.

The bridge:

- **Tags** every accepted client with a `WorkspaceTag` `UserData` on the smithay `Client`. Adoption logic in `tab_binding` reads this tag to route surfaces to the correct workspace.
- **Filters** protocol messages against the workspace's grants. Pass-through globals: `wl_compositor`, `wl_subcompositor`, `wl_shm`, `xdg_shell`. Capability-gated globals: `wl_data_device` (cross-workspace clipboard, blocked v1), `zwlr_screencopy_v1` (always prompts).
- **Routes** the per-workspace seat capabilities so input from the compositor's seat only reaches surfaces tagged for the corresponding workspace.

Surfaces from sandbox A literally cannot adopt into workspace B — the bridge tag mismatches.

## Capability grants

Capabilities are CRDT-persisted (`quick-shell/src/shell/state.rs`):

```rust
pub enum Capability {
    Filesystem { path, mode: FsMode,            scope: GrantScope },
    Camera     {                                scope: GrantScope },
    Microphone {                                scope: GrantScope },
    Screencast {                                scope: GrantScope },
    Network    { hosts: Option<Vec<String>>,    scope: GrantScope },
}
pub enum FsMode    { Read, ReadWrite }
pub enum GrantScope { Once, Session, Always, Workspace }
```

`Workspace.grants: HashMap<AppId, Vec<Capability>>` persists Session/Always/Workspace-scoped grants. Once-grants live only in the in-flight prompt's resolution and never hit the CRDT.

Replication semantics:

- Grants replicate to peers as part of the workspace document.
- **Only the owner can grant.** Non-owners see the grant rows in the Permissions tab but the Revoke button is disabled.
- A grant revocation writes the removal to the CRDT; the next workspace start uses the updated profile.

### Inline prompt

When an unscoped capability request arrives, a `PermissionPrompt` widget pushes onto `Chrome::pending_grants: VecDeque<(AppId, Capability)>` and renders inline at the top of the active strip (looks like the notification bar). Buttons: **Allow** / **Allow this session** / **Always allow** / **Deny**. "Always allow" is disabled for non-owners.

### Portal D-Bus backend

The compositor implements the `org.freedesktop.impl.portal.*` backend (`quick-shell/src/portal/`) via `zbus`. We expose three impls under the service:

| Interface                               | Methods                                | Capability flow                                   |
|-----------------------------------------|----------------------------------------|---------------------------------------------------|
| `…impl.portal.FileChooser`              | `OpenFile`, `SaveFile`                 | Returns a one-shot file descriptor, not a path    |
| `…impl.portal.ScreenCast`               | `CreateSession`, `SelectSources`, `Start` | Bound to the requesting app's tab only — never the full output |
| `…impl.portal.Access` (incl. Camera)    | `AccessCamera` flows here              | `/dev/video*` only bound on grant                 |

Per request:

1. Identify the caller's workspace via the bridge tag.
2. Look up the caller's app-id in `Workspace::grants`. If `Always`/`Workspace`-scoped, grant immediately.
3. Else push to `pending_grants`; await user response; cache per `GrantScope` rules.

`xdg-desktop-portal`'s config registers `nirios` as the backend for these interfaces. ScreenCast output streams use PipeWire (`quick-shell/src/portal/screencast_pipewire.rs`).

`tests/portal_test.rs` exercises one round-trip per interface plus prompt-resolution branches. A `PromptResolver::AutoAllow` mode is used in tests so the round-trip doesn't need a human.

## Default-deny enforcement

### Filesystem

- The bind-mount story already enforces FS isolation — apps see only `/work`/`/cache`.
- `FileChooser` returns an open file descriptor via the portal, not a path. `cap-std` wraps the FD.
- `Always`-scoped FS grants extend `SandboxProfile::binds_{ro,rw}` on next workspace start.

### Network

- `unshare(CLONE_NEWNET)` isolates per-workspace.
- A `Network::Always { hosts: Some(["github.com"]) }` grant wires per-workspace `nftables` rules allowing only those hosts. Network isolation is optional via a helper that's a no-op if `nftables` is missing.
- Owner workspace defaults to "no isolation"; invited-peer workspaces default to "isolated".

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
- iroh's FUSE traffic (`quick-fs-fuse`) is QUIC + TLS 1.3.
- At-rest encryption for cloud-backed `/var/workspace` is a follow-up.

## Security matrix

| Attack vector              | Defense                                                 | Component                          |
|----------------------------|---------------------------------------------------------|------------------------------------|
| Unauthorised peer connect  | ed25519 invite token + libp2p noise                     | `network/invite.rs`, libp2p        |
| File exfil from sandbox    | bwrap mount namespace + bind-mount whitelist            | `sandbox/profile.rs`               |
| Hardware exfil (cam/mic)   | Default-deny binds + portal-mediated grant              | `sandbox/profile.rs`, `portal/`    |
| Cross-workspace input leak | Bridge tag + per-workspace seat routing                 | `compositor/bridge.rs`             |
| Clipboard data leak        | `wl_data_device` blocked at bridge v1                   | `compositor/bridge.rs`             |
| Input injection (remote)   | Region + Z-order + scrubber + rate limit                | `compositor/state.rs`              |
| Sandbox escape via syscall | seccomp-bpf blocklist                                   | `sandbox/seccomp.rs`               |
| Clickjacking               | Z-order verification before remote input inject         | compositor scene graph             |
| Privilege escalation       | Owner-only grant authority + CRDT-persisted grants       | `shell/state.rs::Workspace.grants` |
| Traffic snoop              | noise / TLS 1.3 end-to-end                              | libp2p, iroh                       |
| Token forgery              | ed25519 signature + canonical payload + expiry          | `network/invite.rs`                |
| Host takeover by peer      | Physical-input priority + kill switch                   | compositor input loop              |

## Where to read first

- `quick-shell/src/sandbox/{mod.rs, profile.rs, spawn.rs, seccomp.rs}` — the bwrap profile + seccomp blob.
- `quick-shell/src/compositor/bridge.rs` — the per-workspace Wayland bridge socket.
- `quick-shell/src/portal/{mod.rs, file_chooser.rs, screencast.rs, camera.rs}` — D-Bus impls.
- `quick-shell/src/shell/state.rs` — `Capability`, `GrantScope`, `Workspace.grants`.
- `quick-shell/src/network/invite.rs` — invite tokens.
- `docs/superpowers/plans/2026-05-02-per-workspace-sandbox-permissions.md` — phase log.
