# Security Architecture

> **Scope note.** This is the *workspace-scoped* threat model: the remote-input
> firewall, the per-workspace sandbox and the portal capability model, in more
> detail than anywhere else. It predates the machine-scoped work, so on identity,
> the boot chain and disk encryption it is superseded by
> [docs/security/](../security/README.md).

The security boundary is the **workspace**. Every workspace runs apps inside its own Bubblewrap sandbox with its own user namespace, mount namespace, seccomp-bpf filter, and a per-workspace Wayland bridge socket. Capabilities (filesystem, hardware, network) require explicit grants surfaced as inline prompts; grants persist in the workspace's CRDT and replicate to peers.

## Threat model

The scenarios we defend against:

1. **Malicious guest** — a peer joins a workspace and attempts to read files the workspace was never granted, snoop on tabs not in the shared workspace, or inject input into the host's private windows.
2. **Compromised app** — a valid sandboxed app has a vulnerability that a remote attacker exploits to attempt sandbox escape, network exfil, or privilege escalation.
3. **Misbehaving peer** — a connected peer issues capability requests the owner didn't authorise. The portal backend gates everything.
4. **Man-in-the-middle** — an attacker intercepts P2P traffic to inject fake input events or steal file data.

## Identity

- One Ed25519 device key per machine at `/var/lib/tilo/device-private/keypair.bin`, provisioned by root before login. Every network and document identity derives from it — no subsystem mints its own. The file is 0640 in a root-owned, non-writable `tilo-identity` directory: the root filesystem daemon and session compositor can read it, but session processes cannot delete or replace it. A key that exists but cannot be read is an error, never a reason to re-key.
- The certified device public key is the Iroh `EndpointId` used by the shell, filesystem, and workspace-host client. It is the stable peer identity everywhere.
- Identity decisions carry the whole peer id (`PeerOrigin`), never a fold of it: an equality check a digest can be ground against is not an identity check.
- The personal signing identity is the user root. It certifies each device key; workspace hosts accept only device certificates chaining to the configured user root and permitted by the workspace membership/revocation state.

Invite tokens are signed ed25519 blobs scoped to a workspace; see `docs/architecture/collaboration.md §7` for the wire format and validation order.

## Sandbox: Bubblewrap profile

Every app launched into a workspace runs through `bwrap`, whose argv is built by `tilo-launcher` (`crates/packaging/launcher`) from the package manifest and the capability grants the shell hands it. The compositor never builds a sandbox: it execs `tilo-launcher` (`shell::shell::launcher`), which is the single enforcement point. Inside the new mount namespace the launcher re-execs itself as `__sandbox-exec` to install Landlock before the app takes over. Landlock ruleset construction and seccomp compilation live in `crates/system/sandbox-core`, a leaf crate the launcher and the `ush` runtime share, so the launcher still depends on nothing in the compositor.

A launch is refused unless the complete Landlock policy is enforced, with one exception. A hosting provider's single-tenant VM or sandbox can be the security boundary itself and offer no Landlock (gVisor implements none; stock Firecracker guest kernels leave it out). An image built for such a machine declares it in `/etc/tilo/provider-sandbox.toml` (`provider_sandboxed = true`, root-owned, not group- or world-writable); the environment cannot. There, apps whose payload lies in the root-owned system store (`/usr/lib/tilo-apps`) run with Landlock not or only partially enforced, which the sandbox shim logs; bubblewrap, seccomp and the network namespace still apply, and a ruleset that fails to build still refuses. Apps a user installs keep the full requirement (`launcher::provider_sandbox`).

What a sandboxed app is actually presented with. It names none of this; the launcher decides all of it, and the read side is identical for every package:

- Read-only: the system read profile (`launcher::profile`) — loader and C library, fonts, GPU loader manifests, `/sys`, CA bundle, timezone data — plus the package payload at `/var/tilo-apps/<pkg-id>` and its runtime bundle. `/etc/resolv.conf` and `/etc/hosts` only when the launch shares the host network.
- Read-write: exactly one directory, `<data home>/tilo-rooms/<room>/<pkg-id>` → `/run/tilo/app`, with `HOME` and every `XDG_*` pointed inside it. Two rooms are two directories, so an app opened in one room cannot read what it wrote in another. Plus the bridge socket → `/run/user/1000/wayland-0`.
- `env`: `WAYLAND_DISPLAY=wayland-0`, `XDG_RUNTIME_DIR=/run/user/1000`, and `TILO_APP_DIR`, `HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`, `XDG_CONFIG_HOME`, `XDG_CACHE_HOME` all under `/run/tilo/app`.
- Private DBus session bus per workspace.
- `unshare(CLONE_NEWNET)` if `net_isolated`. Default: invited peers run isolated; the owner's session runs without isolation unless overridden.

Anything outside that set is a capability the user grants at runtime, which becomes a bind on the app's next launch (`launcher::capabilities`).

## Seccomp-bpf blocklist

`tilo-launcher` compiles the filter for every launch with the `seccompiler` crate (`sandbox_core::seccomp`) and hands it to `bwrap --seccomp <fd>`. It is a default-allow deny-list answering `EPERM`, derived from Flatpak's profile:

```
ptrace, mount, umount2, pivot_root, chroot, unshare, setns,
clone (with any namespace flag), socket(AF_PACKET, …), bpf, keyctl,
kexec_load, {init,finit,delete}_module, reboot, swapon, swapoff,
userfaultfd, perf_event_open, open_by_handle_at, process_vm_{readv,writev},
pidfd_getfd, fanotify_init, fsopen, fspick, fsconfig, fsmount, move_mount,
mount_setattr, quotactl, acct
```

The default profile, `tilo-default`, also refuses any `mmap`/`mprotect`/`pkey_mprotect` asking for writable and executable memory at once. `tilo-jit` drops only that rule, and a manifest's request for it is honoured only for packages the image entitles (`/usr/lib/tilo/jit-entitled.conf`; `launcher::seccomp::resolve_profile`). `tilo-web-engine` is `tilo-jit` for a web engine that confines its own renderers: it also permits `clone`/`unshare` with `CLONE_NEWUSER`, `CLONE_NEWPID` and `CLONE_NEWNET` and `chroot`, the parts of Chromium's renderer sandbox, and still refuses `setns`, `mount`, `pivot_root` and every other namespace kind. It is honoured only for packages in `/usr/lib/tilo/web-engine-entitled.conf` (built in: the browser); a package entitled to the JIT but not to this gets `tilo-jit`. The compiled programs are pinned byte for byte by the launcher's tests, so any change to the filter is reviewed as a policy change.

## Per-workspace Wayland bridge socket

The compositor's main wayland socket lives at `$XDG_RUNTIME_DIR/wayland-1` (or whatever the listener registered). For each workspace, the bridge (`crates/session/shell/src/compositor/bridge.rs`) binds an additional listening socket `$XDG_RUNTIME_DIR/tilo-ws-<ws_id>` and proxies its connections to the main socket.

The bridge:

- **Tags** every accepted client with a `WorkspaceTag` `UserData` on the smithay `Client`. Adoption logic in `tab_binding` reads this tag to route surfaces to the correct workspace.
- **Filters** protocol messages against the workspace's grants. Pass-through globals: `wl_compositor`, `wl_subcompositor`, `wl_shm`, `xdg_shell`. Capability-gated globals: `wl_data_device` (cross-workspace clipboard, blocked v1), `zwlr_screencopy_v1` (always prompts).
- **Routes** the per-workspace seat capabilities so input from the compositor's seat only reaches surfaces tagged for the corresponding workspace.

Surfaces from sandbox A literally cannot adopt into workspace B — the bridge tag mismatches.

## Capability grants

Capabilities are CRDT-persisted (`crates/session/shell/src/shell/state.rs`):

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
- **Only the owner can grant over the wire either.** A room document's owner and capability list are accepted only from the peer entitled to write them, so a guest cannot project its own cache into the room and let automerge's last-writer-wins decide who administers it. See `docs/architecture/collaboration.md §4` — "Write authority on a room's governance" — for the record that decides it, why the invite's signature is what makes it checkable by the receiver, and what it does not yet cover (a relayed or delegated governance change, which needs the op signed rather than the hop authenticated).

### Inline prompt

When an unscoped capability request arrives, a `PermissionPrompt` widget pushes onto `Chrome::pending_grants: VecDeque<(AppId, Capability)>` and renders inline at the top of the active strip (looks like the notification bar). Buttons: **Allow** / **Allow this session** / **Always allow** / **Deny**. "Always allow" is disabled for non-owners.

### Portal D-Bus backend

The compositor implements the `org.freedesktop.impl.portal.*` backend (`crates/session/shell/src/portal/`) via `zbus`. We expose these impls under the service:

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

Step 3 needs something to empty that queue. The compositor drains `PromptQueueHandle` onto `ChromeState::pending_grants` every frame (`ShellOverlayHost::drain_portal_prompts`), and each entry carries the portal's reply channel, so the user's click resolves the D-Bus method that is parked on it. An entry dropped without an answer reads as deny — the failure mode is a refusal, not a hang.

**The portal is served by the compositor**, in-process, in both native and hosted mode (`compositor::native::start_portal_backend`). It is not a separate daemon: the questions the portal answers are all about the live session — which workspace the caller is in, what it has been granted, what the person here decides now — and all of them live in the `WorkspaceTree` the compositor owns. The former `shell --portal-server-only` unit built its context on an empty tree and had no output to draw a dialog on, so it could answer none of them; it is removed, and D-Bus permits one owner per name, so leaving it in place would have been a race whose loser served nobody while looking healthy. `ServerHandle::owns_name` reports whether the name was actually acquired, and the compositor logs at error level when it was not.

`xdg-desktop-portal`'s config (`os/usr/share/xdg-desktop-portal/portals/tilo.portal`) registers `org.freedesktop.impl.portal.desktop.tilo` as the backend for these interfaces. ScreenCast output streams use PipeWire (`crates/session/shell/src/portal/screencast_pipewire.rs`); the provisioner is a seam (`MockScreenCastProvisioner` for headless/tests, a real libpipewire connection in the production OS build). RemoteDesktop seat injection reuses the same `RemoteInputSink`/seat path as peer-sync remote input.

`tests/portal_test.rs` exercises one round-trip per interface plus prompt-resolution branches. A `PromptResolver::AutoAllow` mode is used in tests so the round-trip doesn't need a human.

## Default-deny enforcement

### Filesystem

- An app names no path. `crates/packaging/launcher` binds a fixed system read profile (`launcher::profile`: loader and C library, fonts, GPU loader manifests, `/sys`, CA bundle, timezone data) plus one read-write directory per room per package at `/run/tilo/app` (`launcher::appdir`), and nothing else. The `[sandbox.fs]` manifest block is retired; a package that still declares one is refused at install and at launch.
- `FileChooser` returns `file://` URIs because `org.freedesktop.impl.portal.FileChooser` answers `OpenFile`/`SaveFile`/`SaveFiles` with a `uris` key rather than file descriptors.
- **The dialog is the prompt.** Choosing a file in the compositor-owned dialog is the act of authorisation, and the grant names exactly the path chosen: read-only for `OpenFile`, read-write for `SaveFile`, session-scoped. A separate prompt must not grant a broad path before the user chooses one.
- A path outside the profile is reachable only through a capability the user grants, which becomes a bind at the next launch of that app (`launcher::capabilities`, the enforcement point).

  **Next launch is the operative phrase.** A confined app's mount namespace is fixed when `bwrap` creates it, so a grant made mid-session cannot add anything to the namespace the app is already living in. `crates/session/shell/tests/portal_grant_reaches_sandbox.rs` asserts the grant→bind chain end to end; what it asserts is the *next* launch. For a running app the portal additionally publishes the pick through `shell::portal::doc_store`, which is where the three candidate mechanisms — a launch-time grant, a live re-bind, and a document store — are compared and one is chosen. The shipped store is `HostPathStore`, which is correct for callers sharing the compositor's namespace and reports `reaches_confined_apps() == false` rather than pretending otherwise; the compositor logs that at startup.

**The mount is the second enforcement point** (`crates/system/fs/src/acl.rs`). `fs --acl FILE` binds a capability list to the mount, and every `open`/`read`/`write` is answered by `collab_types::decide_capability` — the same function the compositor calls, so a path readable through one and not the other is a bug rather than a policy. Without `--acl` the mount keeps the Unix-veneer-only behaviour, which is the documented opt-out.

Two properties this rests on:

- **The list is re-read while the mount is up** (`crates/system/fs/src/caps_file.rs`). `/var/workspace` is mounted at boot and lives as long as the session, so a list read only at mount time could not express a grant — or a revocation — made afterwards. A deleted list denies everything (its absence is the producer saying nothing is granted); an unreadable or malformed one keeps the last good list, because the file is rewritten in place and a reader can catch it truncated.
- **The shipped image enforces.** `os/usr/lib/systemd/system/fs.service` passes `--acl /var/lib/tilo/fs-capabilities.json`, seeded by `tmpfiles.d` from `/usr/share/tilo/fs-capabilities.json`. Be plain about what that default is: it grants the local session read+write on the whole workspace — exactly what an unenforced mount already gave. It narrows nothing on a single-user desktop. What it buys is that the enforcement point is *live*, so a restriction added to that list bites without a remount, and a mount joined from a peer's ticket has somewhere to put a narrower policy. The shipped default uses a human-readable principal (`person:local`); the mount itself presents the same durable, certified device key as the shell, so remote policies may name a stable cryptographic subject.

### Network

- `unshare(CLONE_NEWNET)` isolates per-workspace.
- Owner workspace defaults to "no isolation"; invited-peer workspaces default to "isolated".

**Per-host egress** (`crates/packaging/launcher/src/netgate.rs`). A grant naming specific hosts does *not* share the network namespace. The namespace stays empty and the app's only route out is an `AF_UNIX` socket bind-mounted from the launcher, which speaks SOCKS5 and enforces the allowlist. A grant for `*` still shares the host network — there is no allowlist to enforce, so there is nothing to broker.

Three properties make that a boundary rather than a convention:

- **An app that ignores the gate is offline, not unfiltered.** An unshared namespace has no route, so there is no fallback path to fall back to. `crates/packaging/launcher/tests/netgate_egress.rs` asserts this against a real sandbox.
- **The allowlist is matched against the host *name*,** taken from the SOCKS request, before that name is resolved. So a rotating CDN works, a DNS record that changes mid-session is just two resolutions, and no address is ever pinned.
- **An address literal never satisfies a hostname entry.** Otherwise an app could resolve an allowed name itself and then ask for the address — and anyone controlling a DNS answer would turn a name-scoped grant into an any-host grant.

DNS is treated as egress. An app with no grant reaches no resolver; an app with an allowlist needs none, because the gate resolves on its behalf after the name has passed the policy. So an app cannot make the gate resolve a name it was not allowed to reach, and cannot signal through query contents because it emits no query.

This needs no privilege and no kernel feature `bwrap` does not already require. Address-based `nftables` rules would not preserve hostname-scoped policy and would require `CAP_NET_ADMIN`, which the launcher does not have.

Not constrained: **ports** (a `NetHost` scope names a host and carries no port) and **content** (the gate relays TCP and deliberately does not terminate TLS).

**A filtered grant is TCP-only.** The namespace is empty, so the sole route out is the gate socket, and both protocols it serves — SOCKS5 `CONNECT` and HTTP `CONNECT` — carry TCP. SOCKS5 `UDP ASSOCIATE` is refused outright, because a connectionless path carries no name for the policy to match. So an app under a host-scoped grant has no UDP egress at all: QUIC and HTTP/3 do not work, and a client that does not fall back to TCP is offline rather than slow. A grant of `*` shares the host network and is unaffected. This is a real behavioural constraint of choosing a name-filtering broker, not an oversight — there is no way to name a host in a UDP datagram.

**What the permissions panel can express is narrower than what the gate enforces.** The composer's scope picker (`crates/session/desktop/src/workspace_settings.rs`, `GRANT_CHOICES`) is a fixed list with one network entry, "Network access", which mints `NetHost("*")`. So the mechanism accepts a host-scoped grant and enforces it exactly, but the only grant a user can *make from the UI today* is the unrestricted one. Two consequences worth stating plainly:

- An app whose manifest enumerates endpoints is still filtered to them, because a `*` grant narrows to a declared allowlist rather than widening past it. This is the common case and it works.
- An app whose manifest says `net.mode = "host"` is unrestricted once the user clicks the one network choice, and the UI offers no way to say "only `mail.example.com`". Closing this needs the composer to offer the package's declared endpoints as individual choices (the manifest is already parsed into `InstalledApp`), or a hostname text field. Until then, a host-scoped grant is reachable through the capability file and the manifest, not through the panel.

If the gate cannot start, the launch is **refused** rather than downgraded — sharing the network would apply "this host" as "every host". `TILO_LAUNCHER_NET_GATE=off` takes the unfiltered path deliberately and logs it — in a developer build only. Like the launcher's other trust and policy overrides (system app store, runtime bundle root, trusted-publisher list, JIT and untrusted-parser policy files, D-Bus proxy binary; see `crates/packaging/launcher/src/dev_env.rs`), a release build ignores it with a one-time warning unless built with `--features dev-overrides`, and a release compositor strips all of them from the launcher's environment.

### Hardware

- Camera / microphone: `/dev/video*` and the PipeWire socket are unbound by default. A grant binds them.
- Screencast: the captured stream is the requesting app's tab content only — never the full output, never another tab.

## Input firewall (remote input)

When a peer publishes `RemoteInput { peer, surface, event }`, the owner's compositor runs it through `State::review_remote_input` (`crates/session/shell/src/compositor/state.rs`) — the single door between the network and the seat. The order below is the order that function reads in:

0. **Host overrides** — the kill-switch latch, the session lock, then physical-input priority. All three are absolute and none looks at the event, so a flooding peer cannot delay any of them. The lock is checked here rather than falling out of the z-order test because the lock surface is not part of the tiled tree: the tiles under it stay laid out and stay topmost as far as the tree knows.
1. The per-peer `read_only` flag. If set, drop.
2. Surface ownership — there must be a share record for this `(peer, surface)` pair. A peer inventing a surface id it was never given is refused here.
3. **Region check** — is the event coordinate inside the shared surface's content rect? A peer sees only the surface it was shared, so it sends *surface-local* coordinates; an accepted one is translated into the owner's output space by adding the tile origin, which is what confines a guest's pointer to the rectangle it was actually given.
4. **Z-order check** — is the surface visible and topmost at that point? (Prevents clickjacking via transparent overlays.) The hit test runs against the live tile list the compositor publishes each tick from `client_input::foreign_tiles` — the same enumeration `foreign_tile_at` routes *local* clicks with, so the two cannot drift into disagreeing about what is on top.
5. **Keyboard scrubber** — system shortcuts are denied for remote users. The denylist is not a second hand-written table: it is `client_input::shell_owns_chord`, the one the shell already uses to decide whether the *local* user's keystroke belongs to the compositor rather than to the focused client, plus the chords the shell never classifies because something below it takes them first — `Ctrl+Alt+F*` (kernel TTY switch) and `Ctrl+Alt+Delete`. The Super key is refused on its own, and `Alt+Tab` is named explicitly. Modifier state is reconstructed from the peer's own keycode stream rather than trusted from the wire's `modifiers` field, which the peer writes and can zero. Focus guard: keystrokes only inject if the shared surface holds the seat's keyboard focus; if the host focuses a private window or shell chrome, the remote keyboard stream mutes instantly.
6. **Rate limit** — max 120 input events/s per remote peer; >10 clicks in 100 ms trips a one-second block on everything from that peer. Every event a peer offers is metered, including the ones another check refuses, so a flood of junk cannot buy a fresh budget.
7. Inject into the compositor's `smithay::input::Seat`.

Steps 3-6 live in `crates/session/shell/src/network/input_firewall.rs`. Every one of them is **deny-biased**: a check that cannot answer refuses. A compositor that has published no screen refuses all remote input rather than passing it, and a button that arrives before any motion has established where the peer's pointer is has no coordinate to check and is refused.

## Host overrides

- **Session lock.** While the lock surface is up, every remote event is refused. A locked machine has no driver.
- **Physical priority.** Physical input from the host's seat causes a 500 ms block on all remote input — the user can always grab control. Stamped at the backend event funnel in `compositor/native.rs`, so branches that never reach the client router (the lock screen, the VT chord, the power key) still count as the host taking over.
- **Kill switch (`Super+Esc`).** Stops every peer's input at once: the firewall
  latches, held remote keys are released, peer-seat attribution is dropped, every
  active share session is unregistered (terminating its capture stream), and the
  share registry is cleared. Recognised at the same funnel, ahead of the client
  router and ahead of the shell's own `Escape` handling — a panic key an overlay
  or focused client can swallow is not a panic key — and confirmed with a
  persistent notification that bypasses Do-Not-Disturb.

  The broader machine-wide emergency teardown is still incomplete: the
  underlying P2P transport is not disconnected, durable session capability
  grants are not revoked, peer-owned FUSE bind-mounts are not unmounted, and the
  screen is not locked. There is deliberately no UI affordance for clearing the
  latch; restoring a guest's write access requires starting a new share rather
  than silently reviving the stopped session.

## Encryption

- Iroh provides encrypted QUIC transport, authenticated endpoint ids, NAT traversal, and encrypted relay fallback. Workspace hosts add mutual device-certificate authentication.
- iroh's FUSE traffic (`fs`) is QUIC + TLS 1.3.
- At-rest encryption for cloud-backed `/var/workspace` is a follow-up.

## Security matrix

| Attack vector              | Defense                                                 | Component                          |
|----------------------------|---------------------------------------------------------|------------------------------------|
| Unauthorised peer connect  | Ed25519 invite token + authenticated Iroh endpoint      | `network/invite.rs`, Iroh          |
| File exfil from sandbox    | bwrap mount namespace + fixed system read profile + one per-room app dir | `packaging/launcher/{profile,appdir}.rs` |
| Hardware exfil (cam/mic)   | Default-deny binds + portal-mediated grant              | `packaging/launcher/capabilities.rs`, `portal/` |
| Cross-workspace input leak | Bridge tag + per-workspace seat routing                 | `compositor/bridge.rs`             |
| Clipboard data leak        | `wl_data_device` blocked at bridge v1                   | `compositor/bridge.rs`             |
| Input injection (remote)   | Region + Z-order + scrubber + rate limit                | `network/input_firewall.rs`, driven from `compositor/state.rs::review_remote_input` |
| Sandbox escape via syscall | seccomp-bpf blocklist + Landlock                        | `system/sandbox-core`, `packaging/launcher/landlock_shim.rs` |
| Network exfil to an ungranted host | Empty netns + name-matched SOCKS5 egress gate   | `packaging/launcher/netgate.rs`    |
| Clickjacking               | Z-order verification before remote input inject         | `network/input_firewall.rs`, over the tile order `compositor/client_input.rs` publishes |
| Privilege escalation       | Owner-only grant authority + CRDT-persisted grants       | `crates/session/shell/src/shell/state.rs::Workspace.grants` |
| Room takeover via replication | Governance written only by the room's authority, pinned from the invite signature | `crates/session/shell/src/shell/state.rs::apply_room_changes` |
| Traffic snoop              | QUIC/TLS end-to-end                                     | Iroh                               |
| Token forgery              | ed25519 signature + canonical payload + expiry          | `network/invite.rs`                |
| Host takeover by peer      | Physical-input priority + `Super+Esc` kill switch        | `network/input_firewall.rs`, `compositor/native.rs` |

## Where to read first

- `crates/packaging/launcher/src/{sandbox.rs, run.rs, capabilities.rs, landlock_shim.rs, seccomp.rs}` — the bwrap argv, grants, Landlock shim and seccomp profile choice.
- `crates/system/sandbox-core/src/{landlock.rs, seccomp.rs}` — Landlock ruleset construction and seccomp compilation.
- `crates/session/shell/src/compositor/bridge.rs` — the per-workspace Wayland bridge socket.
- `crates/session/shell/src/portal/{mod.rs, file_chooser.rs, screencast.rs, screencast_pipewire.rs, remote_desktop.rs, camera.rs}` — D-Bus impls.
- `crates/session/shell/src/shell/state.rs` — `Capability`, `GrantScope`, `Workspace.grants`.
- `crates/session/shell/src/network/invite.rs` — invite tokens.
- `crates/session/shell/src/network/input_firewall.rs` — the remote-input checks, with each denial proven through `State::review_remote_input` in its `production_path` tests.
